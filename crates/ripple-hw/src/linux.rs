//! Linux backends.
//!
//! * [`AcpiCallEc`] — ACPI `ECRR`/`ECRW` via `/proc/acpi/call`.
//! * [`WmiEc`] — WMI `WMBC 0 4 <val>` (`GetSetULong` encoding).
//! * [`SysfsEc`] — kernel `uniwill-laptop` sysfs (Linux ≥ 6.19), read-only
//!   fan/temp/charge-limit where available.
//!
//! ACPI namespace paths are **discovered at runtime** (sysfs
//! `/sys/bus/acpi/devices/<HID>:*/path` + a harmless test read): the INOU0000
//! Uniwill device lives at a DMI/DSDT-dependent path (`\_SB.INOU` on the dev
//! box, `\_SB.PCI0.LPCB.INOU0000`-style elsewhere). Hardcoded guesses are kept as
//! fallbacks.

use crate::{EcAccess, HwError, HwResult};
use ripple_core::protocol;

/// Read-only probe register: `ADDR_PROJECT_ID_BYTE`.
/// Existence + parseability of a read is all discovery needs.
const PROBE_ADDR: u64 = 1856;

// ---------------------------------------------------------------- open/detect

/// Best available EC backend: `acpi_call`, then WMI. No fake fallback.
pub fn open_auto() -> HwResult<Box<dyn EcAccess>> {
    let acpi = match AcpiCallEc::probe() {
        Ok(ec) => return Ok(Box::new(ec)),
        Err(e) => e,
    };
    match WmiEc::probe() {
        Ok(ec) => Ok(Box::new(ec)),
        Err(wmi) => Err(HwError::Unavailable(format!(
            "no EC backend (acpi: {acpi}; wmi: {wmi}) — run `rcc doctor`"
        ))),
    }
}

