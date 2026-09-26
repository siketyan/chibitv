//! Basic implementation for the ARIB STD-B61 standard.

mod cas;
mod descrambler;

pub use descrambler::{Descrambler, EcmRefusedError, NoDecryptionKeyError};

use strum::FromRepr;

/// A physical CAS module capable of executing ARIB STD-B61 commands.
///
/// The descramblers share one behind a mutex, each asking it from a thread of its own so that
/// the stream goes on while it answers.
pub trait CasModule: Send {
    /// Sends the command to the module and waits for its response.
    fn transmit(&mut self, command: &[u8]) -> anyhow::Result<Vec<u8>>;
}

#[derive(Copy, Clone, Debug, Eq, FromRepr, PartialEq)]
#[repr(u8)]
pub enum EncryptionFlag {
    Unscrambled = 0x00,
    Reserved = 0x01,
    Even = 0x02,
    Odd = 0x03,
}
