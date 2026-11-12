//! `rcc` — Ripple Control Center CLI.
//!
//! Talks to real hardware only (acpi_call / WMI / kernel sysfs) and reads the
//! OEM file formats (UserFanTables, RGBKeyboard.reg). Needs root.
//!
//! Enumerated arguments use clap `ValueEnum`, so every `--help` shows the
//! legal values (`[possible values: …]`) instead of failing at runtime.

mod commands;

use clap::{Parser, Subcommand, ValueEnum};

#[derive(Parser)]
#[command(
    name = "rcc",
    version,
    about = "Ripple Control Center — Linux control for Uniwill/XMG barebones",
    long_about = "Reimplements the OEM Control Center features (fans, performance modes,\n\
                  RGB, battery protection, monitoring, display) as a Rust CLI."
)]
struct Cli {
    /// EC backend (auto = acpi_call, then wmi)
    #[arg(long, global = true, value_enum, default_value_t = Backend::Auto)]
    backend: Backend,

    /// Path to the OEM package root (UserFanTables/, RGBKeyboard.reg)
    #[arg(long, global = true, env = "RCC_OEM_DIR")]
    oem_dir: Option<std::path::PathBuf>,

    #[command(subcommand)]
    command: Command,
}

#[derive(Copy, Clone, PartialEq, Eq, ValueEnum)]
pub enum Backend {
    /// Try acpi → wmi
    Auto,
    /// ECRR/ECRW via /proc/acpi/call (needs sudo)
    Acpi,
    /// WMBC GetSetULong via /proc/acpi/call (needs sudo)
    Wmi,
    /// uniwill-laptop kernel driver (sensors, lightbar, charge profiles)
    Sysfs,
}

#[derive(Copy, Clone, PartialEq, Eq, ValueEnum)]
pub enum OnOff {
    On,
    Off,
}

impl OnOff {
    pub fn as_bool(self) -> bool {
        matches!(self, OnOff::On)
    }
}

#[derive(Subcommand)]
pub enum Command {
    /// Probe hardware: backends, ACPI paths, model, RGB solution, fixes
    Doctor,
    /// Show current mode, fans, temps, battery, RGB summary
    Status,
    /// Live sensor table (uniwill hwmon, falls back to EC)
    Monitor {
        /// Refresh interval in seconds
        #[arg(long, default_value_t = 1.0)]
        interval: f64,
    },
    /// Performance profile (Office/Gaming/Turbo): show, set, cycle
    Mode {
        #[command(subcommand)]
        cmd: ModeCmd,
    },
    /// Follow the hardware mode button: each press cycles Office → Gaming → Turbo and
    /// applies it (what the stock Windows service does). Runs until stopped.
    Watch,
    /// Fan control: curves, boost, levels
    Fan {
        #[command(subcommand)]
        cmd: FanCmd,
    },
    /// Power limits: PL1/PL2/TCC
    Power {
        #[command(subcommand)]
        cmd: PowerCmd,
    },
    /// RGB keyboard + lightbar
    Rgb {
        #[command(subcommand)]
        cmd: RgbCmd,
    },
    /// Battery charge profile (charge limits are a TODO)
    Battery {
        #[command(subcommand)]
        cmd: BatteryCmd,
    },
    /// Display refresh rate / dGPU power
    Display {
        #[command(subcommand)]
        cmd: DisplayCmd,
    },
    /// Kernel platform toggles (fn-lock, super key, touchpad, USB-C priority, cTGP)
    ///
    /// `rcc system` alone lists every attribute with its current value.
    System {
        #[command(subcommand)]
        cmd: Option<SystemCmd>,
    },
    /// Raw EC register access (read-before-write; dev/RE tool)
    ///
    /// Addresses may be decimal or hex (0x751). Writes outside the known
    /// register map are rejected without --force.
    Ec {
        #[command(subcommand)]
        cmd: EcCmd,
    },
}

#[derive(Subcommand)]
pub enum EcCmd {
    /// Read one EC byte: `rcc ec read 1873` or `rcc ec read 0x751`
    Read { addr: String },
    /// Dump an inclusive EC range as `addr value` lines, for diffing two hardware states
    /// (e.g. `rcc ec dump 0 4095 > a.txt`, change something, dump again, `diff a.txt b.txt`)
    Dump { start: String, end: String },
    /// Write one EC byte (prints old → new + readback verify)
    Write {
        addr: String,
        value: String,
        /// Allow addresses outside the known register map
        #[arg(long)]
        force: bool,
    },
}

#[derive(Subcommand)]
pub enum ModeCmd {
    /// Show current mode
    Get,
    /// Set the performance mode
    Set {
        #[arg(value_enum)]
        mode: ModeChoice,
    },
    /// Switch to the next mode, like one press of the hardware button
    Next,
    /// Save your Custom preset (the stock ones are read-only): `rcc mode custom 45 50 --ctgp 5`
    Custom {
        /// PL1 in watts
        pl1: u8,
        /// PL2 in watts
        pl2: u8,
        /// GPU cTGP offset in watts
        #[arg(long, default_value_t = 0)]
        ctgp: u8,
    },
}