/// Model from the EC project id (primary) plus the DMI strings (hint only).
pub fn detect_model(ec: Option<&mut (dyn EcAccess + '_)>) -> ripple_core::model::Model {
    let ec_id = ec.and_then(|e| e.read(ripple_core::regs::addr::PROJECT_ID).ok());
    let dmi: Vec<String> = [
        "product_name",
        "product_family",
        "product_sku",
        "board_name",
        "board_version",
        "bios_version",
    ]
    .iter()
    .filter_map(|f| std::fs::read_to_string(format!("/sys/class/dmi/id/{f}")).ok())
    .map(|s| s.trim().to_string())
    .collect();
    ripple_core::model::detect(ec_id, &dmi)
}

// ---------------------------------------------------------------- acpi_call

/// `/proc/acpi/call` keeps one global result buffer, so a write+read pair from one process
/// can be answered with another process's result (CLI while the GUI polls). Serialise the
/// pair across processes. Best effort: no lock file, no lock.
fn ec_lock() -> Option<std::fs::File> {
    let f = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open("/run/ripple-nova-ec.lock")
        .ok()?;
    f.lock().ok()?;
    Some(f)
}

/// Write `method arg...` to `/proc/acpi/call` and parse the result.
fn acpi_call_raw(method: &str, args: &[u64]) -> HwResult<u64> {
    let mut payload = String::from(method);
    for a in args {
        payload.push_str(&format!(" 0x{a:X}"));
    }
    acpi_call_payload(method, &payload)
}

/// `GetSetULong` through `WMBC`: instance 0, method 4, an 8-byte *buffer* argument (`b<hex>` for
/// acpi_call; an integer works for reads here but a buffer is what the driver itself sends)
/// `addr:u16 data:u16 op:u16 0:u16`, op 0x0100 = read, 0 = write. Result byte 0 is the register value.
fn wmbc_call(wmbc: &str, addr: u16, data: u8, read: bool) -> HwResult<u8> {
    let op: u16 = if read { 0x0100 } else { 0 };
    let mut buf = [0u8; 8];
    buf[0..2].copy_from_slice(&addr.to_le_bytes());
    buf[2..4].copy_from_slice(&u16::from(data).to_le_bytes());
    buf[4..6].copy_from_slice(&op.to_le_bytes());
    let hex: String = buf.iter().map(|b| format!("{b:02x}")).collect();
    let v = acpi_call_payload(wmbc, &format!("{wmbc} 0x0 0x4 b{hex}"))?;
    if (v as u32) == protocol::EC_FAIL {
        return Err(HwError::EcFail { addr });
    }
    Ok(v as u8)
}

/// The fan duty registers behind `WMBC` (fan 1 = CPU, fan 2 = GPU), measured on this machine.
fn fan_duty_addr(fan: u8) -> HwResult<u16> {
    match fan {
        1 => Ok(0x1804),
        2 => Ok(0x1809),
        n => Err(HwError::Other(format!(
            "fan {n}: this EC has fans 1 (CPU) and 2 (GPU)"
        ))),
    }
}

fn acpi_call_payload(method: &str, payload: &str) -> HwResult<u64> {
    use std::io::Write;
    let _lock = ec_lock();

    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .open("/proc/acpi/call")
        .map_err(|e| {
            if e.kind() == std::io::ErrorKind::PermissionDenied {
                HwError::Permission("/proc/acpi/call (run with sudo)".into())
            } else {
                HwError::Other(format!("/proc/acpi/call: {e}"))
            }
        })?;
    f.write_all(payload.as_bytes())?;
    drop(f);

    // One read with a big buffer: acpi_call hands back nothing when the caller's buffer is smaller
    // than the result, and `read_to_string` starts with a 32-byte probe. Short integer results fit
    // anyway; a WMBC buffer result (40 bytes, ~250 characters) does not.
    let mut buf = vec![0u8; 8192];
    let n = std::fs::File::open("/proc/acpi/call")
        .and_then(|mut f| std::io::Read::read(&mut f, &mut buf))
        .map_err(|e| HwError::Other(format!("read result: {e}")))?;
    parse_acpi_result(method, &String::from_utf8_lossy(&buf[..n]))
}

/// acpi_call outputs: `0x1234` (NUL-terminated!), `1234`,
/// `{0x10, 0x07, …}` (buffer results), `Error: AE_NOT_FOUND`,
/// `<not supported>`, `0xffffffffffffffff` (evaluation failure).
fn parse_acpi_result(method: &str, out: &str) -> HwResult<u64> {
    // The proc file is NUL-terminated (`0x10\0`) — cut there first, then trim.
    let out = out.split('\0').next().unwrap_or("").trim();
    let tok = out.split_whitespace().next().unwrap_or("");

    if tok.starts_with("Error") || out.starts_with('<') {
        let upper = out.to_ascii_uppercase();
        if upper.contains("NOT_FOUND")
            || upper.contains("NOT SUPPORTED")
            || upper.contains("NOT_INITIALIZED")
            || upper.contains("UNDEFINED")
            || upper.contains("BAD_PATHNAME")
        {
            return Err(HwError::Unavailable(format!("ACPI method {method}: {out}")));
        }
        return Err(HwError::Other(format!("ACPI method {method}: {out}")));
    }

    // Buffer result: `{0x5b, 0x07, 0x00, …}` → little-endian u64.
    if out.starts_with('{') {
        let inner = out.trim_start_matches('{').trim_end_matches('}');
        let mut val: u64 = 0;
        for (i, b) in inner
            .split(',')
            .filter_map(|s| {
                let s = s.trim();
                s.strip_prefix("0x")
                    .or_else(|| s.strip_prefix("0X"))
                    .and_then(|h| u8::from_str_radix(h, 16).ok())
            })
            .take(8)
            .enumerate()
        {
            val |= (b as u64) << (8 * i);
        }
        if val == u64::MAX {
            return Err(HwError::EcFail { addr: 0 });
        }
        return Ok(val);
    }

    let val = if let Some(h) = tok.strip_prefix("0x").or_else(|| tok.strip_prefix("0X")) {
        u64::from_str_radix(h, 16)
            .map_err(|e| HwError::Other(format!("bad result '{out}': {e}")))?
    } else if tok.is_empty() {
        return Err(HwError::Other(format!(
            "no result from {method} (empty readback)"
        )));
    } else {
        tok.parse::<u64>()
            .map_err(|e| HwError::Other(format!("bad result '{out}': {e}")))?
    };

    // acpi_call reports evaluation failure as 0xffffffffffffffff.
    if val == u64::MAX {
        return Err(HwError::EcFail { addr: 0 });
    }
    Ok(val)
}

/// ACPI paths of devices whose sysfs HID starts with `hid` (e.g. `INOU0000`).
/// Both the sysfs-padded form (`\_SB_.INOU`) and the canonical trimmed form
/// (`\_SB.INOU`) are returned — acpi_call is tried against both.
fn acpi_sysfs_paths(hid: &str) -> Vec<String> {
    let mut out = Vec::new();
    if let Ok(rd) = std::fs::read_dir("/sys/bus/acpi/devices") {
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            if !name.starts_with(hid) {
                continue;
            }
            if let Ok(p) = std::fs::read_to_string(e.path().join("path")) {
                let p = p.trim();
                if p.starts_with('\\') {
                    push_unique(&mut out, trim_acpi_name(p));
                    push_unique(&mut out, p.to_string());
                }
            }
        }
    }
    out
}

fn push_unique(v: &mut Vec<String>, s: String) {
    if !v.contains(&s) {
        v.push(s);
    }
}

/// `\_SB_.PC00.LPCB.EC0_` (sysfs 4-char padded segments) → `\_SB.PC00.LPCB.EC0`.
fn trim_acpi_name(p: &str) -> String {
    let (prefix, rest) = p.split_at(1); // keep leading `\`
    let mut segs: Vec<String> = rest
        .split('.')
        .map(|s| s.trim_end_matches('_').to_string())
        .collect();
    if segs.is_empty() {
        return p.to_string();
    }
    let mut out = String::from(prefix);
    out.push_str(&segs.remove(0));
    for s in segs {
        out.push('.');
        out.push_str(&s);
    }
    out
}

