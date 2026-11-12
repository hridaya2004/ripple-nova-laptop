//! `rcc` — Ripple Control Center CLI.
//!
//! Read-only commands work against the mock backend and the OEM file
//! formats (UserFanTables, RGBKeyboard.reg); the real backends (acpi_call /
//! WMI / sysfs) are selected with `--backend`.
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
    /// Backend (auto = try real hw, fall back to mock)
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
    /// Try acpi → wmi → sysfs, fall back to the mock image
    Auto,
    /// In-memory sample EC (development)
    Mock,
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
    /// Live sensor table (uniwill hwmon, falls back to EC/mock)
    Monitor {
        /// Refresh interval in seconds
        #[arg(long, default_value_t = 1.0)]
        interval: f64,
    },
    /// Performance mode (Office/Gaming/Turbo/Benchmark)
    Mode {
        #[command(subcommand)]
        cmd: ModeCmd,
    },
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
    /// Battery protection / charge limit
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
}

#[derive(Copy, Clone, PartialEq, Eq, ValueEnum)]
pub enum ModeChoice {
    /// Silent/low power (PL defaults + fan profile)
    Office,
    /// Balanced gaming profile
    Gaming,
    /// Max sustained performance
    Turbo,
    /// Short-burst benchmark profile
    Benchmark,
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
    /// Manual fan level 0..=5 (0 = auto/normal)
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
    /// Show current PL1/PL2/TCC (EC read or mock)
    Get,
    /// Set PL1 in watts (5..=150)
    Pl1 { watts: u16 },
    /// Set PL2 in watts (5..=150)
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
    /// Keyboard color: 6 hex digits (`ff8800`) or `off`
    Color { color: String },
    /// Lightbar color: 6 hex digits, `off`, or `on` (restores full white)
    ///
    /// Uses the kernel LED class (`uniwill:multicolor:status`).
    Lightbar { color: String },
}

#[derive(Subcommand)]
pub enum BatteryCmd {
    /// Show charge limits, charge profile, battery health/status
    Get,
    /// Set charge limit 0..=100 (EC registers; kernel threshold attr if present)
    ChargeLimit { percent: u8 },
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
    if let Err(e) = commands::run(&cli) {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}
