//! Command implementations.

use std::path::PathBuf;

use ripple_core::fan::{list_profiles, model_dir_for, FanTable};
use ripple_core::modes::{FanControlByte, OperatingMode, ProtectionStatus};
use ripple_core::regs::{addr, duty_from_ec};
use ripple_core::rgb::RgbRegState;
use ripple_hw::{EcAccess, HwError};

use crate::{BatteryCmd, Cli, CurveCmd, DisplayCmd, FanCmd, ModeCmd, PowerCmd, RgbCmd};

pub type CmdResult = Result<(), String>;

pub fn run(cli: &Cli) -> CmdResult {
    match &cli.command {
        crate::Command::Doctor => doctor(cli),
        crate::Command::Status => status(cli),
        crate::Command::Monitor { interval } => monitor(cli, *interval),
        crate::Command::Mode { cmd } => mode(cli, cmd),
        crate::Command::Fan { cmd } => fan(cli, cmd),
        crate::Command::Power { cmd } => power(cli, cmd),
        crate::Command::Rgb { cmd } => rgb(cli, cmd),
        crate::Command::Battery { cmd } => battery(cli, cmd),
        crate::Command::Display { cmd } => display(cli, cmd),
    }
}

// ---------------------------------------------------------------- backends

fn open_ec(cli: &Cli) -> Result<Box<dyn EcAccess>, String> {
    match cli.backend.as_str() {
        "mock" => Ok(Box::new(ripple_hw::mock::samples::ph4())),
        "acpi" => Ok(Box::new(
            ripple_hw::linux::AcpiCallEc::probe().map_err(err)?,
        )),
        "wmi" => Ok(Box::new(ripple_hw::linux::WmiEc::probe().map_err(err)?)),
        "sysfs" => Ok(Box::new(
            ripple_hw::linux::SysfsEc::probe().map_err(err)?,
        )),
        "auto" => {
            // Prefer real backends; fall back to mock for development.
            if let Ok(ec) = ripple_hw::linux::AcpiCallEc::probe() {
                return Ok(Box::new(ec));
            }
            if let Ok(ec) = ripple_hw::linux::WmiEc::probe() {
                return Ok(Box::new(ec));
            }
            eprintln!("note: no EC backend available, using --mock image (see `rcc doctor`)");
            Ok(Box::new(ripple_hw::mock::samples::ph4()))
        }
        other => Err(format!("unknown backend '{other}' (auto|mock|acpi|wmi|sysfs)")),
    }
}

fn err(e: HwError) -> String {
    e.to_string()
}

/// Locate the OEM package root (UserFanTables/ + RGBKeyboard.reg).
fn oem_dir(cli: &Cli) -> Option<PathBuf> {
    if let Some(d) = &cli.oem_dir {
        return Some(d.clone());
    }
    let candidates = [
        std::env::var("RCC_OEM_DIR").ok().map(PathBuf::from),
        Some(PathBuf::from("ControlCenter_4.9.47.13")),
        // when running from target/debug
        Some(PathBuf::from("../../ControlCenter_4.9.47.13")),
        Some(PathBuf::from("../../../ControlCenter_4.9.47.13")),
    ];
    candidates
        .into_iter()
        .flatten()
        .find(|p| p.join("UserFanTables").is_dir() || p.join("RGBKeyboard.reg").is_file())
}

fn dmi_product() -> Option<String> {
    std::fs::read_to_string("/sys/class/dmi/id/product_name")
        .ok()
        .map(|s| s.trim().to_string())
}

fn model_dir(cli: &Cli, explicit: Option<String>) -> Result<PathBuf, String> {
    let oem = oem_dir(cli).ok_or("OEM package dir not found (use --oem-dir)")?;
    let model = explicit
        .or_else(|| dmi_product().and_then(|p| model_dir_for(&p)))
        .ok_or(
            "could not detect model — pass --model <PH4ARxx|...> \
             (see `ControlCenter_4.9.47.13/UserFanTables/`)",
        )?;
    let dir = oem.join("UserFanTables").join(&model);
    if !dir.is_dir() {
        return Err(format!("no fan tables for model '{model}' at {}", dir.display()));
    }
    Ok(dir)
}