/// Try `<candidate>.<method>` with `args` until one does not fail with
/// "method not found". Returns the working device path.
fn discover_method(candidates: &[String], method: &str, args: &[u64]) -> HwResult<String> {
    let mut tried: Vec<String> = Vec::new();
    // `\_SB_.X` and `\_SB.X` are two spellings of one device: report (and count) it once
    let mut note = |path: &str, why: String| {
        let line = format!("{}: {why}", path.replace("_SB_", "_SB"));
        if !tried.contains(&line) {
            tried.push(line);
        }
    };
    for c in candidates {
        let full = format!("{c}.{method}");
        match acpi_call_raw(&full, args) {
            // Method exists (an EC failure still proves the path resolves).
            Ok(_) | Err(HwError::EcFail { .. }) => return Ok(c.clone()),
            Err(e @ HwError::Permission(_)) => return Err(e),
            // Any other ACPI error: not a usable method, keep looking.
            Err(e) => note(c, short_reason(&e)),
        }
    }
    Err(HwError::Unavailable(format!(
        "no usable {method} method ({})",
        tried.join("; ")
    )))
}

/// A few words for why a probe failed (the full text of an ACPI error is not for a one-line summary).
fn short_reason(e: &HwError) -> String {
    let m = e.to_string();
    if m.contains("AE_NOT_FOUND") {
        "not found".into()
    } else if m.contains("empty readback") {
        "empty readback".into()
    } else {
        m.chars().take(60).collect()
    }
}

fn acpi_available() -> HwResult<()> {
    if std::path::Path::new("/proc/acpi/call").exists() {
        Ok(())
    } else {
        Err(HwError::Unavailable(
            "/proc/acpi/call not present (acpi_call module not loaded)".into(),
        ))
    }
}

/// ACPI method-call backend via the `acpi_call` module (`/proc/acpi/call`).
///
/// [`AcpiCallEc::probe`]
/// discovers a working `ECRR` method path with a harmless read.
pub struct AcpiCallEc {
    /// ACPI device path holding `ECRR`/`ECRW`, e.g. `\_SB.INOU`.
    device: String,
    /// Pacing (6 ms per OEM).
    pace: bool,
    /// `WMBC` path, found on first use (only the fan duty registers need it).
    wmbc: Option<String>,
}

impl AcpiCallEc {
    /// Discover a device that responds to `ECRR <addr>` and return a backend.
    pub fn probe() -> HwResult<Self> {
        acpi_available()?;

        let mut candidates: Vec<String> = Vec::new();
        // INOU0000 = the Uniwill "ACPI\INOU0000" device from UWACPIDriver.
        for p in acpi_sysfs_paths("INOU0000") {
            push_unique(&mut candidates, p);
        }
        // Hardcoded guesses (common Tongfang layouts).
        for p in [
            r"\_SB.INOU",
            r"\_SB_.INOU",
            r"\_SB.PCI0.LPCB.INOU0000",
            r"\_SB.PCI0.LPCB.EC0",
        ] {
            push_unique(&mut candidates, p.into());
        }
        // Last resort: standard EC devices (PNP0C09) — some DSDTs alias them.
        for p in acpi_sysfs_paths("PNP0C09") {
            push_unique(&mut candidates, p);
        }

        let device = discover_method(&candidates, "ECRR", &[PROBE_ADDR])?;
        Ok(Self {
            device,
            pace: true,
            wmbc: None,
        })
    }

    /// Discovered device path (shown by `rcc doctor`).
    pub fn device(&self) -> &str {
        &self.device
    }

    fn wmbc_path(&mut self) -> HwResult<String> {
        if let Some(p) = &self.wmbc {
            return Ok(p.clone());
        }
        let mut candidates: Vec<String> = Vec::new();
        for p in acpi_sysfs_paths("PNP0C14") {
            push_unique(&mut candidates, p);
        }
        for p in [r"\_SB.AMW0", r"\_SB_.AMW0"] {
            push_unique(&mut candidates, p.into());
        }
        let dev = discover_method(
            &candidates,
            "WMBC",
            &[0, 4, protocol::encode_read(PROBE_ADDR as u16)],
        )?;
        let path = format!("{dev}.WMBC");
        self.wmbc = Some(path.clone());
        Ok(path)
    }
}

impl EcAccess for AcpiCallEc {
    fn name(&self) -> &str {
        "acpi_call"
    }

    fn read(&mut self, addr: u16) -> HwResult<u8> {
        let full = format!("{}.ECRR", self.device);
        let v = match acpi_call_raw(&full, &[addr as u64]) {
            Ok(v) => v,
            Err(HwError::EcFail { .. }) => return Err(HwError::EcFail { addr }),
            Err(e) => return Err(e),
        };
        if self.pace {
            std::thread::sleep(crate::EC_PACE);
        }
        if (v as u32) == protocol::EC_FAIL {
            return Err(HwError::EcFail { addr });
        }
        Ok((v & 0xFF) as u8)
    }

