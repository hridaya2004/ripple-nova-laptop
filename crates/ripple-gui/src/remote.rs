//! The running window's control socket. The window is a root process that can hide in the
//! background (no window at all), so whoever lives in the user's session (the tray icon, the menu
//! launcher) needs a way to ask it to show its window or quit. Two fixed verbs, over a unix socket
//! that only that user (and root) can reach.

use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::fs::{chown, MetadataExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::Path;
use std::sync::mpsc::{channel, Receiver};
use std::time::Duration;

pub const SOCKET: &str = "/run/ripple-nova-gui.sock";

#[derive(Debug, PartialEq, Eq)]
pub enum Cmd {
    Show,
    Quit,
}

fn parse(line: &str) -> Option<Cmd> {
    match line.trim() {
        "show" => Some(Cmd::Show),
        "quit" => Some(Cmd::Quit),
        _ => None,
    }
}

/// Ask the running window (`show` or `quit`). Err = nothing is listening.
pub fn ask(verb: &str) -> std::io::Result<()> {
    ask_at(Path::new(SOCKET), verb)
}

fn ask_at(path: &Path, verb: &str) -> std::io::Result<()> {
    let mut s = UnixStream::connect(path)?;
    s.write_all(verb.as_bytes())?;
    s.write_all(b"\n")
}

/// Start listening (after the instance lock is held, so a stale socket file is ours to remove). The
/// socket belongs to the owner of `$XDG_RUNTIME_DIR` (the logged-in user) with mode 0600; without
/// one only root can reach it.
pub fn listen() -> std::io::Result<Receiver<Cmd>> {
    let owner = std::env::var_os("XDG_RUNTIME_DIR")
        .and_then(|d| std::fs::metadata(d).ok())
        .map(|m| (m.uid(), m.gid()))
        .filter(|(uid, _)| *uid != 0);
    listen_at(Path::new(SOCKET), owner)
}

fn listen_at(path: &Path, owner: Option<(u32, u32)>) -> std::io::Result<Receiver<Cmd>> {
    let _ = std::fs::remove_file(path);
    let listener = UnixListener::bind(path)?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    if let Some((uid, gid)) = owner {
        chown(path, Some(uid), Some(gid))?;
    }
    let (tx, rx) = channel();
    std::thread::spawn(move || {
        for conn in listener.incoming().flatten() {
            let _ = conn.set_read_timeout(Some(Duration::from_secs(1)));
            let mut line = String::new();
            // a client sends one short line; never read more than that
            if BufReader::new(conn.take(32)).read_line(&mut line).is_ok() {
                if let Some(cmd) = parse(&line) {
                    if tx.send(cmd).is_err() {
                        break; // the app is gone
                    }
                }
            }
        }
    });
    Ok(rx)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn two_verbs_and_nothing_else() {
        assert_eq!(parse("show\n"), Some(Cmd::Show));
        assert_eq!(parse("quit"), Some(Cmd::Quit));
        assert_eq!(parse("shutdown"), None);
        assert_eq!(parse(""), None);
    }

    #[test]
    fn the_socket_delivers_commands_and_ignores_junk() {
        let path =
            std::env::temp_dir().join(format!("ripple-remote-test-{}.sock", std::process::id()));
        let rx = listen_at(&path, None).unwrap();
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        ask_at(&path, "bogus").unwrap();
        ask_at(&path, "show").unwrap();
        assert_eq!(rx.recv_timeout(Duration::from_secs(2)).unwrap(), Cmd::Show);
        ask_at(&path, "quit").unwrap();
        assert_eq!(rx.recv_timeout(Duration::from_secs(2)).unwrap(), Cmd::Quit);
        // a stale socket from a crashed run is replaced
        drop(rx);
        let _rx2 = listen_at(&path, None).unwrap();
        assert!(ask_at(&path, "show").is_ok());
        let _ = std::fs::remove_file(&path);
        assert!(ask_at(&path, "show").is_err(), "nothing listening");
    }
}