// ---------------------------------------------------------------- commands

fn doctor(cli: &Cli) -> CmdResult {
    println!("ripple-nova / rcc doctor\n");

    let kernel_rel = std::fs::read_to_string("/proc/sys/kernel/osrelease")
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|_| "unknown".into());
    let kernel_ver = parse_version(&kernel_rel);

    println!("system:");
    println!("  kernel      : {kernel_rel}");
    if let Some((id, pretty)) = os_release() {
        println!("  distro      : {pretty} ({id})");
    }
    match dmi_product() {
        Some(p) => println!("  dmi product : {p}"),
        None => println!("  dmi product : (unreadable)"),
    }
    if let Ok(s) = std::fs::read_to_string("/sys/class/dmi/id/sys_vendor") {
        println!("  dmi vendor  : {}", s.trim());
    }

    let acpi_missing = || {
        if std::path::Path::new("/proc/acpi/call").exists() {
            "/proc/acpi/call exists but is not usable".to_string()
        } else {
            "/proc/acpi/call not present (acpi_call module not loaded)".to_string()
        }
    };
    let acpi_res = ripple_hw::linux::AcpiCallEc::probe()
        .map(|_| "available".to_string())
        .map_err(|e| {
            if std::path::Path::new("/proc/acpi/call").exists() {
                e.to_string()
            } else {
                acpi_missing()
            }
        });
    let wmi_res = ripple_hw::linux::WmiEc::probe()
        .map(|_| "available".to_string())
        .map_err(|_| acpi_missing());
    let sysfs_res = ripple_hw::linux::SysfsEc::probe()
        .map(|ec| format!("available ({})", ec.hwmon_dir().display()))
        .map_err(|_| "uniwill-laptop hwmon not found".to_string());
    let hid_res = probe_hid();

    println!("\nbackends:");
    for (name, res) in [
        ("acpi_call (ECRR/ECRW)", &acpi_res),
        ("wmi GetSetULong (WMBC)", &wmi_res),
        ("sysfs uniwill (>=6.19)", &sysfs_res),
        ("hidraw RGB keyboard", &hid_res),
    ] {
        match res {
            Ok(v) => println!("  {name:28}: {v}"),
            Err(e) => println!("  {name:28}: no — {e}"),
        }
    }

    // Actionable fix block: concrete install/load commands + URLs.
    let mut fixes: Vec<String> = Vec::new();
    if acpi_res.is_err() {
        fixes.push("acpi_call kernel module (backs BOTH the acpi_call and WMI EC paths):".into());
        match module_path("acpi_call") {
            Some(p) => fixes.push(format!("  already installed ({p}) → run: sudo modprobe acpi_call")),
            None => {
                match distro_family().as_deref() {
                    Some("arch") => fixes.push("  install:  sudo pacman -S acpi_call        # Arch (extra repo)".into()),
                    Some("debian") => fixes.push("  install:  sudo apt install acpi-call-dkms # Debian/Ubuntu".into()),
                    _ => fixes.push(
                        "  install:  git clone https://github.com/mkottman/acpi_call && cd acpi_call && make".into(),
                    ),
                }
                fixes.push("  load:     sudo modprobe acpi_call          # creates /proc/acpi/call".into());
            }
        }
        fixes.push("  source:   https://github.com/mkottman/acpi_call".into());
        fixes.push(String::new());
    }
    if sysfs_res.is_err() {
        fixes.push("uniwill-laptop kernel driver (hwmon temps/fans, charge limit, lightbar, hotkeys):".into());
        if module_loaded("uniwill-laptop") {
            fixes.push("  loaded, but no hwmon device → DMI whitelist may not cover this machine:".into());
            fixes.push("  try:     sudo modprobe uniwill-laptop force=1    # testing only".into());
        } else if let Some(p) = module_path("uniwill-laptop") {
            fixes.push(format!("  present ({p}) → run: sudo modprobe uniwill-laptop"));
            fixes.push("  no hwmon after load? → sudo modprobe uniwill-laptop force=1 (DMI whitelist)".into());
        } else if kernel_ver.is_some_and(|v| v < (6, 19)) {
            fixes.push(format!(
                "  kernel {kernel_rel} < 6.19 → driver is mainline since 6.19; upgrade the kernel"
            ));
            fixes.push("  or build out-of-tree: https://github.com/Wer-Wolf/uniwill-laptop".into());
        } else {
            fixes.push("  module missing on a ≥6.19 kernel → CONFIG_X86_PLATFORM_DRIVERS_UNIWILL not set?".into());
            fixes.push("  check:   grep UNIWILL /boot/config-$(uname -r)   # or: zgrep UNIWILL /proc/config.gz".into());
        }
        fixes.push("  docs:    https://docs.kernel.org/admin-guide/laptops/uniwill-laptop.html".into());
        fixes.push(String::new());
    }
    if let Err(e) = &hid_res {
        fixes.push("RGB keyboard — ITE HID solution (only if this machine has one):".into());
        fixes.push(format!("  {e}"));
        if hidraw_present() {
            fixes.push(
                "  /dev/hidraw* exists but no 048D device → likely the EC RGB solution (registers 1897-1902); no HID setup needed".into(),
            );
        } else {
            fixes.push(
                "  no /dev/hidraw* → likely the EC RGB solution (registers 1897-1902); no HID setup needed".into(),
            );
        }
        fixes.push(
            "  ITE protocol: https://github.com/rodgomesc/avell-unofficial-control-center".into(),
        );
        fixes.push(String::new());
    }
    let had_fixes = !fixes.is_empty();
    if had_fixes {
        println!("\nfix:");
        for line in fixes {
            println!("{line}");
        }
    }

    if had_fixes {
        println!("EC:");
    } else {
        println!("\nEC:");
    }
    let mut ec = open_ec(cli)?;
    println!("  backend     : {}", ec.name());
    if ec.name() == "mock" {
        println!("  (no real EC backend active — see the fix block above)");
    }
    if let Ok(pid) = ec.read(addr::PROJECT_ID) {
        let hi = ec.read(addr::PROJECT_ID + 1).unwrap_or(0);
        let id = (hi as u16) << 8 | pid as u16;
        println!("  project id  : {id} (0x{id:04x}) low-byte {pid} high-byte {hi}");
        println!("  (full id → model map: OEM Define.ProjectID)");
    }
    if let Ok(s1) = ec.read(addr::SUPPORT_BYTE1) {
        println!("  support[1]  : {s1:#010b}");
    }

    println!("\nOEM package:");
    match oem_dir(cli) {
        Some(d) => {
            println!("  root        : {}", d.display());
            if let Ok(dir) = model_dir(cli, None) {
                match list_profiles(&dir) {
                    Ok(p) => println!(
                        "  model       : {} profiles: {}",
                        dir.file_name().unwrap_or_default().to_string_lossy(),
                        p.join(", ")
                    ),
                    Err(e) => println!("  profiles    : error {e}"),
                }
            } else {
                println!("  model       : not detected (pass --model)");
            }
        }
        None => println!("  root        : not found (pass --oem-dir)"),
    }

    println!("\nRGB solution:");
    match &hid_res {
        Ok(v) => println!("  {v} → hidraw backend (not implemented yet)"),
        Err(e) => println!("  {e} → EC-solution RGB likely (registers 1897-1902)"),
    }

    Ok(())
}

