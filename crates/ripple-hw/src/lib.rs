//! Hardware abstraction for EC / platform access.
//!
//! * [`EcAccess`] — the trait every backend implements (byte r/w with pacing,
//!   read-before-write and retry safety rules).
//! * `AcpiCallEc` / `WmiEc` / `SysfsEc` — Linux backends (`linux` module).
//! * [`require_root`] — every real hardware path needs root.

#[cfg(target_os = "linux")]
pub mod control;
#[cfg(target_os = "linux")]
pub mod gpu;
#[cfg(target_os = "linux")]
pub mod linux;
#[cfg(test)]
mod mock;
#[cfg(target_os = "linux")]
pub mod notify;
#[cfg(target_os = "linux")]
pub mod store;
#[cfg(target_os = "linux")]
pub mod touchpad;

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

/// A plain message is an `Other` error, so a `String` error can `?` into a `HwResult`.
impl From<String> for HwError {
    fn from(msg: String) -> Self {
        Self::Other(msg)
    }
}

/// Convenience: convert into `String` so CLI code can use `?` directly.
impl From<HwError> for String {
    fn from(e: HwError) -> Self {
        e.to_string()
    }
}

/// OEM pacing: wait 6 ms after every EC transaction.
pub const EC_PACE: Duration = Duration::from_millis(6);
/// Attempts (the first one included) before giving up on an EC failure.
pub const EC_RETRIES: u32 = 3;

/// Run one EC transaction, repeating it when the EC answers 0xFEFEFEFE (it was busy): reads, and
/// writes of an absolute value, are safe to repeat. Any other error is final.
pub fn with_retries<T>(mut op: impl FnMut() -> HwResult<T>) -> HwResult<T> {
    let mut attempt = 1;
    loop {
        match op() {
            Err(HwError::EcFail { .. }) if attempt < EC_RETRIES => {
                attempt += 1;
                std::thread::sleep(EC_PACE);
            }
            r => return r,
        }
    }
}

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

    /// Duty of fan `fan` (1 = CPU, 2 = GPU) on the EC's 0..=200 scale. These registers (0x1804 and
    /// 0x1809) are out of `ECRR`'s reach (it reads 0xFF there): only the OEM WMI call gets at them.
    fn fan_duty(&mut self, _fan: u8) -> HwResult<u8> {
        Err(HwError::Unavailable(format!(
            "{}: no WMI fan duty access",
            self.name()
        )))
    }

    /// Set a fan's duty (0..=200). The EC only follows it while the manual bit (0x40) of EC 1873 is set.
    fn set_fan_duty(&mut self, _fan: u8, _duty: u8) -> HwResult<()> {
        Err(HwError::Unavailable(format!(
            "{}: no WMI fan duty access",
            self.name()
        )))
    }

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
    fn a_busy_ec_is_asked_again_but_other_errors_are_final() {
        let mut calls = 0;
        let r = with_retries(|| {
            calls += 1;
            if calls < 3 {
                Err(HwError::EcFail { addr: 7 })
            } else {
                Ok(calls)
            }
        });
        assert_eq!(r.unwrap(), 3);
        calls = 0;
        let r: HwResult<u32> = with_retries(|| {
            calls += 1;
            Err(HwError::EcFail { addr: 7 })
        });
        assert!(matches!(r, Err(HwError::EcFail { addr: 7 })));
        assert_eq!(calls, EC_RETRIES, "gives up after the last attempt");
        calls = 0;
        let r: HwResult<u32> = with_retries(|| {
            calls += 1;
            Err(HwError::Other("no".into()))
        });
        assert!(r.is_err());
        assert_eq!(calls, 1, "only an EC failure is repeated");
    }

    #[test]
    fn euid_is_the_second_uid_field() {
        let st = "Name:\tx\nUid:\t1000\t0\t0\t0\nGid:\t1000\t1000\t1000\t1000\n";
        assert_eq!(euid_from_status(st), Some(0));
        assert_eq!(euid_from_status("Uid:\t0\t1000\t0\t0\n"), Some(1000));
        assert_eq!(euid_from_status(""), None);
    }
}
