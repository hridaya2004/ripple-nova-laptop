//! Command implementations.

use std::path::PathBuf;

use ripple_core::fan::{list_profiles, FanTable};
use ripple_core::modes::OperatingMode;
use ripple_core::regs::{addr, duty_from_ec};
use ripple_core::rgb::RgbRegState;
use ripple_hw::control::{self, Config, Hsv, Preset, Profile};
use ripple_hw::{EcAccess, HwError, HwResult};

use crate::{
    Backend, BatteryCmd, Cli, CurveCmd, CustomFanCmd, EcCmd, FanCmd, GpuCmd, ModeChoice, ModeCmd,
    PowerCmd, ProfileChoice, RgbCmd, SystemCmd,
};

pub type CmdResult = Result<(), String>;

pub fn run(cli: &Cli) -> CmdResult {
    match &cli.command {
        crate::Command::Doctor => doctor(cli),
        crate::Command::Status => status(cli),
        crate::Command::Monitor { interval } => monitor(cli, *interval),
        crate::Command::Mode { cmd } => mode(cli, cmd),
        crate::Command::Watch => watch(cli),
        crate::Command::Fan { cmd } => fan(cli, cmd),
        crate::Command::Power { cmd } => power(cli, cmd),
        crate::Command::Rgb { cmd } => rgb(cli, cmd),
        crate::Command::Battery { cmd } => battery(cli, cmd),
        crate::Command::System { cmd } => system(cli, cmd),
        crate::Command::Touchpad { state } => touchpad(*state),
        crate::Command::Popups { state } => popups(*state),
        crate::Command::Gpu { cmd } => gpu(cmd.as_ref().unwrap_or(&crate::GpuCmd::Show)),
        crate::Command::Ec { cmd } => ec_cmd(cli, cmd),
    }
}

// ---------------------------------------------------------------- backends

fn open_ec(cli: &Cli) -> Result<Box<dyn EcAccess>, String> {
    match cli.backend {
        Backend::Acpi => Ok(Box::new(
            ripple_hw::linux::AcpiCallEc::probe().map_err(err)?,
        )),
        Backend::Wmi => Ok(Box::new(ripple_hw::linux::WmiEc::probe().map_err(err)?)),
        Backend::Sysfs => Ok(Box::new(ripple_hw::linux::SysfsEc::probe().map_err(err)?)),
        Backend::Auto => ripple_hw::linux::open_auto().map_err(err),
    }
}