/// Sysfs HID device dirs look like `0003:048D:8291.0003`
/// (bus:vendor:product.instance, optionally `hid:`-prefixed).
fn probe_hid() -> Result<String, String> {
    let mut total = 0usize;
    let mut found: Option<String> = None;
    if let Ok(rd) = std::fs::read_dir("/sys/bus/hid/devices") {
        for entry in rd.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            total += 1;
            let id = name.rsplit("hid:").next().unwrap_or("");
            let core = id.split('.').next().unwrap_or("");
            let mut parts = core.split(':');
            let _bus = parts.next();
            let vendor = parts.next().unwrap_or("");
            if vendor.eq_ignore_ascii_case("048D") {
                found = Some(name);
                break;
            }
        }
    }
    match found {
        Some(n) => Ok(format!("ITE (048D) controller present: {n}")),
        None if total > 0 => Err(format!("no ITE (048D) device among {total} HID device(s)")),
        None => Err("no HID devices found".into()),
    }
}

/// `"7.2.15-arch1-1"` → `Some((7, 2))`.
fn parse_version(s: &str) -> Option<(u32, u32)> {
    let mut it = s.split(|c: char| !c.is_ascii_digit());
    let major = it.next()?.parse().ok()?;
    let minor = it.next()?.parse().ok()?;
    Some((major, minor))
}

