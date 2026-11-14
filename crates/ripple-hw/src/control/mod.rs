//! Profile (Office/Gaming/Turbo), fan boost and mode-button handling shared by
//! `rcc` and `rcc-gui`. Everything here was verified on real hardware:
//! PL1/PL2 via EC 1923/1924 (35→55→65 W measured as package power under
//! load), fan boost via the 0x40 bit of EC 1873, mode button = WMI event 176 =
//! key 184 (KEY_F14) on the "Uniwill WMI hotkeys" input device.

use std::collections::BTreeMap;
use std::io::Read;
use std::path::Path;

use ripple_core::modes::{FanControlByte, OperatingMode};
use ripple_core::regs::{addr, duty_from_ec, duty_to_ec};
use serde::{Deserialize, Serialize};

use crate::linux::SysfsEc;
use crate::store;
use crate::{EcAccess, HwError, HwResult};

mod config;
mod daemon;
mod device;
mod fan;
mod keyboard;
mod profile;
#[cfg(test)]
mod tests;

pub use config::*;
pub use daemon::*;
pub use device::*;
pub use fan::*;
pub use keyboard::*;
pub use profile::*;
