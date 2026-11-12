//! ripple-core — shared knowledge layer for the Ripple Control Center.
//!
//! Everything in this crate is derived from reverse-engineering the OEM
//! `ControlCenter` packages. It contains:
//!
//! * [`regs`] — EC register offsets & flag enums (const tables)
//! * [`protocol`] — `GetSetULong` bit encoding used by the WMI/ACPI path
//! * [`fan`] — fan curve model + `UserFanTables/*.json` parser
//! * [`rgb`] — RGB keyboard/lightbar state model + `.reg` parser
//! * [`modes`] — performance mode / power limit / battery enums
//!
//! No hardware access lives here — that is `ripple-hw`'s job.

pub mod fan;
pub mod modes;
pub mod protocol;
pub mod regs;

pub use modes::{OperatingMode, ProtectionStatus};

/// Crate-level error type.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),

    #[error("json parse: {0}")]
    Json(#[from] serde_json::Error),

    #[error("parse error: {0}")]
    Parse(String),
}

pub type Result<T> = std::result::Result<T, Error>;

/// Convenience: convert into `String` so CLI code can use `?` directly.
impl From<Error> for String {
    fn from(e: Error) -> Self {
        e.to_string()
    }
}