    fn write(&mut self, addr: u16, value: u8) -> HwResult<()> {
        // ECRW(addr, data) — two args.
        let full = format!("{}.ECRW", self.device);
        match acpi_call_raw(&full, &[addr as u64, value as u64]) {
            Ok(_) => {}
            Err(HwError::EcFail { .. }) => return Err(HwError::EcFail { addr }),
            Err(e) => return Err(e),
        }
        if self.pace {
            std::thread::sleep(crate::EC_PACE);
        }
        Ok(())
    }

    fn fan_duty(&mut self, fan: u8) -> HwResult<u8> {
        let (addr, wmbc) = (fan_duty_addr(fan)?, self.wmbc_path()?);
        let v = wmbc_call(&wmbc, addr, 0, true)?;
        std::thread::sleep(crate::EC_PACE);
        Ok(v)
    }

    fn set_fan_duty(&mut self, fan: u8, duty: u8) -> HwResult<()> {
        if duty > 200 {
            return Err(HwError::Other(format!("fan duty is 0..=200 (got {duty})")));
        }
        let (addr, wmbc) = (fan_duty_addr(fan)?, self.wmbc_path()?);
        wmbc_call(&wmbc, addr, duty, false)?;
        std::thread::sleep(crate::EC_PACE);
        Ok(())
    }
}

/// WMI backend using `WMBC 0 4 <val>` with the
/// [`ripple_core::protocol`] `GetSetULong` encoding.
///
/// The `WMBC` device path is discovered at runtime (typically `\_SB.AMW0`,
/// one of the PNP0C14 WMI devices).
pub struct WmiEc {
    wmbc: String,
    pace: bool,
}

impl WmiEc {
    pub fn probe() -> HwResult<Self> {
        acpi_available()?;

        let mut candidates: Vec<String> = Vec::new();
        // All PNP0C14 WMI devices (WFDE/WFTE/AMW0 on this DSDT).
        for p in acpi_sysfs_paths("PNP0C14") {
            push_unique(&mut candidates, p);
        }
        for p in [r"\_SB.AMW0", r"\_SB_.AMW0"] {
            push_unique(&mut candidates, p.into());
        }

        let probe_args = [0, 4, protocol::encode_read(PROBE_ADDR as u16)];
        let wmbc = discover_method(&candidates, "WMBC", &probe_args)?;
        let wmbc = format!("{wmbc}.WMBC");

        // Validate: a real GetSetULong read returns the byte value, but some
        // DSDTs expose an unrelated WMBC that merely echoes the argument
        // (observed on this machine: {addr_lo, addr_hi, 0…} instead of the
        // register value — the opcode field cleared, everything else copied).
        let echo = acpi_call_raw(&wmbc, &probe_args).map(|v| {
            v & 0xFFFF == PROBE_ADDR && (v >> 16) & 0xFFFF == 0 && (v >> 32) & 0xFFFF == 0
        });
        if echo.unwrap_or(false) {
            return Err(HwError::Unavailable(format!(
                "{wmbc} echoes its argument instead of reading the EC — \
                 not the GetSetULong method on this firmware (use the \
                 acpi_call ECRR/ECRW backend)"
            )));
        }

        Ok(Self { wmbc, pace: true })
    }

    /// Discovered `WMBC` path (shown by `rcc doctor`).
    pub fn wmbc(&self) -> &str {
        &self.wmbc
    }

    fn call_wmbc(&self, data: u64) -> HwResult<u64> {
        // WMBC(Function=0, Command=4, Argument=data)
        acpi_call_raw(&self.wmbc, &[0, 4, data])
    }
}

impl EcAccess for WmiEc {
    fn name(&self) -> &str {
        "wmi(GetSetULong)"
    }

    fn read(&mut self, addr: u16) -> HwResult<u8> {
        let v = match self.call_wmbc(protocol::encode_read(addr)) {
            Ok(v) => v,
            Err(HwError::EcFail { .. }) => return Err(HwError::EcFail { addr }),
            Err(e) => return Err(e),
        };
        if self.pace {
            std::thread::sleep(crate::EC_PACE);
        }
        protocol::decode_read(v as u32).ok_or(HwError::EcFail { addr })
    }

    fn write(&mut self, addr: u16, value: u8) -> HwResult<()> {
        let v = match self.call_wmbc(protocol::encode_write(addr, value)) {
            Ok(v) => v,
            Err(HwError::EcFail { .. }) => return Err(HwError::EcFail { addr }),
            Err(e) => return Err(e),
        };
        if self.pace {
            std::thread::sleep(crate::EC_PACE);
        }
        // Writes return the echoed/op result; 0xFEFEFEFE signals EC failure.
        if (v as u32) == protocol::EC_FAIL {
            return Err(HwError::EcFail { addr });
        }
        Ok(())
    }
}

