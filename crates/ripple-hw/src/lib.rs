//! Hardware abstraction for EC / platform access.
//!
//! * [`EcAccess`] — the trait every backend implements (byte r/w with pacing,
//!   read-before-write and retry safety rules).
//! * `AcpiCallEc` / `WmiEc` / `SysfsEc` — Linux backends (`linux` module).
//! * [`require_root`] — every real hardware path needs root.

#[cfg(target_os = "linux")]
pub mod control;
#[cfg(target_os = "linux")]
pub mod linux;
#[cfg(test)]
mod mock;

use std::time::Duration;

/// Errors from a hardware backend.
#[derive(Debug, thiserror::Error)]
pub enum HwError {
    #[error("backend unavailable: {0}")]
    Unavailable(String),

    #[error("ec communication failure (0xFEFEFEFE) at addr {addr}")]
    EcFail { addr: u16 },

    #[error("permission denied accessing {0}")]
    Permission(String),

    #[error("io: {0}")]
    Io(#[from] std::io::Error),

    #[error("{0}")]
    Other(String),
}

pub type HwResult<T> = Result<T, HwError>;

/// Convenience: convert into `String` so CLI code can use `?` directly.
impl From<HwError> for String {
    fn from(e: HwError) -> Self {
        e.to_string()
    }
}

/// OEM pacing: wait 6 ms after every EC transaction.
pub const EC_PACE: Duration = Duration::from_millis(6);
/// Retries before giving up on an EC-failure.
pub const EC_RETRIES: u32 = 3;

/// Byte-granular EC register access.
///
/// Implementations should apply [`EC_PACE`] between transactions and map
/// the `0xFEFEFEFE` failure code to [`HwError::EcFail`].
pub trait EcAccess: Send {
    /// Human-readable backend name (for `rcc doctor`).
    fn name(&self) -> &str;

    /// Read one byte at `addr`.
    fn read(&mut self, addr: u16) -> HwResult<u8>;

    /// Write one byte at `addr`.
    fn write(&mut self, addr: u16, value: u8) -> HwResult<()>;

    /// Read-modify-write, preserving bits not in `mask`.
    fn update(&mut self, addr: u16, mask: u8, value: u8) -> HwResult<u8> {
        let cur = self.read(addr)?;
        let new = (cur & !mask) | (value & mask);
        if new != cur {
            self.write(addr, new)?;
        }
        Ok(new)
    }

    /// Read `len` consecutive bytes.
    fn read_block(&mut self, addr: u16, len: usize) -> HwResult<Vec<u8>> {
        let mut out = Vec::with_capacity(len);
        for i in 0..len {
            out.push(self.read(addr.wrapping_add(i as u16))?);
        }
        Ok(out)
    }

    /// Write `data` at `addr` and read back to verify.
    fn write_verified(&mut self, addr: u16, value: u8) -> HwResult<()> {
        self.write(addr, value)?;
        let back = self.read(addr)?;
        if back != value {
            return Err(HwError::Other(format!(
                "verify failed at {addr}: wrote {value:#04x}, read {back:#04x}"
            )));
        }
        Ok(())
    }
}

/// Effective uid out of `/proc/self/status` (`Uid: real effective saved fs`).
fn euid_from_status(status: &str) -> Option<u32> {
    let line = status.lines().find_map(|l| l.strip_prefix("Uid:"))?;
    line.split_whitespace().nth(1)?.parse().ok()
}

/// Hardware access (`/proc/acpi/call`, sysfs writes) only works as root, so
/// both binaries call this right after argument parsing (`--help` still works).
///
/// shortcut: the suggested command repeats the args unquoted, quote them by
/// hand if one contains spaces.
pub fn require_root(gui: bool) -> Result<(), String> {
    let status = std::fs::read_to_string("/proc/self/status").unwrap_or_default();
    if euid_from_status(&status) == Some(0) {
        return Ok(());
    }
    let exe = std::env::current_exe()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| "<path to this binary>".into());
    let args = std::env::args().skip(1).collect::<Vec<_>>().join(" ");
    // sudo drops the display env a GUI needs; keep it, but not HOME (-E would).
    let env = if gui {
        "--preserve-env=WAYLAND_DISPLAY,XDG_RUNTIME_DIR,DISPLAY,XAUTHORITY "
    } else {
        ""
    };
    Err(format!(
        "must be run as root (EC access via /proc/acpi/call and sysfs).\n\
         run:  sudo {env}{}",
        format!("{exe} {args}").trim_end()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn euid_is_the_second_uid_field() {
        let st = "Name:\tx\nUid:\t1000\t0\t0\t0\nGid:\t1000\t1000\t1000\t1000\n";
        assert_eq!(euid_from_status(st), Some(0));
        assert_eq!(euid_from_status("Uid:\t0\t1000\t0\t0\n"), Some(1000));
        assert_eq!(euid_from_status(""), None);
    }
}
