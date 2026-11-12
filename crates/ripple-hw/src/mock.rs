//! In-memory EC backend for development and tests.

use crate::{EcAccess, HwError, HwResult};
use std::collections::BTreeMap;

/// Simple map-backed EC. Pre-populate with known register values to simulate
/// a machine (see `mock::samples::ph4` for a canned PH4-like image).
#[derive(Debug, Default, Clone)]
pub struct MockEc {
    mem: BTreeMap<u16, u8>,
    /// Simulated read of an unmapped address (EC returns garbage or 0).
    unmapped_value: u8,
    pub fail_on: Vec<u16>,
}

impl MockEc {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with(addr: u16, value: u8) -> Self {
        let mut ec = Self::new();
        ec.mem.insert(addr, value);
        ec
    }

    pub fn set(&mut self, addr: u16, value: u8) {
        self.mem.insert(addr, value);
    }

    pub fn image(&self) -> &BTreeMap<u16, u8> {
        &self.mem
    }
}

impl EcAccess for MockEc {
    fn name(&self) -> &str {
        "mock"
    }

    fn read(&mut self, addr: u16) -> HwResult<u8> {
        if self.fail_on.contains(&addr) {
            return Err(HwError::EcFail { addr });
        }
        Ok(self.mem.get(&addr).copied().unwrap_or(self.unmapped_value))
    }

    fn write(&mut self, addr: u16, value: u8) -> HwResult<()> {
        if self.fail_on.contains(&addr) {
            return Err(HwError::EcFail { addr });
        }
        self.mem.insert(addr, value);
        Ok(())
    }
}

/// Canned register images for tests / `rcc status --mock`.
pub mod samples {
    use super::MockEc;
    use ripple_core::regs::{addr, fan_table};

    /// A plausible PH4-like machine: gaming mode, some RPM, charge limit 80.
    pub fn ph4() -> MockEc {
        let mut ec = MockEc::new();
        ec.set(addr::PROJECT_ID, 0x01); // low byte of 5889 (PH4ARxx) — see doctor
        ec.set(addr::MAFAN_CONTROL_BYTE, 0x00);
        // duty 0–200 scale
        ec.set(addr::MAIN_FAN_L_DUTY, 120);
        ec.set(addr::MAIN_FAN_R_DUTY, 118);
        // RPM (arbitrary plausible values, little-endian-ish)
        ec.set(addr::MAIN_FAN_RPM1, 0x60);
        ec.set(addr::MAIN_FAN_RPM2, 0x0F); // 0x0F60 = 3936
        ec.set(addr::SECOND_FAN_RPM1, 0x20);
        ec.set(addr::SECOND_FAN_RPM2, 0x0E);
        // power limits
        ec.set(addr::PL1_SETTING, 45);
        ec.set(addr::PL2_SETTING, 60);
        // battery
        ec.set(addr::BATTERY_CHARGE_LIMIT_UP, 80);
        ec.set(addr::BATTERY_CHARGE_LIMIT_DOWN, 75);
        // a small fan curve in RAM
        for i in 0..16u16 {
            let v_up: u8 = if i == 0 { 0 } else { (40 + i * 3) as u8 };
            let v_duty: u8 = if i == 0 { 0 } else { (i * 8) as u8 };
            ec.set(fan_table::CPU_TEMP_UP + i, v_up);
            ec.set(fan_table::CPU_DUTY + i, v_duty);
        }
        ec
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_and_update() {
        let mut ec = MockEc::with(1873, 0b0000_0101);
        assert_eq!(ec.read(1873).unwrap(), 0b0000_0101);
        // set only bit 5, preserve the rest
        let v = ec.update(1873, 0b0010_0000, 0b0010_0000).unwrap();
        assert_eq!(v, 0b0010_0101);
        ec.write_verified(1883, 140).unwrap();
        assert_eq!(ec.read(1883).unwrap(), 140);
    }

    #[test]
    fn verify_detects_mismatch() {
        struct Liar;
        impl EcAccess for Liar {
            fn name(&self) -> &str {
                "liar"
            }
            fn read(&mut self, _: u16) -> crate::HwResult<u8> {
                Ok(0)
            }
            fn write(&mut self, _: u16, _: u8) -> crate::HwResult<()> {
                Ok(())
            }
        }
        let mut l = Liar;
        assert!(l.write_verified(1, 0x42).is_err());
    }

    #[test]
    fn ec_fail_mapping() {
        let mut ec = MockEc::new();
        ec.fail_on.push(1873);
        assert!(matches!(
            ec.read(1873),
            Err(crate::HwError::EcFail { addr: 1873 })
        ));
    }
}
