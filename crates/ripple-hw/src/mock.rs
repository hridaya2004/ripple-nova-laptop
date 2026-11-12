//! In-memory EC fake, compiled for unit tests only (never shipped in a binary).

use crate::{EcAccess, HwError, HwResult};
use std::collections::BTreeMap;

/// Simple map-backed EC used to test the `EcAccess` default methods.
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