/// Kernel `uniwill-laptop` sysfs backend (Linux ≥ 6.19 / 7.x).
///
/// The bound driver (`/sys/bus/platform/devices/INOU0000:XX`) exposes:
/// * hwmon: CPU/GPU temps, up to 2 fan RPMs, pwm1/pwm2 (read-only duty)
/// * LED class lightbar (`uniwill:multicolor:status`, `multi_intensity`)
/// * `charge_types` battery profiles on the power supply
/// * platform attrs: `fn_lock`, `super_key_enable`, `touchpad_toggle_enable`,
///   `rainbow_animation`, `breathing_in_suspend`, `usb_c_power_priority`,
///   `ctgp_offset`, `ac_auto_boot`, `usb_powershare_high`
///
/// EC register r/w (fan curves, PL1/PL2, EC charge limit) stays with
/// [`AcpiCallEc`]/[`WmiEc`] — the kernel does not expose those.
/// ABI: `Documentation/ABI/testing/sysfs-driver-uniwill-laptop`.
pub struct SysfsEc {
    hwmon: std::path::PathBuf,
    platform: Option<std::path::PathBuf>,
}

/// Snapshot of the hwmon sensors.
#[derive(Debug, Default)]
pub struct Sensors {
    /// `(label, millidegreeC)`
    pub temps: Vec<(String, i32)>,
    /// `(label, rpm)`
    pub fans: Vec<(String, u32)>,
    /// raw pwm1..N (0..=255)
    pub pwms: Vec<u8>,
}

/// Known platform attributes (ABI file) in display order.
pub const PLATFORM_ATTRS: &[&str] = &[
    "fn_lock",
    "super_key_enable",
    "touchpad_toggle_enable",
    "rainbow_animation",
    "breathing_in_suspend",
    "usb_c_power_priority",
    "ctgp_offset",
    "ac_auto_boot",
    "usb_powershare_high",
];

/// Never both on: the driver doc calls it unsupported, but nothing in the kernel stops it, so
/// `set_attr` switches the other off when one is enabled.
const EXCLUSIVE: [&str; 2] = ["ac_auto_boot", "usb_powershare_high"];

/// The kernel calls the fans "Main"/"Secondary"; the Windows app names them by what they cool:
/// fan 1 = CPU, fan 2 = GPU.
fn fan_label(idx: u32) -> String {
    match idx {
        1 => "Fan 1 (CPU)".into(),
        2 => "Fan 2 (GPU)".into(),
        n => format!("Fan {n}"),
    }
}

fn write_sysfs(path: &std::path::Path, value: &str) -> HwResult<()> {
    use std::io::Write;
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .open(path)
        .map_err(|e| map_io(e, path))?;
    f.write_all(value.as_bytes()).map_err(|e| map_io(e, path))
}

fn map_io(e: std::io::Error, path: &std::path::Path) -> HwError {
    if e.kind() == std::io::ErrorKind::PermissionDenied {
        HwError::Permission(format!("{} (run with sudo)", path.display()))
    } else {
        HwError::Other(format!("{}: {e}", path.display()))
    }
}

fn read_sysfs(path: &std::path::Path) -> HwResult<String> {
    std::fs::read_to_string(path)
        .map(|s| s.trim().to_string())
        .map_err(|e| map_io(e, path))
}

impl SysfsEc {
    /// Find the bound `uniwill` platform device + its hwmon child.
    pub fn probe() -> HwResult<Self> {
        let mut platform = None;
        if let Ok(rd) = std::fs::read_dir("/sys/bus/platform/devices") {
            for e in rd.flatten() {
                let name = e.file_name().to_string_lossy().to_string();
                if name.starts_with("INOU") && e.path().join("driver").exists() {
                    platform = Some(e.path());
                    break;
                }
            }
        }

        // hwmon child of the platform device, else the global uniwill hwmon.
        let mut hwmon = None;
        if let Some(p) = &platform {
            if let Ok(rd) = std::fs::read_dir(p.join("hwmon")) {
                hwmon = rd.flatten().next().map(|e| e.path());
            }
        }
        if hwmon.is_none() {
            if let Ok(rd) = std::fs::read_dir("/sys/class/hwmon") {
                for e in rd.flatten() {
                    if std::fs::read_to_string(e.path().join("name"))
                        .map(|n| n.trim() == "uniwill")
                        .unwrap_or(false)
                    {
                        hwmon = Some(e.path());
                        break;
                    }
                }
            }
        }

        match (hwmon, platform) {
            (Some(hwmon), platform) => Ok(Self { hwmon, platform }),
            _ => Err(HwError::Unavailable(
                "uniwill-laptop not bound — run: sudo modprobe uniwill-laptop \
                 (DMI whitelist: force=1) — needs Linux >= 6.19"
                    .into(),
            )),
        }
    }

    pub fn hwmon_dir(&self) -> &std::path::Path {
        &self.hwmon
    }

    pub fn platform_dir(&self) -> Option<&std::path::Path> {
        self.platform.as_deref()
    }