/// The kernel `uniwill-laptop` sysfs interface (sensors/lightbar/charge/attrs).
/// Independent of the EC backend.
fn uniwill(_cli: &Cli) -> Result<ripple_hw::linux::SysfsEc, String> {
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

fn detected_model(cli: &Cli) -> ripple_core::model::Model {
    let mut ec = open_ec(cli).ok();
    ripple_hw::linux::detect_model(ec.as_deref_mut())
}

fn model_dir(cli: &Cli, explicit: Option<String>) -> Result<PathBuf, String> {
    let oem = oem_dir(cli).ok_or("OEM package dir not found (use --oem-dir)")?;
    let model = match explicit {
        Some(m) => m,
        None => {
            let m = detected_model(cli);
            m.name
                .ok_or_else(|| format!("model not recognised ({m}) — pass --model <PH4ARxx|...>"))?
                .to_string()
        }
    };
    let dir = oem.join("UserFanTables").join(&model);
    if !dir.is_dir() {
        return Err(format!(
            "no OEM fan tables for model '{model}' at {}",
            dir.display()
        ));
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
    // The ITE HID keyboard is only a question when the EC does not drive the backlight itself.
    let ec_rgb = open_ec(cli).is_ok_and(|mut ec| control::has_keyboard_rgb(ec.as_mut()));
    let hid_res = match probe_hid() {
        Err(_) if ec_rgb => {
            Ok("not needed: the EC drives the keyboard backlight (EC 1897-1899)".to_string())
        }
        r => r,
    };

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
            fixes.push(
                "  → method discovery failed: no device on this DSDT answered to <path>.ECRR"
                    .into(),
            );
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
                fixes.push(
                    "  load:     sudo modprobe acpi_call          # creates /proc/acpi/call".into(),
                );
            }
        }
        fixes.push("  source:   https://github.com/mkottman/acpi_call".into());
        fixes.push(String::new());
    }
    if sysfs_res.is_err() {
        fixes.push(
            "uniwill-laptop kernel driver (hwmon temps/fans, charge limit, lightbar, hotkeys):"
                .into(),
        );
        if module_loaded("uniwill-laptop") {
            fixes.push(
                "  loaded, but no hwmon device → DMI whitelist may not cover this machine:".into(),
            );
            fixes.push("  try:     sudo modprobe uniwill-laptop force=1    # testing only".into());
        } else if let Some(p) = module_path("uniwill-laptop") {
            fixes.push(format!(
                "  present ({p}) → run: sudo modprobe uniwill-laptop"
            ));
            fixes.push(
                "  no hwmon after load? → sudo modprobe uniwill-laptop force=1 (DMI whitelist)"
                    .into(),
            );
        } else if kernel_ver.is_some_and(|v| v < (6, 19)) {
            fixes.push(format!(
                "  kernel {kernel_rel} < 6.19 → driver is mainline since 6.19; upgrade the kernel"
            ));
            fixes.push("  or build out-of-tree: https://github.com/Wer-Wolf/uniwill-laptop".into());
        } else {
            fixes.push(
                "  module missing on a ≥6.19 kernel → CONFIG_X86_PLATFORM_DRIVERS_UNIWILL not set?"
                    .into(),
            );
            fixes.push("  check:   grep UNIWILL /boot/config-$(uname -r)   # or: zgrep UNIWILL /proc/config.gz".into());
        }
        fixes.push(
            "  docs:    https://docs.kernel.org/admin-guide/laptops/uniwill-laptop.html".into(),
        );
        fixes.push(String::new());
    }
    if let Err(e) = &hid_res {
        fixes.push("RGB keyboard: neither the EC nor an ITE (048D) controller reports one:".into());
        fixes.push(format!("  {e}"));
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
    match open_ec(cli) {
        Ok(mut ec) => {
            println!("  backend     : {}", ec.name());
            println!(
                "  model       : {}",
                ripple_hw::linux::detect_model(Some(ec.as_mut()))
            );
            if let Ok(s1) = ec.read(addr::SUPPORT_BYTE1) {
                println!("  support[1]  : {s1:#010b}");
            }
        }
        Err(e) => {
            println!("  backend     : none ({e})");
            println!("  model       : {}", ripple_hw::linux::detect_model(None));
        }
    }

    println!("\nOEM package:");
    match oem_dir(cli) {
        Some(d) => {
            println!("  root        : {}", d.display());
            match model_dir(cli, None) {
                Ok(dir) => match list_profiles(&dir) {
                    Ok(p) => println!(
                        "  fan tables  : {} profiles: {}",
                        dir.file_name().unwrap_or_default().to_string_lossy(),
                        p.join(", ")
                    ),
                    Err(e) => println!("  fan tables  : error {e}"),
                },
                Err(e) => println!("  fan tables  : {e}"),
            }
        }
        None => println!("  root        : not found (pass --oem-dir)"),
    }

    println!("\nRGB solution:");
    match &hid_res {
        Ok(v) if v.starts_with("not needed") => println!("  {v}"),
        Ok(v) => println!("  {v} → a HID RGB device (only the EC solution is supported)"),
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
    if toks.iter().any(|t| {
        matches!(
            *t,
            "arch" | "archlinux" | "manjaro" | "endeavouros" | "cachyos"
        )
    }) {
        return Some("arch".into());
    }
    if toks.iter().any(|t| {
        matches!(
            *t,
            "debian" | "ubuntu" | "linuxmint" | "pop" | "raspbian" | "elementary"
        )
    }) {
        return Some("debian".into());
    }
    Some(id)
}

/// `modinfo -n <name>` → module file path (works for builtin modules too).
fn module_path(name: &str) -> Option<String> {
    let out = std::process::Command::new("modinfo")
        .arg("-n")
        .arg(name)
        .output()
        .ok()?;
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
            rd.flatten()
                .any(|e| e.file_name().to_string_lossy().replace('-', "_") == target)
        })
        .unwrap_or(false)
}

