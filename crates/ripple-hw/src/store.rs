//! Files that several processes edit (the window, the CLI, the daemon, the tray's root helper): every
//! edit is one locked read-modify-write, and the file is replaced atomically, so a crash never leaves
//! half a file and two editors never overwrite each other's fields.

use std::io::Write;
use std::path::{Path, PathBuf};

/// The lock file of `path`: next to it (`config.json` -> `config.lock`). Not the file itself, because
/// a rename replaces that one's inode and the lock would be on the old one.
fn lock_path(path: &Path) -> PathBuf {
    path.with_extension("lock")
}

/// Run `f` while holding the exclusive lock of `path`; the directory is created when missing.
pub fn locked<R>(path: &Path, f: impl FnOnce() -> std::io::Result<R>) -> std::io::Result<R> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(lock_path(path))?;
    lock.lock()?; // released when `lock` drops
    f()
}

/// Replace `path` with `bytes` in one step: write a temp file beside it, flush it, rename it over.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let tmp = path.with_extension("tmp");
    let mut f = std::fs::File::create(&tmp)?;
    f.write_all(bytes)?;
    f.sync_all()?;
    drop(f);
    std::fs::rename(&tmp, path).inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp);
    })
}

/// Set some keys of a JSON object file and keep every other key, under the lock. A missing file, or
/// one that is not a JSON object, starts as an empty object: this is for preferences, where a bad
/// file costs a default, not data.
pub fn set_json_keys(path: &Path, keys: &[(&str, serde_json::Value)]) -> std::io::Result<()> {
    locked(path, || {
        let mut v = std::fs::read_to_string(path)
            .ok()
            .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
            .filter(serde_json::Value::is_object)
            .unwrap_or_else(|| serde_json::json!({}));
        for (k, val) in keys {
            v[*k] = val.clone();
        }
        let text = serde_json::to_string_pretty(&v).map_err(std::io::Error::other)?;
        write_atomic(path, text.as_bytes())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("ripple-store-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn keys_are_set_and_the_others_kept() {
        let d = dir("keys");
        let f = d.join("gui.json");
        std::fs::write(&f, r#"{"font":"Fira Code","popups":false}"#).unwrap();
        set_json_keys(&f, &[("light", true.into())]).unwrap();
        let v: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&f).unwrap()).unwrap();
        assert_eq!(v["light"], true);
        assert_eq!(v["font"], "Fira Code");
        assert_eq!(v["popups"], false);
        assert!(!d.join("gui.tmp").exists(), "no temp file is left behind");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_file_that_is_not_an_object_starts_over() {
        let d = dir("junk");
        let f = d.join("gui.json");
        std::fs::write(&f, "[1,2").unwrap();
        set_json_keys(&f, &[("popups", true.into())]).unwrap();
        assert_eq!(std::fs::read_to_string(&f).unwrap().trim_start()[..1], *"{");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn writers_in_parallel_lose_nothing() {
        let d = dir("race");
        let f = d.join("gui.json");
        let threads: Vec<_> = (0..8)
            .map(|i| {
                let f = f.clone();
                std::thread::spawn(move || {
                    for j in 0..10 {
                        set_json_keys(&f, &[(&format!("k{i}_{j}"), i.into())]).unwrap();
                    }
                })
            })
            .collect();
        for t in threads {
            t.join().unwrap();
        }
        let v: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&f).unwrap()).unwrap();
        assert_eq!(v.as_object().unwrap().len(), 80, "every update survived");
        let _ = std::fs::remove_dir_all(&d);
    }
}
