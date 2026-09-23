//! Basic implementation for the ARIB STD-B25 standard.

mod cas;
mod descrambler;
mod multi2;

pub use cas::{EcmReceptionResponse, InitialSettingConditionResponse};
pub use descrambler::{B25Descrambler, EcmRefusedError, NoDecryptionKeyError};

use std::sync::mpsc;

/// The responses to a run of commands, one for each, arriving once the CAS module has answered
/// the last of them.
pub type PendingResponses = mpsc::Receiver<anyhow::Result<Vec<Vec<u8>>>>;

/// A physical CAS module capable of executing ARIB STD-B25 commands.
pub trait CasModule: Send + Sync {
    /// Sends the commands to the module one after another, with nothing else sent to it in
    /// between, and returns without waiting for it to answer.
    fn transmit(&self, commands: Vec<Vec<u8>>) -> PendingResponses;
}
