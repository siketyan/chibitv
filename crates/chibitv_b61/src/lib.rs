//! Basic implementation for the ARIB STD-B61 standard.

mod cas;
mod descrambler;

pub use cas::CasModule;
pub use descrambler::{B61CasModule, Descrambler, NoDecryptionKeyError, SharedCasModule};

use strum::FromRepr;

#[derive(Copy, Clone, Debug, Eq, FromRepr, PartialEq)]
#[repr(u8)]
pub enum EncryptionFlag {
    Unscrambled = 0x00,
    Reserved = 0x01,
    Even = 0x02,
    Odd = 0x03,
}
