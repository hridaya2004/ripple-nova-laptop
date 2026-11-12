//! `rcc` — Ripple Control Center CLI.
//!
//! Read-only commands work against the mock backend and the OEM file
//! formats (UserFanTables, RGBKeyboard.reg); the real backends (acpi_call /
//! WMI / sysfs) are selected with `--backend`.

mod commands;

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(
    name = "rcc",
    version,
    about = "Ripple Control Center — Linux control for Uniwill/XMG barebones",
    long_about = "Reimplements the OEM Control Center features (fans, performance modes,\n\
                  RGB, battery protection, monitoring, display) as a Rust CLI."
)]
struct Cli {
    /// Backend: auto|mock|acpi|wmi|sysfs (default: auto → real hw, falls back to mock)
    #[arg(long, global = true, default_value = "auto")]
    backend: String,

    /// Path to the OEM package root (UserFanTables/, RGBKeyboard.reg)
    #[arg(long, global = true, env = "RCC_OEM_DIR")]
    oem_dir: Option<std::path::PathBuf>,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Probe hardware: backend availability, model, RGB solution
    Doctor,
    /// Show current mode, fans, temps, battery, RGB summary
    Status,
    /// Live sensor table (mock backend)
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
}

#[derive(Subcommand)]
pub enum ModeCmd {
    /// Show current mode
    Get,
    /// Set mode: office|gaming|turbo|benchmark
    Set { mode: String },
}

#[derive(Subcommand)]
pub enum FanCmd {
    /// Show fan table profiles / curves
    Curve {
        #[command(subcommand)]
        cmd: CurveCmd,
    },
    /// Fan boost on/off
    Boost { state: String },
    /// Manual fan level 0..=5
    Level { level: u8 },
}

#[derive(Subcommand)]
pub enum CurveCmd {
    /// List shipped profiles for the detected model (or --model)
    List {
        #[arg(long)]
        model: Option<String>,
    },
    /// Show one profile: `rcc fan curve show --model PH4ARxx M1T1`
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
    /// Set PL1 in watts
    Pl1 { watts: u16 },
    /// Set PL2 in watts
    Pl2 { watts: u16 },
    /// Set TCC offset in °C
    Tcc { offset: i8 },
}

#[derive(Subcommand)]
pub enum RgbCmd {
    /// Show saved RGB state (from RGBKeyboard.reg)
    State,
    /// Show effects list
    Effects,
    /// Set a static color: `rcc rgb color ff8800` (or `off`)
    Color { color: String },
}

#[derive(Subcommand)]
pub enum BatteryCmd {
    /// Show charge limit / protection profile
    Get,
    /// Set charge limit 0..=100
    ChargeLimit { percent: u8 },
    /// Set protection profile: performance|balanced|healthy
    Profile { profile: String },
}

#[derive(Subcommand)]
pub enum DisplayCmd {
    /// List available refresh rates (needs kscreen-doctor/xrandr)
    Refresh,
    /// dGPU power: on|off
    Dgpu { state: String },
}

fn main() {
    let cli = Cli::parse();
    if let Err(e) = commands::run(&cli) {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}
