//! Basic implementation for the ARIB STD-B61 standard.

mod cas;
mod descrambler;

pub use descrambler::{Descrambler, EcmRefusedError, NoDecryptionKeyError};

use std::sync::mpsc;

use strum::FromRepr;

/// The responses to a run of commands, one for each, arriving once the CAS module has answered
/// the last of them.
pub type PendingResponses = mpsc::Receiver<anyhow::Result<Vec<Vec<u8>>>>;

/// A physical CAS module capable of executing ARIB STD-B61 commands.
pub trait CasModule: Send + Sync {
    /// Sends the commands to the module one after another, with nothing else sent to it in
    /// between, and returns without waiting for it to answer.
    fn transmit(&self, commands: Vec<Vec<u8>>) -> PendingResponses;
}

#[derive(Copy, Clone, Debug, Eq, FromRepr, PartialEq)]
#[repr(u8)]
pub enum EncryptionFlag {
    Unscrambled = 0x00,
    Reserved = 0x01,
    Even = 0x02,
    Odd = 0x03,
}