/// Read `/etc/os-release` → `(ID, PRETTY_NAME)`.
fn os_release() -> Option<(String, String)> {
    let text = std::fs::read_to_string("/etc/os-release").ok()?;
    let mut id = String::new();
    let mut pretty = String::new();
    for line in text.lines() {
        if let Some(v) = line.strip_prefix("ID=") {
            id = v.trim_matches('"').to_string();
        }
        if let Some(v) = line.strip_prefix("PRETTY_NAME=") {
            pretty = v.trim_matches('"').to_string();
        }
    }
    (!id.is_empty()).then_some((id, pretty))
}

/// Map the distro to a package-manager family for install hints.
fn distro_family() -> Option<String> {
    let text = std::fs::read_to_string("/etc/os-release").ok()?;
    let mut id = String::new();
    let mut like = String::new();
    for line in text.lines() {
        if let Some(v) = line.strip_prefix("ID=") {
            id = v.trim_matches('"').to_string();
        }
        if let Some(v) = line.strip_prefix("ID_LIKE=") {
            like = v.trim_matches('"').to_string();
        }
    }
    let all = format!("{id} {like}").to_lowercase();
    let toks: Vec<&str> = all.split_whitespace().collect();
    if toks.iter().any(|t| matches!(*t, "arch" | "archlinux" | "manjaro" | "endeavouros" | "cachyos")) {
        return Some("arch".into());
    }
    if toks.iter().any(|t| {
        matches!(*t, "debian" | "ubuntu" | "linuxmint" | "pop" | "raspbian" | "elementary")
    }) {
        return Some("debian".into());
    }
    Some(id)
}

/// `modinfo -n <name>` → module file path (works for builtin modules too).
fn module_path(name: &str) -> Option<String> {
    let out = std::process::Command::new("modinfo").arg("-n").arg(name).output().ok()?;
    if out.status.success() {
        let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if !s.is_empty() {
            return Some(s);
        }
    }
    None
}

/// Whether a module is currently loaded (checks `/sys/module`, dash/underscore agnostic).
fn module_loaded(name: &str) -> bool {
    let target = name.replace('-', "_");
    std::fs::read_dir("/sys/module")
        .map(|rd| {
            rd.flatten().any(|e| {
                e.file_name().to_string_lossy().replace('-', "_") == target
            })
        })
        .unwrap_or(false)
}

fn hidraw_present() -> bool {
    std::fs::read_dir("/dev")
        .map(|rd| {
            rd.flatten()
                .any(|e| e.file_name().to_string_lossy().starts_with("hidraw"))
        })
        .unwrap_or(false)
}