fn status(cli: &Cli) -> CmdResult {
    let mut ec = open_ec(cli)?;

    println!("mode:");
    let stock = control::stock(Some(ec.as_mut()));
    let cfg = Config::load();
    match control::running(ec.as_mut(), &stock, &cfg.profiles) {
        Ok(Some(p)) => println!("  profile         : {}", profile_label(&cfg, p)),
        Ok(None) => println!("  profile         : unknown (matches no profile)"),
        Err(e) => println!("  profile         : unreadable ({e})"),
    }
    if let (Ok(pl1), Ok(pl2)) = (ec.read(addr::PL1_SETTING), ec.read(addr::PL2_SETTING)) {
        println!("  power limits    : PL1 {pl1} W / PL2 {pl2} W");
    }
    // Raw control bytes only in debug builds.
    if cfg!(debug_assertions) {
        if let Ok(b) = ec.read(addr::MAFAN_CONTROL_BYTE) {
            println!(
                "  [debug] fan control byte: {b:#04x} ({})",
                fan_mode_name(b)
            );
        }
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
                println!("  {l:13}: {rpm:>5} rpm  {pwm}");
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
    // TODO(charge-limit): show EC charge limit (BATTERY_CHARGE_LIMIT_UP) once charge limits are wanted again.
    if let Some(u) = &sysfs {
        if let Ok((sel, all)) = u.charge_types() {
            println!("  charge mode     : {sel} (available: {})", all.join(", "));
        }
        if let Ok(bat) = std::fs::read_to_string("/sys/class/power_supply/BAT0/status") {
            println!("  status          : {}", bat.trim());
        }
        if let Ok(h) = std::fs::read_to_string("/sys/class/power_supply/BAT0/health") {
            println!("  health          : {}", h.trim());
        }
    }

    let lightbar = control::has_lightbar(ec.as_mut());
    if let Some(u) = sysfs.as_ref().filter(|_| lightbar) {
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
                        t.pl1()
                            .map(|v| v.to_string())
                            .unwrap_or_else(|| "NA".into()),
                        t.pl2()
                            .map(|v| v.to_string())
                            .unwrap_or_else(|| "NA".into()),
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

    // Fallback: EC registers (acpi/wmi).
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

/// A profile's name as shown: the stock modes in lowercase, one of the user's by its own name.
fn profile_label(cfg: &Config, p: Profile) -> String {
    match p {
        Profile::Custom(i) => cfg
            .profiles
            .get(i)
            .map_or_else(|| "custom".to_string(), |c| c.name.clone()),
        _ => p.name().to_lowercase(),
    }
}

/// The profile a `rcc fan custom` command means: `--profile` (a stock mode or one of yours), else what is
/// running now, else Gaming.
fn fan_profile(cli: &Cli, cfg: &Config, name: &Option<String>) -> Result<Profile, String> {
    use clap::ValueEnum;
    if let Some(n) = name {
        return Ok(match ModeChoice::from_str(n, true) {
            Ok(m) => Profile::Stock(operating_mode(m)),
            Err(_) => Profile::Custom(find_profile(cfg, n)?),
        });
    }
    let running = open_ec(cli).ok().and_then(|mut ec| {
        let stock = control::stock(Some(ec.as_mut()));
        control::running(ec.as_mut(), &stock, &cfg.profiles)
            .ok()
            .flatten()
    });
    Ok(running.unwrap_or(Profile::Stock(OperatingMode::Gaming)))
}

fn find_profile(cfg: &Config, name: &str) -> Result<usize, String> {
    cfg.profiles
        .iter()
        .position(|p| p.name.eq_ignore_ascii_case(name))
        .ok_or_else(|| {
            let names: Vec<_> = cfg.profiles.iter().map(|p| p.name.as_str()).collect();
            format!(
                "no profile named '{name}' (yours: {})",
                if names.is_empty() {
                    "none".into()
                } else {
                    names.join(", ")
                }
            )
        })
}

/// Apply a profile; prints what was written.
fn apply_profile(
    ec: &mut dyn EcAccess,
    cli: &Cli,
    stock: &[Preset; 3],
    cfg: &Config,
    p: Profile,
) -> CmdResult {
    let pr = control::preset_for(stock, &cfg.profiles, p);
    let sysfs = uniwill(cli).ok();
    let pl4_refused = control::apply(ec, sysfs.as_ref(), pr)?;
    control::set_mode_byte(ec, p)?; // the button's mode lights (0/1/2) + the EC's fan profile
    control::remember_profile(&cfg.profiles, p); // which of two with the same limits is running
    println!(
        "mode → {} (PL1 {} W, PL2 {} W, PL4 {} W, GPU cTGP +{} W)",
        profile_label(cfg, p),
        pr.pl1,
        pr.pl2,
        pr.pl4,
        pr.ctgp
    );
    if let Some(why) = pl4_refused {
        eprintln!("warning: PL4 was not applied ({why}); PL1 and PL2 were");
    }
    if sysfs.is_none() && pr.ctgp > 0 {
        println!("note: uniwill kernel driver not available, GPU cTGP not applied");
    }
    Ok(())
}

fn operating_mode(m: ModeChoice) -> OperatingMode {
    match m {
        ModeChoice::Office => OperatingMode::Office,
        ModeChoice::Gaming => OperatingMode::Gaming,
        ModeChoice::Turbo => OperatingMode::Turbo,
    }
}

fn limits(pr: &Preset) -> String {
    format!(
        "PL1 {} W, PL2 {} W, PL4 {} W, cTGP +{} W",
        pr.pl1, pr.pl2, pr.pl4, pr.ctgp
    )
}

fn mode(cli: &Cli, cmd: &ModeCmd) -> CmdResult {
    use clap::ValueEnum;
    let mut ec = open_ec(cli)?;
    let stock = control::stock(Some(ec.as_mut()));
    let cfg = Config::load();
    match cmd {
        ModeCmd::Get => {
            let (pl1, pl2, pl4) = (
                ec.read(addr::PL1_SETTING)?,
                ec.read(addr::PL2_SETTING)?,
                ec.read(addr::PL4_SETTING)?,
            );
            match control::running(ec.as_mut(), &stock, &cfg.profiles)? {
                Some(p) => println!(
                    "mode: {} (PL1 {pl1} W, PL2 {pl2} W, PL4 {pl4} W)",
                    profile_label(&cfg, p)
                ),
                None => println!("mode: unknown (PL1 {pl1} W, PL2 {pl2} W matches no profile)"),
            }
            Ok(())
        }
        ModeCmd::Set { mode } => {
            // a stock name (or its Windows-app alias) first, else one of the user's profiles
            let p = match ModeChoice::from_str(mode, true) {
                Ok(m) => Profile::Stock(operating_mode(m)),
                Err(_) => Profile::Custom(find_profile(&cfg, mode)?),
            };
            apply_profile(ec.as_mut(), cli, &stock, &cfg, p)
        }
        ModeCmd::Next => {
            let cur = control::running(ec.as_mut(), &stock, &cfg.profiles)?;
            apply_profile(ec.as_mut(), cli, &stock, &cfg, Profile::after_button(cur))
        }
        ModeCmd::List => {
            println!("stock (read-only):");
            for (m, pr) in control::STOCK_MODES.into_iter().zip(&stock) {
                println!("  {:<10} {}", m.as_str(), limits(pr));
            }
            println!("yours:");
            if cfg.profiles.is_empty() {
                println!("  none (make one with `rcc mode custom <name> <pl1> <pl2>`)");
            }
            for c in &cfg.profiles {
                println!(
                    "  {:<10} {} (from {})",
                    c.name,
                    limits(&c.preset),
                    c.base.as_str()
                );
            }
            Ok(())
        }
        ModeCmd::Custom {
            name,
            pl1,
            pl2,
            pl4,
            ctgp,
            base,
        } => {
            let name = name.trim();
            if name.is_empty() || ModeChoice::from_str(name, true).is_ok() {
                return Err(format!(
                    "'{name}' cannot be a profile name (empty, or a stock mode)"
                ));
            }
            let pr = Preset {
                pl1: *pl1,
                pl2: *pl2,
                pl4: *pl4,
                ctgp: *ctgp,
            };
            pr.validate()?;
            Config::update(|cfg| {
                match find_profile(cfg, name) {
                    Ok(i) => cfg.profiles[i].preset = pr,
                    Err(_) => cfg.profiles.push(control::CustomProfile {
                        name: name.to_string(),
                        base: operating_mode(*base),
                        preset: pr,
                    }),
                }
                Ok(())
            })?;
            println!(
                "profile '{name}' saved: {} (apply with `rcc mode set \"{name}\"`)",
                limits(&pr)
            );
            Ok(())
        }
        ModeCmd::ResetCustom { name } => {
            let (name, base) = Config::update(|cfg| {
                let i = find_profile(cfg, name)?;
                let base = cfg.profiles[i].base;
                cfg.profiles[i].preset = stock[base as usize];
                Ok((cfg.profiles[i].name.clone(), base))
            })?;
            println!(
                "profile '{name}' back to {}: {} (apply with `rcc mode set \"{name}\"`)",
                base.as_str(),
                limits(&stock[base as usize]),
            );
            Ok(())
        }
        ModeCmd::Delete { name } => {
            let gone = Config::update(|cfg| {
                let i = find_profile(cfg, name)?;
                let gone = cfg.profiles.remove(i);
                cfg.fan_sets.remove(&gone.name);
                Ok(gone)
            })?;
            println!("profile '{}' deleted", gone.name);
            Ok(())
        }
    }
}

/// Foreground loop: the Linux stand-in for the stock service's mode-button handling.
fn watch(cli: &Cli) -> CmdResult {
    let mut ec = open_ec(cli)?;
    let stock = control::stock(Some(ec.as_mut()));
    let cfg = Config::load();
    // First find out which profile the EC is running (right after boot its limits are unset, only
    // the lights know), write its limits and put the lights on it: this has to come before the fan
    // engine can set its manual bit, which hides the mode from `current`.
    match control::adopt(
        ec.as_mut(),
        uniwill(cli).ok().as_ref(),
        &stock,
        &cfg.profiles,
    )? {
        Some(p) => println!("profile: {}", profile_label(&cfg, p)),
        None => println!("profile: unknown (limits and lights match no profile)"),
    }
    // The custom fan profile (when switched on) runs inside this daemon, independent of the button
    // lock below: it must not wait for a GUI that holds it.
    static NEVER: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    std::thread::spawn(|| control::fan_loop(&NEVER));
    // Popups for profile / fan boost / toggle changes, whoever makes them: the daemon is always there.
    std::thread::spawn(|| control::popup_loop(&NEVER));
    // keeps the touchpad on or off as the user chose (an "off" does not survive a reboot by itself)
    std::thread::spawn(|| control::touchpad_loop(&NEVER));
    // puts the NVIDIA GPU's clock offsets back after a driver reload (does nothing without the driver)
    std::thread::spawn(|| control::gpu_loop(&NEVER));
    // the touchpad's corner double tap (keys from the internal keyboard) switches the touchpad off and on
    std::thread::spawn(|| control::touchpad_key_loop(&NEVER));
    let retry = std::time::Duration::from_secs(10);
    let mut key = open_waiting(control::open_mode_key, retry);
    let _lock = match control::follow_lock(false) {
        Some(l) => l,
        None => {
            eprintln!("another process (the GUI?) follows the button, waiting for it to exit");
            control::follow_lock(true).ok_or("could not take the button lock")?
        }
    };
    println!("watching the mode button (Ctrl+C to stop)");
    loop {
        if let Err(e) = control::wait_mode_key(&mut key) {
            eprintln!("mode button: {e}"); // the device went away (driver unloaded): wait for it again
            key = open_waiting(control::open_mode_key, retry);
            continue;
        }
        // read again: profiles made in the window or with `rcc mode custom` since the last press count
        let cfg = Config::load();
        let pressed = control::running(ec.as_mut(), &stock, &cfg.profiles)
            .map_err(|e| e.to_string())
            .and_then(|cur| {
                apply_profile(ec.as_mut(), cli, &stock, &cfg, Profile::after_button(cur))
            });
        if let Err(e) = pressed {
            eprintln!("error: {e}"); // keep watching, a failed press must not kill the daemon
        }
    }
}

/// `open`, again and again until it works. The mode button's device exists only while the kernel
/// driver is loaded (it may come after the daemon starts, say after a rebuild for a new kernel), and
/// the fan, popup and touchpad threads must not die with a missing device.
fn open_waiting<T>(mut open: impl FnMut() -> HwResult<T>, pause: std::time::Duration) -> T {
    let mut said = false;
    loop {
        match open() {
            Ok(v) => return v,
            Err(e) => {
                if !said {
                    eprintln!("mode button: {e}; trying again every {} s", pause.as_secs());
                    said = true;
                }
                std::thread::sleep(pause);
            }
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
                let t = FanTable::load(path)?;
                println!("{}", serde_json::to_string_pretty(&t).map_err(|e| e.to_string())?);
                Ok(())
            }
        },
        FanCmd::Boost { state } => {
            if state.as_bool() && Config::load().fan.enabled {
                return Err("the custom fan profile is on and would undo the boost: `rcc fan custom off` first".into());
            }
            let mut ec = open_ec(cli)?;
            let (old, new) = control::set_boost(ec.as_mut(), state.as_bool())?;
            if old == new {
                println!("fan boost already {}", if state.as_bool() { "on" } else { "off" });
            } else {
                println!("fan control byte {old:#04x} → {new:#04x} ({})", fan_mode_name(new));
            }
            Ok(())
        }
        FanCmd::Custom { cmd } => fan_custom(cli, cmd),
        FanCmd::Release { force } => {
            // the service runs this whenever it stops; without the custom profile the fans are not
            // ours (a user's Boost, say), so leave them alone
            if !*force && !Config::load().fan.enabled {
                println!("the custom fan profile is off, the fans are not under its control (--force hands them back anyway)");
                return Ok(());
            }
            let mut ec = open_ec(cli)?;
            control::fan_release(ec.as_mut())?;
            println!("fans handed back to the firmware");
            Ok(())
        }
        FanCmd::Level { level } => Err(format!(
            "fan level {level} refused: user fan levels (0x81..0x85) read their PWM from tables that are \
             empty on this EC (curve RAM 3840..4095 is all zero; regs 1859..1862 are GPU cTGP/Dynamic \
             Boost here), so the fans could be told to stop. Use `fan custom` or `fan boost on|off`."
        )),
    }
}

fn touchpad(choice: crate::TouchpadChoice) -> CmdResult {
    use crate::TouchpadChoice::*;
    let powered = ripple_hw::touchpad::is_on().ok_or("no Uniwill touchpad (i2c-UNIW*) found")?;
    // powered but silent: the corner double tap switched it off; that is "off" here and in the window too
    let silent = powered && ripple_hw::touchpad::surface_on() == Some(false);
    let now = powered && !silent;
    let want = match choice {
        On => true,
        Off => false,
        Toggle => !now,
        Status => {
            println!(
                "touchpad: {}",
                if silent {
                    "off (silenced by the double tap, LED on)"
                } else if powered {
                    "on"
                } else {
                    "off (powered down, LED on)"
                }
            );
            return Ok(());
        }
    };
    control::set_touchpad(want)?;
    println!(
        "touchpad: {}",
        if want {
            "on"
        } else {
            "off (powered down, LED on)"
        }
    );
    Ok(())
}

/// `rcc gpu`: the NVIDIA GPU's state and clock offsets.
fn gpu(cmd: &GpuCmd) -> CmdResult {
    let g = ripple_hw::gpu::Gpu::open().map_err(|e| {
        format!("{e}: the GPU settings need the NVIDIA driver (README, \"The NVIDIA driver\")")
    })?;
    let save =
        |f: &dyn Fn(&mut ripple_hw::gpu::GpuTune)| -> Result<ripple_hw::gpu::GpuTune, String> {
            Ok(Config::update(|c| {
                let mut t = c.gpu;
                f(&mut t);
                t.validate()?;
                c.gpu = t;
                Ok(t)
            })?)
        };
    match cmd {
        GpuCmd::Show => {}
        GpuCmd::Set { core, mem } => {
            let t = save(&|t| {
                t.core_mhz = core.unwrap_or(t.core_mhz);
                t.mem_mhz = mem.unwrap_or(t.mem_mhz);
            })?;
            if t.enabled {
                g.apply(&t)?;
            } else {
                println!("saved; `rcc gpu on` applies it");
            }
        }
        GpuCmd::On | GpuCmd::Off => {
            let on = matches!(cmd, GpuCmd::On);
            let t = save(&|t| t.enabled = on)?;
            g.apply(&t)?;
        }
    }
    let (s, t) = (g.status(), Config::load().gpu);
    let w = |v: Option<f64>| v.map_or("n/a".to_string(), |v| format!("{v:.0} W"));
    println!("{}", s.name);
    println!(
        "  power         : {} now, limit {} (default {}, at most {})",
        w(s.power_w),
        w(s.limit_w),
        w(s.default_limit_w),
        w(s.max_limit_w)
    );
    println!(
        "  clocks        : core {} MHz, memory {} MHz, {} °C, P{}",
        s.core_mhz.map_or("n/a".into(), |v| v.to_string()),
        s.mem_mhz.map_or("n/a".into(), |v| v.to_string()),
        s.temp_c.map_or("n/a".into(), |v| v.to_string()),
        s.pstate.map_or("?".into(), |v| v.to_string())
    );
    println!(
        "  offsets now   : core {:+} MHz, memory {:+} MHz",
        s.core_offset, s.mem_offset
    );
    println!(
        "  offsets saved : core {:+} MHz, memory {:+} MHz, {}",
        t.core_mhz,
        t.mem_mhz,
        if t.enabled { "on" } else { "off" }
    );
    Ok(())
}

fn popups(state: Option<crate::OnOff>) -> CmdResult {
    if let Some(s) = state {
        ripple_hw::notify::set_popups(s.as_bool())
            .map_err(|e| format!("could not save the popup setting: {e} (run with sudo)"))?;
    }
    let on = ripple_hw::notify::popups_enabled();
    println!("popups: {}", if on { "on" } else { "off" });
    Ok(())
}

fn fan_custom(cli: &Cli, cmd: &CustomFanCmd) -> CmdResult {
    let cfg = Config::load();
    match cmd {
        CustomFanCmd::Show { profile } => {
            let p = fan_profile(cli, &cfg, profile)?;
            let set = cfg.fan_set(p);
            let anchors: Vec<String> = control::FAN_ANCHORS
                .iter()
                .map(|t| format!("{t:>4}"))
                .collect();
            let row = |d: &[u8; 5]| d.iter().map(|d| format!("{d:>4}")).collect::<String>();
            println!(
                "custom fan profile: {} (curves of {}, {})",
                if cfg.fan.enabled { "on" } else { "off" },
                profile_label(&cfg, p),
                if cfg.fan_sets.contains_key(&cfg.profile_key(p)) {
                    "yours"
                } else {
                    "the default"
                }
            );
            println!("  °C            {}", anchors.concat());
            println!("  fan 1 (CPU) % {}", row(&set.cpu));
            println!("  fan 2 (GPU) % {}", row(&set.gpu));
            println!(
                "  (linear between the points; at {} °C or with no sensor both fans run at 100 %)",
                control::FAN_PANIC_C
            );
        }
        CustomFanCmd::On => {
            Config::update(|c| {
                c.fan.enabled = true;
                Ok(())
            })?;
            // the engine leaves a boost alone, so end it first or the profile would never take over
            if let Ok(mut ec) = open_ec(cli) {
                if ec
                    .read(addr::MAFAN_CONTROL_BYTE)
                    .is_ok_and(control::is_boosted)
                {
                    control::fan_release(ec.as_mut())?;
                }
            }
            println!(
                "custom fan profile on: the ripple-nova service applies it within a few seconds"
            );
        }
        CustomFanCmd::Off => {
            Config::update(|c| {
                c.fan.enabled = false;
                Ok(())
            })?;
            control::fan_release(open_ec(cli)?.as_mut())?;
            println!("custom fan profile off, fans handed back to the firmware");
        }
        CustomFanCmd::Set { cpu, gpu, profile } => {
            let arr = |v: &Vec<u8>| {
                <[u8; 5]>::try_from(v.as_slice())
                    .map_err(|_| "need exactly five values".to_string())
            };
            let label = Config::update(|cfg| {
                let p = fan_profile(cli, cfg, profile)?;
                let mut set = cfg.fan_set(p);
                if let Some(v) = cpu {
                    set.cpu = arr(v)?;
                }
                if let Some(v) = gpu {
                    set.gpu = arr(v)?;
                }
                set.validate()?;
                cfg.set_fan_set(p, set);
                Ok(profile_label(cfg, p))
            })?;
            println!(
                "curves of {label} saved (on at {:?} °C)",
                control::FAN_ANCHORS
            );
        }
        CustomFanCmd::Reset { profile } => {
            let label = Config::update(|cfg| {
                let p = fan_profile(cli, cfg, profile)?;
                let default = cfg.fan_default(p);
                cfg.set_fan_set(p, default);
                Ok(profile_label(cfg, p))
            })?;
            println!("curves of {label} back to the default preset");
        }
        CustomFanCmd::Run { seconds } => {
            static STOP: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
            if let Some(s) = *seconds {
                std::thread::spawn(move || {
                    std::thread::sleep(std::time::Duration::from_secs(s));
                    STOP.store(true, std::sync::atomic::Ordering::Relaxed);
                });
            }
            if !cfg.fan.enabled {
                return Err("the custom profile is off: `rcc fan custom on` first".into());
            }
            control::fan_loop(&STOP);
        }
    }
    Ok(())
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
        PowerCmd::Pl1 { watts } | PowerCmd::Pl2 { watts } => {
            let is_pl1 = matches!(cmd, PowerCmd::Pl1 { .. });
            let w = u8::try_from(*watts).map_err(|_| format!("{watts} W is out of range"))?;
            let (reg, other) = if is_pl1 {
                (addr::PL1_SETTING, addr::PL2_SETTING)
            } else {
                (addr::PL2_SETTING, addr::PL1_SETTING)
            };
            let other = ec.read(other)?;
            let (pl1, pl2) = if is_pl1 { (w, other) } else { (other, w) };
            Preset {
                pl1,
                pl2,
                pl4: control::PL4_CEILING_W,
                ctgp: 0,
            }
            .validate()?; // 5..=ceiling, PL1 <= PL2
            ec.write_verified(reg, w)?;
            println!(
                "PL{} → {w} W (readback {} W)",
                if is_pl1 { 1 } else { 2 },
                ec.read(reg)?
            );
            Ok(())
        }
    }
}

/// Backlight on (back to the level it had) or off (the level is remembered).
fn keyboard_backlight(ec: &mut dyn EcAccess, on: bool) -> CmdResult {
    if on {
        control::keyboard_on(ec, Config::load().keyboard_level)?;
    } else {
        let level = control::keyboard_off(ec)?;
        if level > 0 {
            Config::update(|c| {
                c.keyboard_level = Some(level);
                Ok(())
            })?;
        }
    }
    println!("keyboard backlight {}", if on { "on" } else { "off" });
    Ok(())
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
                Effect::Single,
                Effect::Breathing,
                Effect::Wave,
                Effect::Reactive,
                Effect::Rainbow,
                Effect::Ripple,
                Effect::Raindrop,
                Effect::Neon,
                Effect::Marquee,
                Effect::Stack,
                Effect::Impact,
                Effect::Spark,
                Effect::Aurora,
                Effect::Music,
                Effect::UserMode,
                Effect::Gaming,
                Effect::Flash,
                Effect::Mix,
                Effect::RippleO,
                Effect::Alphabet,
                Effect::StarHitting,
                Effect::StarSpark,
                Effect::Thinking,
                Effect::Manual,
                Effect::BatteryPercent,
            ];
            for e in ALL.iter() {
                println!("{:>2}  {}", *e as u8, e.name());
            }
            Ok(())
        }
        RgbCmd::Color { color } => {
            let mut ec = open_ec(cli)?;
            if !control::has_keyboard_rgb(ec.as_mut()) {
                return Err("the EC reports no RGB keyboard on this machine".into());
            }
            if color.eq_ignore_ascii_case("off") {
                return keyboard_backlight(ec.as_mut(), false);
            }
            let hsv = Hsv::from_levels(control::kb_from_8bit(parse_hex_color(color)?));
            control::keyboard_on(ec.as_mut(), Config::load().keyboard_level)?;
            set_keyboard(ec.as_mut(), hsv)
        }
        RgbCmd::Brightness { percent } => {
            let mut ec = open_ec(cli)?;
            if !control::has_keyboard_rgb(ec.as_mut()) {
                return Err("the EC reports no RGB keyboard on this machine".into());
            }
            if *percent > 100 {
                return Err("brightness is 0..=100 (percent)".into());
            }
            let base = match Config::load().keyboard {
                Some(h) => h,
                None => Hsv::from_levels(control::keyboard_rgb(ec.as_mut())?),
            };
            set_keyboard(
                ec.as_mut(),
                Hsv {
                    v: *percent,
                    ..base
                },
            )
        }
        RgbCmd::Backlight { state } => {
            let mut ec = open_ec(cli)?;
            if !control::has_keyboard_rgb(ec.as_mut()) {
                return Err("the EC reports no RGB keyboard on this machine".into());
            }
            keyboard_backlight(ec.as_mut(), state.as_bool())
        }
        RgbCmd::Rainbow { state } => {
            let mut ec = open_ec(cli)?;
            if !control::has_keyboard_rgb(ec.as_mut()) {
                return Err("the EC reports no RGB keyboard on this machine".into());
            }
            control::keyboard_on(ec.as_mut(), Config::load().keyboard_level)?;
            control::set_keyboard_rainbow(ec.as_mut(), state.as_bool())?;
            println!(
                "keyboard: {}",
                if state.as_bool() { "rainbow" } else { "colour" }
            );
            Ok(())
        }
        RgbCmd::Lightbar { color } => {
            let u = uniwill(cli)?;
            if open_ec(cli).is_ok_and(|mut ec| !control::has_lightbar(ec.as_mut())) {
                eprintln!("note: the EC reports no lightbar on this machine (the kernel exposes the LED anyway), so nothing will light up");
            }
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

/// Apply a keyboard colour and remember it (the EC cannot hold hue and brightness separately).
fn set_keyboard(ec: &mut dyn EcAccess, hsv: Hsv) -> CmdResult {
    control::set_keyboard_hsv(ec, hsv)?;
    Config::update(|c| {
        c.keyboard = Some(hsv);
        Ok(())
    })?;
    let l = hsv.to_levels();
    println!(
        "keyboard colour → hue {} sat {} % brightness {} % (EC levels {},{},{} of {})",
        hsv.h,
        hsv.s,
        hsv.v,
        l.0,
        l.1,
        l.2,
        control::KB_LEVEL_MAX
    );
    Ok(())
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
    // TODO(charge-limit): `battery charge-limit <percent>` and the EC limit reads
    // (BATTERY_CHARGE_LIMIT_UP/DOWN, 1977/2000) are parked; add back with
    // `BatteryCmd::ChargeLimit` in main.rs when wanted.
    let u = uniwill(cli)
        .map_err(|e| format!("battery charge modes need the uniwill kernel driver: {e}"))?;
    match cmd {
        BatteryCmd::Get => {
            let (sel, all) = u.charge_types()?;
            println!("battery charge mode: {sel} (available: {})", all.join(", "));
            Ok(())
        }
        BatteryCmd::Profile { profile } => {
            // OEM names map onto the kernel charge_types interface.
            let want = match profile {
                ProfileChoice::Performance | ProfileChoice::Standard => "Standard",
                ProfileChoice::Balanced | ProfileChoice::LongLife => "Long_Life",
                ProfileChoice::Healthy | ProfileChoice::Trickle => "Trickle",
            };
            let (_sel, all) = u.charge_types()?;
            u.set_charge_types(want)?;
            println!(
                "battery charge mode → {want} (available: {})",
                all.join(", ")
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
            println!("{n} = {}", u.get_attr(n)?);
            Ok(())
        }
        Some(SystemCmd::Set { name, value }) => {
            let n = canonical_attr(name)?;
            let new = u.set_attr(n, value).map_err(|e| {
                let allowed = attr_value_hint(n);
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
        t.parse::<u16>()
            .map_err(|e| format!("bad address '{s}': {e}"))?
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
        EcCmd::Dump { start, end } => {
            let (a, b) = (parse_addr(start)?, parse_addr(end)?);
            if a > b || b > 0xFFF {
                return Err("range must satisfy start <= end <= 0xFFF".into());
            }
            let mut ec = open_ec(cli)?;
            for addr in a..=b {
                println!("{addr} 0x{:02x}", ec.read(addr)?);
            }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_device_that_appears_late_is_waited_for() {
        let mut tries = 0;
        let got = open_waiting(
            || {
                tries += 1;
                if tries < 3 {
                    Err(HwError::Unavailable("not there yet".into()))
                } else {
                    Ok(tries)
                }
            },
            std::time::Duration::ZERO,
        );
        assert_eq!(got, 3);
    }
}