#[derive(Copy, Clone, PartialEq, Eq, ValueEnum)]
pub enum ModeChoice {
    /// 0 lights: lowest power (EC default PL, 35 W here)
    Office,
    /// 1 light: gaming (EC default PL, 55 W here)
    Gaming,
    /// 2 lights: Overboost (80 W + GPU cTGP +20 W, the stock app's values for this machine)
    Turbo,
    /// Your own preset (see `rcc mode custom`); not reachable with the hardware button
    Custom,
}

#[derive(Subcommand)]
pub enum FanCmd {
    /// Show fan table profiles / curves
    Curve {
        #[command(subcommand)]
        cmd: CurveCmd,
    },
    /// Fan boost on/off (EC fan control byte 0x40)
    Boost {
        #[arg(value_enum)]
        state: OnOff,
    },
    /// Manual fan level (disabled: this EC's user-level tables are empty, see `fan level`)
    Level {
        /// 0=normal, 1..=5 user levels (0x81..0x85)
        level: u8,
    },
}

#[derive(Subcommand)]
pub enum CurveCmd {
    /// List shipped profiles for the detected model (or --model)
    List {
        #[arg(long)]
        model: Option<String>,
    },
    /// Show one profile: `rcc fan curve show M1T1 --model PH4ARxx`
    Show {
        profile: String,
        #[arg(long)]
        model: Option<String>,
    },
    /// Show a curve JSON file
    File { path: std::path::PathBuf },
}

#[derive(Subcommand)]
pub enum PowerCmd {
    /// Show current PL1/PL2/TCC (EC read)
    Get,
    /// Set PL1 in watts (5..=80, the stock Turbo value for this machine)
    Pl1 { watts: u16 },
    /// Set PL2 in watts (5..=80)
    Pl2 { watts: u16 },
    /// Set TCC offset in °C (-30..=0)
    Tcc { offset: i8 },
}

#[derive(Subcommand)]
pub enum RgbCmd {
    /// Show saved RGB state (from RGBKeyboard.reg)
    State,
    /// Show effects list (ids as used by the EC/HID protocol)
    Effects,
    /// Keyboard backlight colour: 6 hex digits (`ff8800`) or `off` (EC registers 1897-1899)
    Color { color: String },
    /// Keyboard backlight brightness 0..=100 (percent of the chosen colour)
    Brightness { percent: u8 },
    /// Lightbar color: 6 hex digits, `off`, or `on` (restores full white)
    ///
    /// Uses the kernel LED class (`uniwill:multicolor:status`).
    Lightbar { color: String },
}

#[derive(Subcommand)]
pub enum BatteryCmd {
    /// Show the current charge profile
    Get,
    // TODO(charge-limit): parked for now, see commands.rs `battery`.
    // /// Set charge limit 0..=100 (EC registers; kernel threshold attr if present)
    // ChargeLimit { percent: u8 },
    /// Set protection/charge profile
    ///
    /// OEM names map onto the kernel `charge_types` interface:
    /// performance→Standard, balanced→Trickle, healthy→Long_Life
    /// (native names work too).
    Profile {
        #[arg(value_enum)]
        profile: ProfileChoice,
    },
}

#[derive(Copy, Clone, PartialEq, Eq, ValueEnum)]
pub enum ProfileChoice {
    /// OEM: full-speed charging (= charge_types Standard)
    Performance,
    /// OEM: gentler charging (= charge_types Trickle)
    Balanced,
    /// OEM: max battery longevity (= charge_types Long_Life)
    Healthy,
    /// charge_types: full-speed charging
    Standard,
    /// charge_types: reduced trickle charging
    Trickle,
    /// charge_types: longevity profile
    LongLife,
}

#[derive(Subcommand)]
pub enum DisplayCmd {
    /// List available refresh-rate tools (kscreen-doctor/xrandr/wlr-randr)
    Refresh,
    /// dGPU power: on|off
    Dgpu {
        #[arg(value_enum)]
        state: OnOff,
    },
}

#[derive(Subcommand)]
pub enum SystemCmd {
    /// Show attributes (optionally one by name) with current values
    Show {
        /// Attribute name; omit to list all
        name: Option<String>,
    },
    /// Set an attribute: `rcc system set fn-lock on`
    ///
    /// Boolean attrs take on/off (or 1/0); `usb-c-power-priority` takes
    /// `charging|performance`; `ctgp-offset` takes a number.
    Set {
        name: String,
        value: String,
    },
}

fn main() {
    let cli = Cli::parse();
    if let Err(e) = ripple_hw::require_root(false) {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
    if let Err(e) = commands::run(&cli) {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}