fn status(cli: &Cli) -> CmdResult {
    let mut ec = open_ec(cli)?;

    println!("mode:");
    if let Ok(b) = ec.read(addr::MAFAN_CONTROL_BYTE) {
        println!("  fan control byte: {b:#04x}");
    }
    if let Ok(pl1) = ec.read(addr::PL1_SETTING) {
        println!("  PL1 setting     : {pl1} W");
    }
    if let Ok(pl2) = ec.read(addr::PL2_SETTING) {
        println!("  PL2 setting     : {pl2} W");
    }

    println!("fans:");
    let l = ec.read(addr::MAIN_FAN_L_DUTY).unwrap_or(0);
    let r = ec.read(addr::MAIN_FAN_R_DUTY).unwrap_or(0);
    println!("  duty L/R        : {} % / {} %", duty_from_ec(l), duty_from_ec(r));
    let rpm1 = read_u16(&mut ec, addr::MAIN_FAN_RPM1, addr::MAIN_FAN_RPM2);
    let rpm2 = read_u16(&mut ec, addr::SECOND_FAN_RPM1, addr::SECOND_FAN_RPM2);
    println!("  rpm main/second : {rpm1} / {rpm2}");

    println!("battery:");
    if let Ok(up) = ec.read(addr::BATTERY_CHARGE_LIMIT_UP) {
        println!("  charge limit    : {up} %");
    }

    // Fan table summary (read-only, from OEM files)
    if let Ok(dir) = model_dir(cli, None) {
        println!("fan tables ({}):", dir.display());
        if let Ok(profiles) = list_profiles(&dir) {
            for p in profiles {
                if let Ok(t) = FanTable::load(dir.join(format!("{p}.json"))) {
                    println!(
                        "  {:5} PL1={:>4} PL2={:>4} TCC={:>3} activated={}",
                        t.Name,
                        t.pl1().map(|v| v.to_string()).unwrap_or_else(|| "NA".into()),
                        t.pl2().map(|v| v.to_string()).unwrap_or_else(|| "NA".into()),
                        t.TCC.as_deref().unwrap_or("-"),
                        t.Activated
                    );
                }
            }
        }
    }

    if let Some(oem) = oem_dir(cli) {
        let reg = oem.join("RGBKeyboard.reg");
        if reg.is_file() {
            if let Ok(state) = RgbRegState::load(&reg) {
                println!("rgb (saved state):");
                for (k, v) in &state.globals {
                    println!("  {k:22} = {v}");
                }
                for (zone, effects) in &state.zones {
                    println!("  zone {zone}: {} saved effects", effects.len());
                }
            }
        }
    }

    Ok(())
}

fn read_u16(ec: &mut Box<dyn EcAccess>, lo: u16, hi: u16) -> u16 {
    let a = ec.read(lo).unwrap_or(0) as u16;
    let b = ec.read(hi).unwrap_or(0) as u16;
    (b << 8) | a
}

fn monitor(_cli: &Cli, interval: f64) -> CmdResult {
    let mut ec = open_ec(_cli)?;
    println!(
        "{:<8} {:>8} {:>8} {:>8}",
        "t(s)", "dutyL%", "rpm1", "rpm2"
    );
    let mut t = 0.0f64;
    loop {
        let l = ec.read(addr::MAIN_FAN_L_DUTY).unwrap_or(0);
        let rpm1 = read_u16(&mut ec, addr::MAIN_FAN_RPM1, addr::MAIN_FAN_RPM2);
        let rpm2 = read_u16(&mut ec, addr::SECOND_FAN_RPM1, addr::SECOND_FAN_RPM2);
        println!("{t:<8.1} {:>8} {:>8} {:>8}", duty_from_ec(l), rpm1, rpm2);
        std::thread::sleep(std::time::Duration::from_secs_f64(interval.max(0.2)));
        t += interval;
        // the mock backend never changes; Ctrl-C to stop.
    }
}

