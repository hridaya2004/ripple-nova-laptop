//! Hardware abstraction for EC / platform access.
//!
//! * [`EcAccess`] — the trait every backend implements (byte r/w with pacing,
//!   read-before-write and retry safety rules).
//! * [`MockEc`] — in-memory backend for development & tests.
//! * `AcpiCallEc` / `WmiEc` — Linux backends (behind the `linux` module).

pub mod mock;

pub use mock::MockEc;

use std::time::Duration;

/// Errors from a hardware backend.
#[derive(Debug, thiserror::Error)]
pub enum HwError {
    #[error("backend unavailable: {0}")]
    Unavailable(String),

    #[error("ec communication failure (0xFEFEFEFE) at addr {addr}")]
    EcFail { addr: u16 },

    #[error("permission denied accessing {0}")]
    Permission(String),

    #[error("io: {0}")]
    Io(#[from] std::io::Error),

    #[error("{0}")]
    Other(String),
}

pub type HwResult<T> = Result<T, HwError>;

/// Convenience: convert into `String` so CLI code can use `?` directly.
impl From<HwError> for String {
    fn from(e: HwError) -> Self {
        e.to_string()
    }
}

/// OEM pacing: wait 6 ms after every EC transaction.
pub const EC_PACE: Duration = Duration::from_millis(6);
/// Retries before giving up on an EC-failure.
pub const EC_RETRIES: u32 = 3;

/// Byte-granular EC register access.
///
/// Implementations should apply [`EC_PACE`] between transactions and map
/// the `0xFEFEFEFE` failure code to [`HwError::EcFail`].
pub trait EcAccess: Send {
    /// Human-readable backend name (for `rcc doctor`).
    fn name(&self) -> &str;

    /// Read one byte at `addr`.
    fn read(&mut self, addr: u16) -> HwResult<u8>;

    /// Write one byte at `addr`.
    fn write(&mut self, addr: u16, value: u8) -> HwResult<()>;

    /// Read-modify-write, preserving bits not in `mask`.
    fn update(&mut self, addr: u16, mask: u8, value: u8) -> HwResult<u8> {
        let cur = self.read(addr)?;
        let new = (cur & !mask) | (value & mask);
        if new != cur {
            self.write(addr, new)?;
        }
        Ok(new)
    }

    /// Read `len` consecutive bytes.
    fn read_block(&mut self, addr: u16, len: usize) -> HwResult<Vec<u8>> {
        let mut out = Vec::with_capacity(len);
        for i in 0..len {
            out.push(self.read(addr.wrapping_add(i as u16))?);
        }
        Ok(out)
    }

    /// Write `data` at `addr` and read back to verify.
    fn write_verified(&mut self, addr: u16, value: u8) -> HwResult<()> {
        self.write(addr, value)?;
        let back = self.read(addr)?;
        if back != value {
            return Err(HwError::Other(format!(
                "verify failed at {addr}: wrote {value:#04x}, read {back:#04x}"
            )));
        }
        Ok(())
    }
}
