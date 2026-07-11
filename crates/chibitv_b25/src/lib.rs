//! Basic implementation for the ARIB STD-B25 standard.

mod cas;
mod descrambler;
mod multi2;

pub use cas::{EcmReceptionResponse, InitialSettingConditionResponse};
pub use descrambler::{B25Descrambler, NoDecryptionKeyError};

/// A physical CAS module capable of executing ARIB STD-B25 commands.
pub trait CasModule: Send + Sync {
    fn transmit(&self, command: &[u8], response: &mut [u8]) -> anyhow::Result<usize>;
}