    /// One-line inventory for `rcc doctor`.
    pub fn summary(&self) -> String {
        let mut bits: Vec<String> = Vec::new();
        bits.push(format!(
            "hwmon {}",
            self.hwmon.file_name().unwrap_or_default().to_string_lossy()
        ));
        if self.lightbar().is_ok() {
            bits.push("lightbar LED".into());
        }
        if self.charge_types().is_ok() {
            bits.push("charge_types".into());
        }
        let attrs = self
            .platform_attrs()
            .iter()
            .filter(|(_, v)| v.is_some())
            .count();
        if attrs > 0 {
            bits.push(format!("{attrs} platform attrs"));
        }
        bits.join(", ")
    }

    /// Read all hwmon sensors (temps, fan RPMs, pwm duty), each ordered by its hwmon index
    /// so `fans[i]` and `pwms[i]` describe the same fan.
    pub fn sensors(&self) -> HwResult<Sensors> {
        let rd = std::fs::read_dir(&self.hwmon).map_err(|e| map_io(e, &self.hwmon))?;
        let (mut temps, mut fans, mut pwms) = (Vec::new(), Vec::new(), Vec::new());
        let label = |kind: &str, idx: u32| {
            std::fs::read_to_string(self.hwmon.join(format!("{kind}{idx}_label")))
                .map(|l| l.trim().to_string())
                .unwrap_or_else(|_| format!("{kind}{idx}"))
        };
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            let read = |p: &std::path::Path| {
                std::fs::read_to_string(p)
                    .ok()
                    .and_then(|v| v.trim().parse::<i64>().ok())
            };
            if let Some(idx) = name
                .strip_prefix("temp")
                .and_then(|r| r.strip_suffix("_input"))
                .and_then(|i| i.parse::<u32>().ok())
            {
                if let Some(mv) = read(&e.path()) {
                    temps.push((idx, label("temp", idx), mv as i32));
                }
            } else if let Some(idx) = name
                .strip_prefix("fan")
                .and_then(|r| r.strip_suffix("_input"))
                .and_then(|i| i.parse::<u32>().ok())
            {
                if let Some(rpm) = read(&e.path()) {
                    fans.push((idx, fan_label(idx), rpm as u32));
                }
            } else if let Some(idx) = name.strip_prefix("pwm").and_then(|i| i.parse::<u32>().ok()) {
                if let Some(v) = read(&e.path()) {
                    pwms.push((idx, v.clamp(0, 255) as u8));
                }
            }
        }
        temps.sort_by_key(|t| t.0);
        fans.sort_by_key(|t| t.0);
        pwms.sort_by_key(|t| t.0);
        Ok(Sensors {
            temps: temps.into_iter().map(|(_, l, v)| (l, v)).collect(),
            fans: fans.into_iter().map(|(_, l, v)| (l, v)).collect(),
            pwms: pwms.into_iter().map(|(_, v)| v).collect(),
        })
    }

    // ------------------------------------------------------------ lightbar

    /// The lightbar LED device: a uniwill multicolor LED (`uniwill:*:status` preferred).
    /// Never any other driver's LED that merely has `multi_intensity`.
    fn lightbar_dir(&self) -> HwResult<std::path::PathBuf> {
        let rd = std::fs::read_dir("/sys/class/leds")
            .map_err(|e| HwError::Other(format!("leds: {e}")))?;
        let mut found: Vec<(bool, std::path::PathBuf)> = rd
            .flatten()
            .filter_map(|e| {
                let name = e.file_name().to_string_lossy().to_string();
                (name.contains("uniwill") && e.path().join("multi_intensity").exists())
                    .then(|| (name.contains("status"), e.path()))
            })
            .collect();
        found.sort_by_key(|(status, _)| !*status);
        found.into_iter().next().map(|(_, p)| p).ok_or_else(|| {
            HwError::Unavailable("no uniwill multicolor LED (lightbar) found".into())
        })
    }

    /// Current lightbar state: `(r, g, b, brightness)`.
    pub fn lightbar(&self) -> HwResult<(u8, u8, u8, u8)> {
        let dir = self.lightbar_dir()?;
        let ints = read_sysfs(&dir.join("multi_intensity"))?;
        let mut it = ints.split_whitespace();
        let r = it.next().and_then(|v| v.parse().ok()).unwrap_or(0);
        let g = it.next().and_then(|v| v.parse().ok()).unwrap_or(0);
        let b = it.next().and_then(|v| v.parse().ok()).unwrap_or(0);
        let bri = read_sysfs(&dir.join("brightness"))
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(0);
        Ok((r, g, b, bri))
    }

    /// Set lightbar color; `None` turns it off (brightness 0, color kept).
    pub fn set_lightbar(&self, rgb: Option<(u8, u8, u8)>) -> HwResult<()> {
        let dir = self.lightbar_dir()?;
        match rgb {
            Some((r, g, b)) => {
                // Clamp to the device's per-channel maximum (docs: RGB max 50
                // on some models, 200 on others).
                let (mut r, mut g, mut b) = (r, g, b);
                if let Ok(maxes) = read_sysfs(&dir.join("multi_max_intensity")) {
                    let m: Vec<u8> = maxes
                        .split_whitespace()
                        .filter_map(|v| v.parse().ok())
                        .collect();
                    if let (Some(&mr), Some(&mg), Some(&mb)) = (m.first(), m.get(1), m.get(2)) {
                        r = r.min(mr);
                        g = g.min(mg);
                        b = b.min(mb);
                    }
                }
                write_sysfs(&dir.join("multi_intensity"), &format!("{r} {g} {b}"))?;
                // turn brightness up if it was off
                let bri = read_sysfs(&dir.join("brightness"))
                    .ok()
                    .and_then(|v| v.parse::<u8>().ok())
                    .unwrap_or(0);
                if bri == 0 {
                    let max = read_sysfs(&dir.join("max_brightness"))
                        .ok()
                        .and_then(|v| v.parse::<u8>().ok())
                        .unwrap_or(200);
                    write_sysfs(&dir.join("brightness"), &max.to_string())?;
                }
            }
            None => write_sysfs(&dir.join("brightness"), "0")?,
        }
        Ok(())
    }

    // ------------------------------------------------------------ battery

    /// `(selected, available)` charge profiles from `charge_types`.
    /// Format: `Trickle [Standard] Long_Life`.
    pub fn charge_types(&self) -> HwResult<(String, Vec<String>)> {
        let bat = std::path::Path::new("/sys/class/power_supply");
        for e in std::fs::read_dir(bat)
            .map_err(|e| HwError::Other(format!("power_supply: {e}")))?
            .flatten()
        {
            let f = e.path().join("charge_types");
            if f.is_file() {
                let raw = read_sysfs(&f)?;
                let mut sel = String::new();
                let mut all = Vec::new();
                for tok in raw.split_whitespace() {
                    if let Some(inner) = tok.strip_prefix('[').and_then(|t| t.strip_suffix(']')) {
                        sel = inner.to_string();
                        all.push(inner.to_string());
                    } else {
                        all.push(tok.to_string());
                    }
                }
                return Ok((sel, all));
            }
        }
        Err(HwError::Unavailable(
            "no BAT*/charge_types (uniwill-laptop charging interface)".into(),
        ))
    }

    /// Write a charge profile (`Standard` / `Trickle` / `Long_Life`).
    pub fn set_charge_types(&self, want: &str) -> HwResult<()> {
        let (sel, all) = self.charge_types()?;
        let norm = |s: &str| s.to_ascii_lowercase().replace('-', "_");
        let target = all
            .iter()
            .find(|a| norm(a) == norm(want))
            .cloned()
            .ok_or_else(|| {
                HwError::Other(format!(
                    "unknown charge profile '{want}' (available: {})",
                    all.join(", ")
                ))
            })?;
        if sel == target {
            return Ok(());
        }
        let bat = std::path::Path::new("/sys/class/power_supply");
        for e in std::fs::read_dir(bat)
            .map_err(|e| HwError::Other(format!("power_supply: {e}")))?
            .flatten()
        {
            let f = e.path().join("charge_types");
            if f.is_file() {
                return write_sysfs(&f, &target);
            }
        }
        Err(HwError::Unavailable("charge_types disappeared".into()))
    }

    // ------------------------------------------------------- platform attrs

    /// All known platform attributes with their current values (if present).
    pub fn platform_attrs(&self) -> Vec<(&'static str, Option<String>)> {
        PLATFORM_ATTRS
            .iter()
            .map(|n| {
                let v = self
                    .platform
                    .as_ref()
                    .map(|p| read_sysfs(&p.join(n)))
                    .and_then(|r| r.ok());
                (*n, v)
            })
            .collect()
    }

    fn platform_attr_path(&self, name: &str) -> HwResult<std::path::PathBuf> {
        if !PLATFORM_ATTRS.contains(&name) {
            return Err(HwError::Other(format!(
                "unknown platform attribute '{name}' (known: {})",
                PLATFORM_ATTRS.join(", ")
            )));
        }
        let p = self
            .platform
            .as_ref()
            .ok_or_else(|| HwError::Unavailable("no bound uniwill platform device".into()))?;
        let path = p.join(name);
        if !path.exists() {
            return Err(HwError::Unavailable(format!(
                "{name} not present on this kernel/device"
            )));
        }
        Ok(path)
    }

    /// Read one platform attribute (`fn_lock`, `ctgp_offset`, …).
    pub fn get_attr(&self, name: &str) -> HwResult<String> {
        read_sysfs(&self.platform_attr_path(name)?)
    }

    /// Write one platform attribute. `on/off/1/0/true/false` are normalised
    /// to `1/0`; anything else (`charging`, integers) is passed through.
    pub fn set_attr(&self, name: &str, value: &str) -> HwResult<String> {
        let path = self.platform_attr_path(name)?;
        let lower = value.to_ascii_lowercase();
        let normalised = match lower.as_str() {
            "on" | "true" | "1" | "enable" | "enabled" => "1",
            "off" | "false" | "0" | "disable" | "disabled" => "0",
            other => other,
        };
        if normalised == "1" && EXCLUSIVE.contains(&name) {
            for other in EXCLUSIVE.iter().filter(|o| **o != name) {
                // an absent partner (older kernel) has nothing to switch off
                if self.get_attr(other).is_ok_and(|v| v == "1") {
                    self.set_attr(other, "0")?;
                }
            }
        }
        write_sysfs(&path, normalised)?;
        Ok(normalised.to_string())
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auto_boot_and_powershare_exclude_each_other() {
        let dir = std::env::temp_dir().join(format!("ripple-excl-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let ec = SysfsEc {
            hwmon: std::path::PathBuf::new(),
            platform: Some(dir.clone()),
        };
        let state = |a: &str, p: &str| {
            std::fs::write(dir.join("ac_auto_boot"), format!("{a}\n")).unwrap();
            std::fs::write(dir.join("usb_powershare_high"), format!("{p}\n")).unwrap();
        };
        let get = |n: &str| ec.get_attr(n).unwrap();

        state("0", "1");
        ec.set_attr("ac_auto_boot", "on").unwrap();
        assert_eq!(
            (get("ac_auto_boot"), get("usb_powershare_high")),
            ("1".into(), "0".into())
        );

        ec.set_attr("usb_powershare_high", "on").unwrap();
        assert_eq!(
            (get("ac_auto_boot"), get("usb_powershare_high")),
            ("0".into(), "1".into())
        );

        // turning one off leaves the other alone
        state("0", "1");
        ec.set_attr("ac_auto_boot", "off").unwrap();
        assert_eq!(
            (get("ac_auto_boot"), get("usb_powershare_high")),
            ("0".into(), "1".into())
        );

        // no partner attribute (older kernel): the write still goes through
        std::fs::remove_file(dir.join("usb_powershare_high")).unwrap();
        ec.set_attr("ac_auto_boot", "on").unwrap();
        assert_eq!(get("ac_auto_boot"), "1");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn parse_hex_and_decimal() {
        assert_eq!(parse_acpi_result("X", "0x1234").unwrap(), 0x1234);
        assert_eq!(parse_acpi_result("X", "42").unwrap(), 42);
        assert_eq!(parse_acpi_result("X", "0x12\n").unwrap(), 0x12);
    }

    #[test]
    fn parse_nul_terminated_result() {
        // Real /proc/acpi/call output observed on the dev box: "0x10\0".
        assert_eq!(parse_acpi_result("X", "0x10\0").unwrap(), 0x10);
        assert_eq!(parse_acpi_result("X", "0x3c\0").unwrap(), 0x3c);
    }

    #[test]
    fn parse_buffer_result() {
        // WMBC buffer form observed live: {0x5b, 0x07, 0x00…} → LE u64.
        let s = "{0x5b, 0x07, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00}";
        assert_eq!(parse_acpi_result("X", s).unwrap(), 0x075B);
        let s = "{0x10, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00}";
        assert_eq!(parse_acpi_result("X", s).unwrap(), 0x10);
    }

    #[test]
    fn wmbc_echo_detection() {
        // The observed echo keeps addr in bits0..15 and clears the opcode.
        let echo: u64 = 0x0740; // addr echoed, data+opcode zero
        assert!(echo & 0xFFFF == 1856 && (echo >> 16) & 0xFFFF == 0);
        // A genuine read result (byte value in the low 8 bits) cannot match:
        let real: u64 = 0x10;
        assert!(!(real & 0xFFFF == 1856));
    }

    #[test]
    fn parse_acpi_errors_are_classified() {
        // The bug from `rcc mode get`: AE_NOT_FOUND must not be parsed as a number.
        let e = parse_acpi_result(r"\_SB.PCI0.LPCB.EC0.ECRR", "Error: AE_NOT_FOUND").unwrap_err();
        assert!(matches!(e, HwError::Unavailable(_)), "{e:?}");
        assert!(e.to_string().contains("AE_NOT_FOUND"));

        let e = parse_acpi_result("X", "Error: AE_ERROR_TYPE").unwrap_err();
        assert!(matches!(e, HwError::Other(_)), "{e:?}");

        let e = parse_acpi_result("X", "0xffffffffffffffff").unwrap_err();
        assert!(matches!(e, HwError::EcFail { .. }), "{e:?}");
    }

    #[test]
    fn trims_sysfs_acpi_path_padding() {
        assert_eq!(trim_acpi_name(r"\_SB_.INOU"), r"\_SB.INOU");
        assert_eq!(
            trim_acpi_name(r"\_SB_.PC00.LPCB.EC0_"),
            r"\_SB.PC00.LPCB.EC0"
        );
        assert_eq!(
            trim_acpi_name(r"\_SB_.PC00.HDAS.SNDW"),
            r"\_SB.PC00.HDAS.SNDW"
        );
        // inner underscores must survive
        assert_eq!(
            trim_acpi_name(r"\_SB_.PC00.LPCB.H_EC"),
            r"\_SB.PC00.LPCB.H_EC"
        );
    }
}
