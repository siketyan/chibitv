//! Basic implementation for the ARIB STD-B61 standard.

mod cas;
mod descrambler;

pub use descrambler::{Descrambler, NoDecryptionKeyError};

/// A physical CAS module capable of executing ARIB STD-B61 commands.
pub trait CasModule: Send + Sync {
    /// Executes a single command while holding the module lock.
    fn transmit(&self, command: &[u8], response: &mut [u8]) -> anyhow::Result<usize>;

    /// Locks the module for a sequence of commands that must not be interleaved.
    fn lock(&self) -> anyhow::Result<Box<dyn CasModuleGuard + '_>>;
}

/// Exclusive access to a CAS module for an arbitrary command sequence.
pub trait CasModuleGuard {
    fn transmit(&mut self, command: &[u8], response: &mut [u8]) -> anyhow::Result<usize>;
}

use strum::FromRepr;

#[derive(Copy, Clone, Debug, Eq, FromRepr, PartialEq)]
#[repr(u8)]
pub enum EncryptionFlag {
    Unscrambled = 0x00,
    Reserved = 0x01,
    Even = 0x02,
    Odd = 0x03,
}
