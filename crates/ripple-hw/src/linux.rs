//! Linux backends.
//!
//! * [`AcpiCallEc`] — ACPI `ECRR`/`ECRW` on `INOU0000` via `/proc/acpi/call`.
//! * [`WmiEc`] — WMI `\_SB.AMW0.WMBC 0 4 <val>` (`GetSetULong` encoding).
//! * [`SysfsEc`] — kernel `uniwill-laptop` sysfs (Linux ≥ 6.19), read-only
//!   fan/temp/charge-limit where available.

use crate::{EcAccess, HwError, HwResult};
use ripple_core::protocol;

/// ACPI method-call backend via the `acpi_call` module (`/proc/acpi/call`).
///
/// Construction probes for the
/// device; use [`AcpiCallEc::probe`] to check availability first.
pub struct AcpiCallEc {
    /// ACPI path of the EC RAM methods on INOU0000, e.g. `\\_SB.PCI0.LPCB.EC0`.
    device: String,
    /// Pacing (6 ms per OEM).
    pace: bool,
}

impl AcpiCallEc {
    /// Write an arbitrary ACPI method invocation with three u64 args
    /// (used for `\_SB.AMW0.WMBC 0 4 <data>`).
    pub fn call3(&mut self, method: &str, a0: u64, a1: u64, a2: u64) -> HwResult<u64> {
        use std::io::Write;
        let payload = format!("{} 0x{:X} 0x{:X} 0x{:X}", method, a0, a1, a2);
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .open("/proc/acpi/call")
            .map_err(|e| HwError::Permission(format!("/proc/acpi/call: {e}")))?;
        f.write_all(payload.as_bytes())?;
        drop(f);
        Self::read_result()
    }

    fn read_result() -> HwResult<u64> {
        let out = std::fs::read_to_string("/proc/acpi/call")
            .map_err(|e| HwError::Other(format!("read result: {e}")))?;
        let out = out.trim();
        let val = if let Some(h) = out.strip_prefix("0x") {
            u64::from_str_radix(h, 16)
                .map_err(|e| HwError::Other(format!("bad result '{out}': {e}")))?
        } else {
            out.parse::<u64>()
                .map_err(|e| HwError::Other(format!("bad result '{out}': {e}")))?
        };
        if val == u64::MAX {
            return Err(HwError::EcFail { addr: 0 });
        }
        Ok(val)
    }

    /// Probe for `/proc/acpi/call` and a callable `ECRR` method.
    pub fn probe() -> HwResult<Self> {
        if !std::path::Path::new("/proc/acpi/call").exists() {
            return Err(HwError::Unavailable(
                "/proc/acpi/call missing — load the acpi_call module (dkms)".into(),
            ));
        }
        // Device path discovery (DMI/ACPI walk) happens in ripple-cli `doctor`
        // for now; default to the common Tongfang path.
        Ok(Self {
            device: r"\_SB.PCI0.LPCB.EC0".into(),
            pace: true,
        })
    }

    fn call(&mut self, method: &str, arg: u64) -> HwResult<u64> {
        use std::io::Write;
        let payload = format!("{} 0x{:X}", method, arg);
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .open("/proc/acpi/call")
            .map_err(|e| HwError::Permission(format!("/proc/acpi/call: {e}")))?;
        f.write_all(payload.as_bytes())?;
        drop(f);

        let out = std::fs::read_to_string("/proc/acpi/call")
            .map_err(|e| HwError::Other(format!("read result: {e}")))?;
        let out = out.trim();
        let val = if let Some(h) = out.strip_prefix("0x") {
            u64::from_str_radix(h, 16)
                .map_err(|e| HwError::Other(format!("bad result '{out}': {e}")))?
        } else {
            out.parse::<u64>()
                .map_err(|e| HwError::Other(format!("bad result '{out}': {e}")))?
        };

        // acpi_call reports failures as 0xffffffffffffffff or "<not supported>".
        if val == u64::MAX {
            return Err(HwError::EcFail { addr: 0 });
        }
        Ok(val)
    }
}

impl EcAccess for AcpiCallEc {
    fn name(&self) -> &str {
        "acpi_call"
    }

