//! EC access protocol encoding — the `AcpiTest_MULong.GetSetULong` /
//! `\_SB.AMW0.WMBC` call used by the OEM on both Windows and (equivalently)
//! Linux via `acpi_call`.
//!
//! ```text
//! Data (u64, little-endian bit layout):
//!   bits  0..15  = register address (u16)
//!   bits 16..31  = data to write (u16, 0 for reads)
//!   bits 32..47  = operation: 0x0100 = READ, 0x0000 = WRITE
//!   bits 48..63  = 0
//! Return (u32): low 8 bits = byte read; 0xFEFEFEFE = EC communication failure
//! ```

/// Operation code occupying bits 32..47 of `Data`: read.
pub const OP_READ: u16 = 0x0100;
/// Operation code for write (0 in the opcode field).
pub const OP_WRITE: u16 = 0x0000;
/// Returned when the EC failed to respond.
pub const EC_FAIL: u32 = 0xFEFEFEFE;

/// Encode a read of `addr`.
pub fn encode_read(addr: u16) -> u64 {
    (addr as u64) | ((OP_READ as u64) << 32)
}

/// Encode a write of `value` to `addr`.
pub fn encode_write(addr: u16, value: u8) -> u64 {
    (addr as u64) | ((value as u64) << 16) | ((OP_WRITE as u64) << 32)
}

/// Decode the `Return` value of a read. `None` signals EC failure.
pub fn decode_read(ret: u32) -> Option<u8> {
    if ret == EC_FAIL {
        None
    } else {
        Some((ret & 0xFF) as u8)
    }
}

/// The ACPI `WMBC` argument when only encoding the value (some backends pass
/// `addr` and the value separately — provided for completeness).
pub fn wmbc_read(addr: u16) -> u64 {
    encode_read(addr)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_encoding() {
        // documented example: read at 1873 (fan control byte)
        let d = encode_read(1873);
        assert_eq!(d & 0xFFFF, 1873);
        assert_eq!((d >> 32) & 0xFFFF, 0x0100);
        assert_eq!((d >> 16) & 0xFFFF, 0);
        assert_eq!(d, 1873u64 | 0x100_0000_0000);
    }

    #[test]
    fn write_encoding() {
        let d = encode_write(1883, 140);
        assert_eq!(d & 0xFFFF, 1883);
        assert_eq!((d >> 16) & 0xFFFF, 140);
        assert_eq!((d >> 32) & 0xFFFF, 0);
    }

    #[test]
    fn failure_code() {
        assert_eq!(decode_read(0xFEFEFEFE), None);
        assert_eq!(decode_read(0x42), Some(0x42));
        assert_eq!(decode_read(0xFFFF_FF42), Some(0x42));
    }
}
