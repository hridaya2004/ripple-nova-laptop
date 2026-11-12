//! Command implementations.

use std::path::PathBuf;

use ripple_core::fan::{list_profiles, model_dir_for, FanTable};
use ripple_core::modes::{FanControlByte, OperatingMode};
use ripple_core::regs::{addr, duty_from_ec};
use ripple_core::rgb::RgbRegState;
use ripple_hw::{EcAccess, HwError};

use crate::{
    Backend, BatteryCmd, Cli, CurveCmd, DisplayCmd, EcCmd, FanCmd, ModeChoice, ModeCmd, PowerCmd,
    ProfileChoice, RgbCmd, SystemCmd,
};

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
        crate::Command::System { cmd } => system(cli, cmd),
        crate::Command::Ec { cmd } => ec_cmd(cli, cmd),
    }
}

// ---------------------------------------------------------------- backends

fn open_ec(cli: &Cli) -> Result<Box<dyn EcAccess>, String> {
    match cli.backend {
        Backend::Mock => Ok(Box::new(ripple_hw::mock::samples::ph4())),
        Backend::Acpi => Ok(Box::new(
            ripple_hw::linux::AcpiCallEc::probe().map_err(err)?,
        )),
        Backend::Wmi => Ok(Box::new(
            ripple_hw::linux::WmiEc::probe().map_err(err)?,
        )),
        Backend::Sysfs => Ok(Box::new(
            ripple_hw::linux::SysfsEc::probe().map_err(err)?,
        )),
        Backend::Auto => {
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
    }
}

/// The kernel `uniwill-laptop` sysfs interface (sensors/lightbar/charge/attrs).
/// Independent of the EC backend; hidden entirely under `--backend mock`.
fn uniwill(cli: &Cli) -> Result<ripple_hw::linux::SysfsEc, String> {
    if matches!(cli.backend, Backend::Mock) {
        return Err("sysfs hidden by --backend mock".into());
    }
    ripple_hw::linux::SysfsEc::probe().map_err(err)
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
        .map(|ec| format!("available ({}.ECRR/.ECRW)", ec.device()))
        .map_err(|e| {
            if std::path::Path::new("/proc/acpi/call").exists() {
                e.to_string()
            } else {
                acpi_missing()
            }
        });
    let wmi_res = ripple_hw::linux::WmiEc::probe()
        .map(|ec| format!("available ({})", ec.wmbc()))
        .map_err(|e| {
            if std::path::Path::new("/proc/acpi/call").exists() {
                e.to_string()
            } else {
                acpi_missing()
            }
        });
    let sysfs_res = ripple_hw::linux::SysfsEc::probe()
        .map(|ec| format!("available ({})", ec.summary()))
        .map_err(err);
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
    if let Err(e) = &acpi_res {
        fixes.push("acpi_call kernel module (backs BOTH the acpi_call and WMI EC paths):".into());
        fixes.push(format!("  error: {e}"));
        if e.contains("sudo") || e.contains("permission") {
            fixes.push("  → run rcc with sudo (writing /proc/acpi/call requires root)".into());
        } else if e.contains("ECRR") {
            fixes.push("  → method discovery failed: no device on this DSDT answered to <path>.ECRR".into());
            fixes.push("    (the paths actually tried are listed in the error above)".into());
        }
        match module_path("acpi_call") {
            Some(p) if !std::path::Path::new("/proc/acpi/call").exists() => {
                fixes.push(format!(
                    "  installed ({p}) but not loaded → run: sudo modprobe acpi_call"
                ));
            }
            Some(_) => {}
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
        println!("  fan control byte: {b:#04x} ({})", fan_mode_name(b));
    }
    if let Ok(pl1) = ec.read(addr::PL1_SETTING) {
        println!("  PL1 setting     : {pl1} W");
    }
    if let Ok(pl2) = ec.read(addr::PL2_SETTING) {
        println!("  PL2 setting     : {pl2} W");
    }

    // Sensors: kernel uniwill hwmon first (real hardware), EC fallback.
    let sysfs = uniwill(cli).ok();
    match sysfs.as_ref().and_then(|u| u.sensors().ok()) {
        Some(s) => {
            println!("sensors (uniwill hwmon):");
            for (l, mv) in &s.temps {
                println!("  temp {l:8}: {:5.1} °C", *mv as f64 / 1000.0);
            }
            for (i, (l, rpm)) in s.fans.iter().enumerate() {
                let pwm = match s.pwms.get(i) {
                    Some(p) => format!("pwm {p:>3} ({:2.0} %)", *p as f64 * 100.0 / 255.0),
                    None => String::new(),
                };
                println!("  fan  {l:8}: {rpm:>5} rpm  {pwm}");
            }
        }
        None => {
            println!("fans (EC):");
            let l = ec.read(addr::MAIN_FAN_L_DUTY).unwrap_or(0);
            let r = ec.read(addr::MAIN_FAN_R_DUTY).unwrap_or(0);
            println!(
                "  duty L/R        : {} % / {} %",
                duty_from_ec(l),
                duty_from_ec(r)
            );
            let rpm1 = read_tach(&mut ec, addr::MAIN_FAN_RPM1, addr::MAIN_FAN_RPM2);
            let rpm2 = read_tach(&mut ec, addr::SECOND_FAN_RPM1, addr::SECOND_FAN_RPM2);
            println!("  rpm main/second : {rpm1} / {rpm2}");
        }
    }

    println!("battery:");
    if let Ok(up) = ec.read(addr::BATTERY_CHARGE_LIMIT_UP) {
        println!("  charge limit    : {up} %");
    }
    if let Some(u) = &sysfs {
        if let Ok((sel, all)) = u.charge_types() {
            println!("  charge profile  : {sel} (available: {})", all.join(", "));
        }
        if let Ok(bat) = std::fs::read_to_string("/sys/class/power_supply/BAT0/status") {
            println!("  status          : {}", bat.trim());
        }
        if let Ok(h) = std::fs::read_to_string("/sys/class/power_supply/BAT0/health") {
            println!("  health          : {}", h.trim());
        }
    }

    if let Some(u) = &sysfs {
        if let Ok((r, g, b, bri)) = u.lightbar() {
            println!("lightbar:");
            println!("  color           : #{r:02x}{g:02x}{b:02x}  brightness {bri}");
        }
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

/// EC tach registers are big-endian as listed in the OEM map:
/// main = (BYTE1@1124 << 8) | BYTE2@1125, second = (BYTE1@1132 << 8) | BYTE2@1131.
/// Verified against hwmon: 0x07E2 = 2018 rpm = fan1_input (2026-11-12).
fn read_tach(ec: &mut Box<dyn EcAccess>, hi_reg: u16, lo_reg: u16) -> u16 {
    let hi = ec.read(hi_reg).unwrap_or(0) as u16;
    let lo = ec.read(lo_reg).unwrap_or(0) as u16;
    (hi << 8) | lo
}

fn monitor(cli: &Cli, interval: f64) -> CmdResult {
    let sleep = std::time::Duration::from_secs_f64(interval.max(0.2));

    // Preferred: kernel hwmon (real temps + fans, no sudo needed).
    if let Ok(u) = uniwill(cli) {
        println!(
            "{:<8} {:>8} {:>8} {:>8} {:>8} {:>6}",
            "t(s)", "cpu°C", "gpu°C", "rpm1", "rpm2", "pwm1"
        );
        let mut t = 0.0f64;
        loop {
            if let Ok(s) = u.sensors() {
                let cpu = s.temps.first().map(|(_, v)| *v as f64 / 1000.0);
                let gpu = s.temps.get(1).map(|(_, v)| *v as f64 / 1000.0);
                let f1 = s.fans.first().map(|(_, v)| *v).unwrap_or(0);
                let f2 = s.fans.get(1).map(|(_, v)| *v).unwrap_or(0);
                let pwm = s.pwms.first().copied().unwrap_or(0);
                println!(
                    "{t:<8.1} {:>8} {:>8} {:>8} {:>8} {:>6}",
                    cpu.map(|v| format!("{v:.1}")).unwrap_or_else(|| "-".into()),
                    gpu.map(|v| format!("{v:.1}")).unwrap_or_else(|| "-".into()),
                    f1,
                    f2,
                    pwm
                );
            }
            std::thread::sleep(sleep);
            t += interval;
        }
    }

    // Fallback: EC registers (acpi/wmi/mock).
    let mut ec = open_ec(cli)?;
    println!("{:<8} {:>8} {:>8} {:>8}", "t(s)", "dutyL%", "rpm1", "rpm2");
    let mut t = 0.0f64;
    loop {
        let l = ec.read(addr::MAIN_FAN_L_DUTY).unwrap_or(0);
        let rpm1 = read_tach(&mut ec, addr::MAIN_FAN_RPM1, addr::MAIN_FAN_RPM2);
        let rpm2 = read_tach(&mut ec, addr::SECOND_FAN_RPM1, addr::SECOND_FAN_RPM2);
        println!("{t:<8.1} {:>8} {:>8} {:>8}", duty_from_ec(l), rpm1, rpm2);
        std::thread::sleep(sleep);
        t += interval;
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
            let m: OperatingMode = match mode {
                ModeChoice::Office => OperatingMode::Office,
                ModeChoice::Gaming => OperatingMode::Gaming,
                ModeChoice::Turbo => OperatingMode::Turbo,
                ModeChoice::Benchmark => OperatingMode::Benchmark,
            };
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
            let flag = if state.as_bool() {
                FanControlByte::FanBoost.to_byte()
            } else {
                FanControlByte::Normal.to_byte()
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
            let (r, g, b) = parse_hex_color(color)?;
            println!("rgb color (R={r} G={g} B={b}) — keyboard write path is not implemented yet");
            Ok(())
        }
        RgbCmd::Lightbar { color } => {
            let u = uniwill(cli)?;
            if color.eq_ignore_ascii_case("off") {
                u.set_lightbar(None)?;
                println!("lightbar off (brightness 0)");
            } else {
                let rgb = if color.eq_ignore_ascii_case("on") {
                    (255, 255, 255)
                } else {
                    parse_hex_color(color)?
                };
                u.set_lightbar(Some(rgb))?;
                let (r, g, b, bri) = u.lightbar()?;
                println!("lightbar → #{r:02x}{g:02x}{b:02x} (brightness {bri})");
            }
            Ok(())
        }
    }
}

/// `ff8800` / `#ff8800` → (r, g, b) with a clap-friendly error message.
fn parse_hex_color(color: &str) -> Result<(u8, u8, u8), String> {
    let hex = color.trim_start_matches('#');
    if hex.len() != 6 || !hex.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(format!(
            "expected 6 hex digits (e.g. ff8800) or 'off', got '{color}'"
        ));
    }
    Ok((
        u8::from_str_radix(&hex[0..2], 16).unwrap(),
        u8::from_str_radix(&hex[2..4], 16).unwrap(),
        u8::from_str_radix(&hex[4..6], 16).unwrap(),
    ))
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
            // OEM names map onto the kernel charge_types interface.
            let want = match profile {
                ProfileChoice::Performance | ProfileChoice::Standard => "Standard",
                ProfileChoice::Balanced | ProfileChoice::Trickle => "Trickle",
                ProfileChoice::Healthy | ProfileChoice::LongLife => "Long_Life",
            };
            let u = uniwill(cli).map_err(|e| {
                format!("charge profiles need the uniwill kernel driver: {e}")
            })?;
            let (_sel, all) = u.charge_types()?;
            u.set_charge_types(want)?;
            println!("charge profile → {want} (available: {})", all.join(", "));
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
            // Deliberately NOT executed automatically — ACPI _OFF on dGPU is
            // risky (needs a verified _OFF/_ON pair).
            println!(
                "dGPU power {} — ACPI _OFF path is not wired yet",
                if state.as_bool() { "on" } else { "off" }
            );
            Ok(())
        }
    }
}

// ------------------------------------------------------------------ system

/// Kernel platform attributes (`rcc system`).
fn system(cli: &Cli, cmd: &Option<SystemCmd>) -> CmdResult {
    let u = uniwill(cli).map_err(|e| format!("platform attrs need the uniwill driver: {e}"))?;

    match cmd {
        None | Some(SystemCmd::Show { name: None }) => {
            println!("platform attributes:");
            let mut shown = 0;
            for (n, v) in u.platform_attrs() {
                match v {
                    Some(v) => {
                        println!("  {n:24} = {v}");
                        shown += 1;
                    }
                    None => println!("  {n:24} - (not exposed by this kernel/device)"),
                }
            }
            if shown > 0 {
                println!("\nset: rcc system set <name> <value>  (e.g. fn-lock on)");
            }
            Ok(())
        }
        Some(SystemCmd::Show { name: Some(name) }) => {
            let n = canonical_attr(name)?;
            println!("{n} = {}", u.get_attr(&n)?);
            Ok(())
        }
        Some(SystemCmd::Set { name, value }) => {
            let n = canonical_attr(name)?;
            let new = u.set_attr(&n, value).map_err(|e| {
                let allowed = attr_value_hint(&n);
                if allowed.is_empty() {
                    e.to_string()
                } else {
                    format!("{e} — {n} accepts: {allowed}")
                }
            })?;
            println!("{n} → {new}");
            Ok(())
        }
    }
}

/// Accept `fn-lock` / `fnlock` / `fn_lock` spellings → ABI name.
fn canonical_attr(input: &str) -> Result<&'static str, String> {
    let norm: String = input
        .to_ascii_lowercase()
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .collect();
    let hit = [
        ("fnlock", "fn_lock"),
        ("superkey", "super_key_enable"),
        ("super", "super_key_enable"),
        ("touchpad", "touchpad_toggle_enable"),
        ("touchpadtoggle", "touchpad_toggle_enable"),
        ("rainbow", "rainbow_animation"),
        ("breathing", "breathing_in_suspend"),
        ("breathingsuspend", "breathing_in_suspend"),
        ("usbc", "usb_c_power_priority"),
        ("usbcpriority", "usb_c_power_priority"),
        ("ctgp", "ctgp_offset"),
        ("tgp", "ctgp_offset"),
        ("acautoboot", "ac_auto_boot"),
        ("powershare", "usb_powershare_high"),
        ("usbpowershare", "usb_powershare_high"),
    ]
    .iter()
    .find(|(k, _)| *k == norm)
    .map(|(_, v)| *v)
    .or_else(|| {
        ripple_hw::linux::PLATFORM_ATTRS
            .iter()
            .find(|a| a.replace('_', "") == norm)
            .copied()
    });
    hit.ok_or_else(|| {
        format!(
            "unknown attribute '{input}' (known: {})",
            ripple_hw::linux::PLATFORM_ATTRS.join(", ")
        )
    })
}

