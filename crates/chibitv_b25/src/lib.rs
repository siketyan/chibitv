//! Basic implementation for the ARIB STD-B25 standard.

mod cas;
mod descrambler;
mod multi2;

pub use cas::{EcmReceptionResponse, InitialSettingConditionResponse};
pub use descrambler::{B25Descrambler, EcmRefusedError};
pub use multi2::NoDecryptionKeyError;

/// A physical CAS module capable of executing ARIB STD-B25 commands.
///
/// The descramblers share one behind a mutex, each asking it from a thread of its own so that
/// the stream goes on while it answers.
pub trait CasModule: Send {
    /// Sends the command to the module and waits for its response.
    fn transmit(&mut self, command: &[u8]) -> anyhow::Result<Vec<u8>>;
}