fn mode(cli: &Cli, cmd: &ModeCmd) -> CmdResult {
    match cmd {
        ModeCmd::Get => {
            let mut ec = open_ec(cli)?;
            let b = ec.read(addr::MAFAN_CONTROL_BYTE)?;
            println!("fan control byte: {b:#04x} ({})", fan_mode_name(b));
            Ok(())
        }
        ModeCmd::Set { mode } => {
            let m: OperatingMode = mode
                .parse()
                .map_err(|e: String| e)?;
            let mut ec = open_ec(cli)?;
            // OEM writes the mode's default PL/TCC registers then flips the
            // active profile; the full sequence is not implemented yet.
            let (pl1, pl2, tcc) = match m {
                OperatingMode::Office => (addr::OFFICE_PL1_DEFAULT, addr::OFFICE_PL2_DEFAULT, addr::OFFICE_TCC_OFFSET_DEFAULT),
                OperatingMode::Gaming => (addr::GAMING_PL1_DEFAULT, addr::GAMING_PL2_DEFAULT, addr::GAMING_TCC_OFFSET_DEFAULT),
                OperatingMode::Turbo => (addr::GAMING_PL1_DEFAULT, addr::GAMING_PL2_DEFAULT, addr::TURBO_TCC_OFFSET_DEFAULT),
                OperatingMode::Benchmark => (addr::GAMING_PL1_DEFAULT, addr::GAMING_PL2_DEFAULT, addr::TURBO_TCC_OFFSET_DEFAULT),
            };
            let d1 = ec.read(pl1)?;
            let d2 = ec.read(pl2)?;
            let t = ec.read(tcc)?;
            println!(
                "set mode {} → PL1={d1}W PL2={d2}W TCC offset={t}°C (backend: {})",
                m.as_str(),
                ec.name()
            );
            ec.write(addr::PL1_SETTING, d1)?;
            ec.write(addr::PL2_SETTING, d2)?;
            Ok(())
        }
    }
}

fn fan_mode_name(b: u8) -> &'static str {
    match b {
        0x00 => "normal",
        0x10 => "turbo",
        0x40 => "fan-boost",
        0x80 => "user",
        0x81..=0x85 => "user-level",
        0xA0 => "user-himode",
        _ => "unknown",
    }
}

fn fan(cli: &Cli, cmd: &FanCmd) -> CmdResult {
    match cmd {
        FanCmd::Curve { cmd } => match cmd {
            CurveCmd::List { model } => {
                let dir = model_dir(cli, model.clone())?;
                let profiles = list_profiles(&dir)?;
                println!("profiles in {}:", dir.display());
                for p in profiles {
                    if let Ok(t) = FanTable::load(dir.join(format!("{p}.json"))) {
                        println!(
                            "  {:5} activated={} cpu[0..3]={:?}",
                            t.Name,
                            t.Activated,
                            t.CPU.iter()
                                .take(4)
                                .map(|c| (c.up_t, c.down_t, c.duty))
                                .collect::<Vec<_>>()
                        );
                    } else {
                        println!("  {p} (unparseable)");
                    }
                }
                Ok(())
            }
            CurveCmd::Show { profile, model } => {
                let dir = model_dir(cli, model.clone())?;
                let t = FanTable::load(dir.join(format!("{profile}.json")))?;
                println!("{}", serde_json::to_string_pretty(&t).map_err(|e| e.to_string())?);
                Ok(())
            }
            CurveCmd::File { path } => {
                let t = FanTable::load(&path)?;
                println!("{}", serde_json::to_string_pretty(&t).map_err(|e| e.to_string())?);
                Ok(())
            }
        },
        FanCmd::Boost { state } => {
            let flag = match state.to_ascii_lowercase().as_str() {
                "on" | "true" | "1" => FanControlByte::FanBoost.to_byte(),
                "off" | "false" | "0" => FanControlByte::Normal.to_byte(),
                other => return Err(format!("boost must be on/off, got '{other}'")),
            };
            let mut ec = open_ec(cli)?;
            let cur = ec.read(addr::MAFAN_CONTROL_BYTE)?;
            // Preserve non-mode bits (read-before-write safety rule).
            let new = (cur & !0xF0) | (flag & 0xF0);
            ec.write_verified(addr::MAFAN_CONTROL_BYTE, new)?;
            println!("fan control byte {cur:#04x} → {new:#04x} ({})", fan_mode_name(new));
            Ok(())
        }
        FanCmd::Level { level } => {
            if *level > 5 {
                return Err("level must be 0..=5".into());
            }
            let flag = if *level == 0 {
                FanControlByte::Normal.to_byte()
            } else {
                FanControlByte::UserLevel(*level).to_byte()
            };
            let mut ec = open_ec(cli)?;
            let cur = ec.read(addr::MAFAN_CONTROL_BYTE)?;
            let new = (cur & !0xF0) | flag;
            ec.write_verified(addr::MAFAN_CONTROL_BYTE, new)?;
            println!("fan control byte → {new:#04x} ({})", fan_mode_name(new));
            Ok(())
        }
    }
}