    fn read(&mut self, addr: u16) -> HwResult<u8> {
        let full = format!("{}.ECRR", self.device);
        let v = self.call(&full, addr as u64)?;
        if self.pace {
            std::thread::sleep(crate::EC_PACE);
        }
        Ok((v & 0xFF) as u8)
    }

    fn write(&mut self, addr: u16, value: u8) -> HwResult<()> {
        let full = format!("{}.ECRW", self.device);
        self.call(&full, (addr as u64) | ((value as u64) << 8))?;
        if self.pace {
            std::thread::sleep(crate::EC_PACE);
        }
        Ok(())
    }
}

/// WMI backend using `\_SB.AMW0.WMBC 0 4 <val>` with the
/// [`ripple_core::protocol`] `GetSetULong` encoding.
pub struct WmiEc {
    pace: bool,
}

impl WmiEc {
    pub fn probe() -> HwResult<Self> {
        if !std::path::Path::new("/proc/acpi/call").exists() {
            return Err(HwError::Unavailable(
                "/proc/acpi/call missing — load the acpi_call module (dkms)".into(),
            ));
        }
        Ok(Self { pace: true })
    }
}

impl EcAccess for WmiEc {
    fn name(&self) -> &str {
        "wmi(GetSetULong)"
    }

    fn read(&mut self, addr: u16) -> HwResult<u8> {
        let data = protocol::encode_read(addr);
        let mut ec = AcpiCallEc::probe()?;
        let v = self.call_wmbc(&mut ec, data)?;
        if self.pace {
            std::thread::sleep(crate::EC_PACE);
        }
        protocol::decode_read(v as u32).ok_or(HwError::EcFail { addr })
    }

    fn write(&mut self, addr: u16, value: u8) -> HwResult<()> {
        let data = protocol::encode_write(addr, value);
        let mut ec = AcpiCallEc::probe()?;
        let _ = self.call_wmbc(&mut ec, data)?;
        if self.pace {
            std::thread::sleep(crate::EC_PACE);
        }
        Ok(())
    }
}

impl WmiEc {
    fn call_wmbc(&self, ec: &mut AcpiCallEc, data: u64) -> HwResult<u64> {
        // WMBC(Function=0, Command=4, Argument=data)
        ec.call3(r"\_SB.AMW0.WMBC", 0, 4, data)
    }
}

/// Kernel `uniwill-laptop` sysfs backend (read-only; Linux ≥ 6.19).
///
/// Fan RPM/duty/temps and battery charge limit come from
/// `/sys/class/hwmon/hwmonX/` + `/sys/class/power_supply/BAT*/`.
pub struct SysfsEc {
    hwmon: std::path::PathBuf,
}

impl SysfsEc {
    /// Find a hwmon device that carries the `uniwill` driver.
    pub fn probe() -> HwResult<Self> {
        let base = std::path::Path::new("/sys/class/hwmon");
        if let Ok(rd) = std::fs::read_dir(base) {
            for entry in rd.flatten() {
                let name = entry.path().join("name");
                if let Ok(n) = std::fs::read_to_string(&name) {
                    if n.trim() == "uniwill" {
                        return Ok(Self { hwmon: entry.path() });
                    }
                }
            }
        }
        Err(HwError::Unavailable(
            "kernel uniwill-laptop hwmon not found (needs Linux >= 6.19)".into(),
        ))
    }

    pub fn hwmon_dir(&self) -> &std::path::Path {
        &self.hwmon
    }
}

impl EcAccess for SysfsEc {
    fn name(&self) -> &str {
        "sysfs(uniwill)"
    }

    fn read(&mut self, _addr: u16) -> HwResult<u8> {
        Err(HwError::Unavailable(
            "sysfs backend is read-only for sensors; use acpi_call/wmi for EC r/w".into(),
        ))
    }

    fn write(&mut self, _addr: u16, _value: u8) -> HwResult<()> {
        Err(HwError::Unavailable(
            "sysfs backend is read-only for sensors; use acpi_call/wmi for EC r/w".into(),
        ))
    }
}
