//! AC auto boot where the firmware keeps it in the OEM's UEFI variable. The kernel driver's `ac_auto_boot` reads an EC
//! bit that stays 0 on such a laptop even while Windows has the feature on. The OEM Windows service keeps the setting
//! ("AC recovery") in `UniWillVariable`, its `NVRAM_STRUCT`: a support flag and, next to it, the on/off byte. We do
//! what that service does: use the variable when it says the feature is supported; the BIOS applies a change at the
//! next boot.

use crate::{HwError, HwResult};
use std::io::Write;
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

const VAR: &str = "/sys/firmware/efi/efivars/UniWillVariable-9f33f85c-13ca-4fd1-9c4a-96217722c593";
/// efivarfs shows the 4 attribute bytes (NV | BS | RT) first, then the data.
const ATTRS: [u8; 4] = [7, 0, 0, 0];
/// `NVRAM_STRUCT` is 180 bytes, the same in the OEM service 4.1 and 4.9.
const LEN: usize = 4 + 180;
/// `NVRAM_STRUCT.ACRecoverySupport`: 1 where the firmware has the feature.
const SUPPORT: usize = 4 + 94;
/// `NVRAM_STRUCT.ACRecoveryStatus`: the setting, 0 or 1.
const BYTE: usize = 4 + 95;
/// A firmware read blocks for about 15 ms and the window and the daemon look every second: look at most this often.
const MAX_AGE: Duration = Duration::from_secs(30);
/// `FS_IMMUTABLE_FL`: efivarfs files are immutable until the flag is cleared (`chattr -i`).
const IMMUTABLE: libc::c_long = 0x10;

pub struct AcBoot {
    path: PathBuf,
    seen: Mutex<Option<(Instant, bool)>>,
}

/// `Some(on)` when the image is the layout we know, says the feature is supported and has a 0/1 flag.
fn parse(img: &[u8]) -> Option<bool> {
    (img.len() == LEN && img[..4] == ATTRS && img[SUPPORT] == 1 && img[BYTE] <= 1)
        .then(|| img[BYTE] == 1)
}

/// The image with only the flag changed; refuses any layout `parse` does not know.
fn with_byte(img: &[u8], on: bool) -> HwResult<Vec<u8>> {
    parse(img).ok_or_else(|| {
        HwError::Other("the AC auto boot firmware variable has an unknown layout".into())
    })?;
    let mut new = img.to_vec();
    new[BYTE] = on.into();
    Ok(new)
}

fn set_flags(fd: i32, flags: libc::c_long) -> std::io::Result<()> {
    // SAFETY: FS_IOC_SETFLAGS reads one `long` from the pointer it is given.
    let r = unsafe { libc::ioctl(fd, libc::FS_IOC_SETFLAGS, &flags as *const libc::c_long) };
    if r < 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

impl AcBoot {
    pub(crate) fn at(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            seen: Mutex::new(None),
        }
    }

    /// `None` unless the variable is there, reads as the known layout and says the feature is supported.
    pub fn probe() -> Option<Self> {
        let a = Self::at(VAR);
        a.get().ok()?;
        Some(a)
    }

    fn read(path: &Path) -> HwResult<Vec<u8>> {
        std::fs::read(path).map_err(|e| crate::linux::map_io(e, path))
    }

    pub fn get(&self) -> HwResult<bool> {
        let mut seen = self.seen.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((at, on)) = *seen {
            if at.elapsed() < MAX_AGE {
                return Ok(on);
            }
        }
        let on = parse(&Self::read(&self.path)?).ok_or_else(|| {
            HwError::Other("the AC auto boot firmware variable has an unknown layout".into())
        })?;
        *seen = Some((Instant::now(), on));
        Ok(on)
    }

    /// Needs root. Takes effect at the next boot.
    pub fn set(&self, on: bool) -> HwResult<()> {
        let mut seen = self.seen.lock().unwrap_or_else(|e| e.into_inner());
        let img = Self::read(&self.path)?;
        let new = with_byte(&img, on)?;
        if new != img {
            self.write(&new)?;
        }
        *seen = Some((Instant::now(), on));
        Ok(())
    }

    fn write(&self, new: &[u8]) -> HwResult<()> {
        let p = &self.path;
        let flags_fd = std::fs::File::open(p).map_err(|e| crate::linux::map_io(e, p))?;
        let fd = flags_fd.as_raw_fd();
        let mut flags: libc::c_long = 0;
        // SAFETY: FS_IOC_GETFLAGS writes one `long` into the pointer it is given. It fails on a plain file (tests).
        let got =
            unsafe { libc::ioctl(fd, libc::FS_IOC_GETFLAGS, &mut flags as *mut libc::c_long) } == 0;
        let locked = got && flags & IMMUTABLE != 0;
        if locked {
            set_flags(fd, flags & !IMMUTABLE).map_err(|e| crate::linux::map_io(e, p))?;
        }
        // attributes + data go in ONE write: efivarfs sets the variable per write call, so a split or short write
        // would store a broken variable
        let wrote = std::fs::OpenOptions::new()
            .write(true)
            .open(p)
            .and_then(|mut f| f.write(new));
        if locked {
            let _ = set_flags(fd, flags); // put the flag back whether or not the write worked
        }
        match wrote {
            Ok(n) if n == new.len() => {}
            Ok(n) => {
                return Err(HwError::Other(format!(
                    "short write to {} ({n} of {} bytes)",
                    p.display(),
                    new.len()
                )))
            }
            Err(e) => return Err(crate::linux::map_io(e, p)),
        }
        if Self::read(p)? != new {
            return Err(HwError::Other(
                "the firmware variable does not read back as written".into(),
            ));
        }
        Ok(())
    }
}

/// A variable image with the flag set to `flag`, for the tests here and in `linux`.
#[cfg(test)]
pub(crate) fn image(flag: u8) -> Vec<u8> {
    let mut v = vec![0xff; LEN];
    v[..4].copy_from_slice(&ATTRS);
    v[SUPPORT] = 1;
    v[BYTE] = flag;
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_flag_changes_and_other_layouts_are_refused() {
        let a = image(1);
        let b = with_byte(&a, false).unwrap();
        assert_eq!(b[BYTE], 0);
        assert_eq!(a.iter().zip(&b).filter(|(x, y)| x != y).count(), 1);
        assert!(with_byte(&a[..LEN - 1], true).is_err(), "other size");
        let mut other_attrs = image(1);
        other_attrs[0] = 3;
        assert!(with_byte(&other_attrs, true).is_err(), "other attributes");
        assert!(with_byte(&image(2), true).is_err(), "not a 0/1 flag");
        let mut unsupported = image(1);
        unsupported[SUPPORT] = 0;
        assert!(
            with_byte(&unsupported, true).is_err(),
            "firmware says unsupported"
        );
    }

    #[test]
    fn set_writes_the_whole_image_and_reads_back() {
        // a plain file stands in for the variable: the real one is never touched by a test
        let p = std::env::temp_dir().join(format!("ripple-acboot-test-{}", std::process::id()));
        std::fs::write(&p, image(0)).unwrap();
        let a = AcBoot::at(&p);
        assert!(!a.get().unwrap());
        a.set(true).unwrap();
        assert_eq!(std::fs::read(&p).unwrap(), image(1));
        assert!(a.get().unwrap());
        a.set(true).unwrap(); // already on: nothing to write
        a.set(false).unwrap();
        assert_eq!(std::fs::read(&p).unwrap(), image(0));
        std::fs::remove_file(&p).unwrap();
    }
}