fn power(cli: &Cli, cmd: &PowerCmd) -> CmdResult {
    let mut ec = open_ec(cli)?;
    match cmd {
        PowerCmd::Get => {
            println!("PL1 = {} W", ec.read(addr::PL1_SETTING)?);
            println!("PL2 = {} W", ec.read(addr::PL2_SETTING)?);
            println!("PL4 = {} W", ec.read(addr::PL4_SETTING)?);
            Ok(())
        }
        PowerCmd::Pl1 { watts } => {
            // Range guard: EC PL fields are single-byte; sane laptop range.
            if !(5..=150).contains(watts) {
                return Err(format!("PL1 {watts} W out of range 5..=150 (use --force later)"));
            }
            ec.write_verified(addr::PL1_SETTING, *watts as u8)?;
            println!("PL1 → {watts} W (readback {} W)", ec.read(addr::PL1_SETTING)?);
            Ok(())
        }
        PowerCmd::Pl2 { watts } => {
            if !(5..=150).contains(watts) {
                return Err(format!("PL2 {watts} W out of range 5..=150"));
            }
            ec.write_verified(addr::PL2_SETTING, *watts as u8)?;
            println!("PL2 → {watts} W (readback {} W)", ec.read(addr::PL2_SETTING)?);
            Ok(())
        }
        PowerCmd::Tcc { offset } => {
            if !(-30..=0).contains(offset) {
                return Err(format!("TCC offset {offset} out of range -30..=0"));
            }
            // Not written to the mode-specific default register yet; for now
            // record intent and write the gaming default as a stand-in.
            println!("TCC offset {offset}°C — writing it is not implemented yet");
            let _ = ec.read(addr::GAMING_TCC_OFFSET_DEFAULT)?;
            Ok(())
        }
    }
}