/// Legal values per attribute, for error messages.
fn attr_value_hint(name: &str) -> &'static str {
    match name {
        "usb_c_power_priority" => "charging | performance",
        "ctgp_offset" => "integer watts (GPU power, use with care)",
        _ => "on | off",
    }
}

// ---------------------------------------------------------------------- ec

/// `1873` / `0x751` → u16.
fn parse_addr(s: &str) -> Result<u16, String> {
    let t = s.trim();
    let v = if let Some(h) = t.strip_prefix("0x").or_else(|| t.strip_prefix("0X")) {
        u16::from_str_radix(h, 16).map_err(|e| format!("bad address '{s}': {e}"))?
    } else {
        t.parse::<u16>().map_err(|e| format!("bad address '{s}': {e}"))?
    };
    Ok(v)
}

fn ec_cmd(cli: &Cli, cmd: &EcCmd) -> CmdResult {
    match cmd {
        EcCmd::Read { addr } => {
            let a = parse_addr(addr)?;
            let mut ec = open_ec(cli)?;
            let v = ec.read(a)?;
            println!("{a} (0x{:03x}) = 0x{v:02x} ({v})", a);
            Ok(())
        }
        EcCmd::Write { addr, value, force } => {
            let a = parse_addr(addr)?;
            let v = if let Some(h) = value
                .trim()
                .strip_prefix("0x")
                .or_else(|| value.trim().strip_prefix("0X"))
            {
                u8::from_str_radix(h, 16).map_err(|e| format!("bad value '{value}': {e}"))?
            } else {
                value
                    .trim()
                    .parse::<u8>()
                    .map_err(|e| format!("bad value '{value}': {e}"))?
            };
            if !ripple_core::regs::is_known(a) && !*force {
                return Err(format!(
                    "address {a} (0x{a:03x}) is outside the known register map \
                     — re-run with --force if you mean it"
                ));
            }
            let mut ec = open_ec(cli)?;
            let old = ec.read(a)?; // read-before-write (safety rule 1)
            ec.write_verified(a, v)?;
            let back = ec.read(a)?;
            println!(
                "{a} (0x{a:03x}): 0x{old:02x} → 0x{v:02x} (readback 0x{back:02x}, {})",
                ec.name()
            );
            Ok(())
        }
    }
}