fn rgb(cli: &Cli, cmd: &RgbCmd) -> CmdResult {
    match cmd {
        RgbCmd::State => {
            let oem = oem_dir(cli).ok_or("OEM package dir not found (use --oem-dir)")?;
            let reg = oem.join("RGBKeyboard.reg");
            if !reg.is_file() {
                return Err(format!("{} not found", reg.display()));
            }
            let state = RgbRegState::load(&reg)?;
            println!("globals:");
            for (k, v) in &state.globals {
                println!("  {k:24} = {v}");
            }
            for (zone, effects) in &state.zones {
                println!("zone {zone}:");
                for (name, v) in effects {
                    let e = match v {
                        ripple_core::rgb::ZoneValue::Effect(e) => e,
                        ripple_core::rgb::ZoneValue::Raw(_) => {
                            println!("  {name:24} (raw JSON, not an effect)");
                            continue;
                        }
                    };
                    let fx = ripple_core::rgb::Effect::from_id(e.save_effect)
                        .map(|f| f.name())
                        .unwrap_or("?");
                    let cols = e.save_layout_color.as_ref().map(|c| {
                        c.ColorBuffer
                            .iter()
                            .map(|b| format!("{:02x}{:02x}{:02x}", b.R, b.G, b.B))
                            .collect::<Vec<_>>()
                            .join(" ")
                    });
                    println!(
                        "  {name:24} effect={fx} speed={} dir={} colors={}",
                        e.save_speed,
                        e.save_direction,
                        cols.as_deref().unwrap_or("-")
                    );
                }
            }
            Ok(())
        }
        RgbCmd::Effects => {
            use ripple_core::rgb::Effect;
            const ALL: [Effect; 25] = [
                Effect::Single, Effect::Breathing, Effect::Wave, Effect::Reactive,
                Effect::Rainbow, Effect::Ripple, Effect::Raindrop, Effect::Neon,
                Effect::Marquee, Effect::Stack, Effect::Impact, Effect::Spark,
                Effect::Aurora, Effect::Music, Effect::UserMode, Effect::Gaming,
                Effect::Flash, Effect::Mix, Effect::RippleO, Effect::Alphabet,
                Effect::StarHitting, Effect::StarSpark, Effect::Thinking,
                Effect::Manual, Effect::BatteryPercent,
            ];
            for e in ALL.iter() {
                println!("{:>2}  {}", *e as u8, e.name());
            }
            Ok(())
        }
        RgbCmd::Color { color } => {
            if color.eq_ignore_ascii_case("off") {
                println!("rgb off — write path is not implemented yet (EC 1897-1902 or ITE hidraw)");
                return Ok(());
            }
            let hex = color.trim_start_matches('#');
            if hex.len() != 6 || !hex.chars().all(|c| c.is_ascii_hexdigit()) {
                return Err(format!("expected 6 hex digits or 'off', got '{color}'"));
            }
            let r = u8::from_str_radix(&hex[0..2], 16).unwrap();
            let g = u8::from_str_radix(&hex[2..4], 16).unwrap();
            let b = u8::from_str_radix(&hex[4..6], 16).unwrap();
            println!("rgb color #{hex} (R={r} G={g} B={b}) — write path is not implemented yet");
            Ok(())
        }
    }
}

fn battery(cli: &Cli, cmd: &BatteryCmd) -> CmdResult {
    let mut ec = open_ec(cli)?;
    match cmd {
        BatteryCmd::Get => {
            println!("charge limit (up)  : {} %", ec.read(addr::BATTERY_CHARGE_LIMIT_UP)?);
            println!("charge limit (down): {} %", ec.read(addr::BATTERY_CHARGE_LIMIT_DOWN)?);
            Ok(())
        }
        BatteryCmd::ChargeLimit { percent } => {
            if *percent > 100 {
                return Err("percent must be 0..=100".into());
            }
            ec.write_verified(addr::BATTERY_CHARGE_LIMIT_UP, *percent)?;
            println!("charge limit → {percent} %");
            println!("note: kernel attr /sys/class/power_supply/BAT*/charge_control_end_threshold preferred when present");
            Ok(())
        }
        BatteryCmd::Profile { profile } => {
            let p: ProtectionStatus = profile.parse().map_err(|e: String| e)?;
            println!(
                "battery protection profile → {} (EC write path is not implemented yet)",
                p.as_str()
            );
            Ok(())
        }
    }
}

fn display(_cli: &Cli, cmd: &DisplayCmd) -> CmdResult {
    match cmd {
        DisplayCmd::Refresh => {
            for tool in ["kscreen-doctor", "xrandr", "wlr-randr"] {
                let found = std::process::Command::new("which")
                    .arg(tool)
                    .output()
                    .map(|o| o.status.success())
                    .unwrap_or(false);
                println!("  {tool:14}: {}", if found { "found" } else { "missing" });
            }
            println!("(rate listing is not implemented yet)");
            Ok(())
        }
        DisplayCmd::Dgpu { state } => {
            match state.to_ascii_lowercase().as_str() {
                "on" | "off" => {}
                other => return Err(format!("dgpu must be on/off, got '{other}'")),
            }
            println!("dGPU {state} — ACPI _OFF path is not implemented yet");
            Ok(())
        }
    }
}
