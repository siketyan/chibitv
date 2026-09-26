use std::error::Error;
use std::fmt::{Debug, Display, Formatter};
use std::sync::{Arc, Mutex};

use aes::Aes128;
use anyhow::Result;
use bytes::{Buf, Bytes};
use chibitv_b60::mmtp::MmtpPacket;
use ctr::Ctr128BE;
use ctr::cipher::{KeyIvInit, StreamCipher};
use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};
use sha2::{Digest, Sha256};
use tracing::{debug, error, info};

use crate::cas::{self, CasClient};
use crate::{CasModule, EncryptionFlag};

#[derive(Copy, Clone, Debug)]
pub struct NoDecryptionKeyError;

impl Display for NoDecryptionKeyError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "Decryption key is not provided yet")
    }
}

impl Error for NoDecryptionKeyError {}

/// The codes the ECM reception command answers with when it hands over a key
/// that descrambles: the programme has been purchased, on a tier or as a
/// conditional access pay-per-view one, or it is being previewed.
///
/// ACAS numbers these its own way rather than the way B-CAS does, having one
/// kind of pay-per-view programme where the older card has two.
const VIEWABLE_RETURN_CODES: [u16; 3] = [0x0600, 0x0800, 0x4680];

/// The codes it answers with when no contract covers the programme: the card
/// holds no key for it at all, or the contract does not reach the tier or the
/// pay-per-view programme it is broadcast in, has run out, or is restricted.
const NOT_CONTRACTED_RETURN_CODES: [u16; 7] =
    [0x8701, 0x8702, 0x8703, 0x8901, 0x8902, 0x8903, 0xA103];

/// The card handed over no key to descramble a programme with.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct EcmRefusedError {
    /// What the card answered, as ARIB STD-B61 numbers it.
    pub return_code: u16,
}

impl EcmRefusedError {
    /// Whether the card refused because no contract covers the programme,
    /// rather than because it would not sell it or could not make sense of the
    /// ECM.
    pub fn is_not_contracted(&self) -> bool {
        NOT_CONTRACTED_RETURN_CODES.contains(&self.return_code)
    }
}

impl Display for EcmRefusedError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        let reason = if self.is_not_contracted() {
            "the card holds no contract for this programme"
        } else {
            "the card handed over no key"
        };

        write!(f, "{reason} ({:#06X})", self.return_code)
    }
}

impl Error for EcmRefusedError {}

#[derive(Clone, Debug)]
struct DecryptionKey {
    odd: [u8; 16],
    even: [u8; 16],
}

/// An ECM the card is working on, with what it takes to make sense of the answer.
#[derive(Debug)]
struct PendingEcm {
    ecm: [u8; 148],
    a0_init: [u8; 8],
}

fn decryption_key(
    master_key: &[u8; 32],
    pending: &PendingEcm,
    responses: &[Vec<u8>],
) -> Result<DecryptionKey> {
    let (setting_response, ecm_response) =
        cas::read_scrambling_key_protection_setting_and_ecm_reception(responses)?;
    if !VIEWABLE_RETURN_CODES.contains(&ecm_response.return_code) {
        return Err(EcmRefusedError {
            return_code: ecm_response.return_code,
        }
        .into());
    }

    let (a0_response, a0_hash) = setting_response.setting_response_data.split_at(8);
    let kcl = Sha256::digest([&master_key[..], &pending.a0_init[..], a0_response].concat());
    let hash = Sha256::digest([&kcl, &pending.a0_init[..]].concat());
    assert_eq!(hash.as_slice(), a0_hash);

    let ecm_init = &pending.ecm[0x04..0x1B];
    let mut hash = Sha256::digest([&kcl, ecm_init].concat()).to_vec();
    for (i, byte) in hash.iter_mut().enumerate() {
        *byte ^= ecm_response.ks[i];
    }

    let (odd, even) = hash.split_at(0x10);
    info!(
        "Decrypted ECM, Odd: {}, Even: {}",
        hex::encode(odd),
        hex::encode(even),
    );

    Ok(DecryptionKey {
        odd: odd.try_into()?,
        even: even.try_into()?,
    })
}

/// High-level decoder implementation for descrambling payloads.
#[derive(Debug)]
pub struct Descrambler {
    cas: CasClient,
    master_key: [u8; 32],
    rng: StdRng,
    key: Option<DecryptionKey>,
    /// The ECM last handed to the card, so that the same one sent again is not asked about twice.
    last_ecm: Option<[u8; 148]>,
    /// The ECM last sent to the card, until its answer arrives.
    pending: Option<PendingEcm>,
    /// Whether the stream goes on while the card works on an ECM, rather than waiting for it.
    is_async: bool,
}

impl Descrambler {
    pub fn init(
        module: Arc<Mutex<dyn CasModule>>,
        master_key: [u8; 32],
        is_async: bool,
    ) -> Result<Self> {
        let mut cas = CasClient::new(module);
        let response = cas.initial_setting_condition()?;
        debug!("CAS module initialized: {:?}", response.cas_module_id);

        Ok(Self {
            cas,
            master_key,
            rng: StdRng::from_rng(&mut rand::rng()),
            key: None,
            last_ecm: None,
            pending: None,
            is_async,
        })
    }

    /// Push an encrypted ECM to the decoder.
    /// The decoder attempts to decrypt the ECM using the CAS module.
    /// At least one ECM must be pushed before decrypting payloads.
    pub fn push_ecm(&mut self, ecm: [u8; 148]) -> Result<()> {
        self.recv_key(false)?;
        if self.last_ecm == Some(ecm) {
            return Ok(());
        }

        let mut a0_init = [0u8; 8];
        self.rng.fill_bytes(&mut a0_init);
        let setting_data = [
            &[0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x8A, 0xF7],
            &a0_init[..],
        ]
        .concat();

        // Whatever the card has yet to answer about an older ECM is not waited for.
        self.cas
            .scrambling_key_protection_setting_and_ecm_reception(&setting_data, &ecm);
        self.pending = Some(PendingEcm { ecm, a0_init });
        self.last_ecm = Some(ecm);
        if self.is_async {
            return Ok(());
        }

        self.recv_key(true)
    }

    pub fn descramble(&mut self, mmtp_packet: &MmtpPacket, data: &mut [u8]) -> Result<()> {
        self.recv_key(false)?;

        let encryption_flag = mmtp_packet
            .extension_header
            .as_ref()
            .and_then(|header| {
                assert_eq!(header.header_type, 0x0000);

                let mut reader = Bytes::copy_from_slice(&header.data);
                let extension_type = reader.get_u16();
                if (extension_type & 0x7FFF) != 0x0001 {
                    return None;
                }

                let extension_length = reader.get_u16();
                assert_eq!(extension_length, 1);

                let extension_payload = reader.get_u8();
                assert_eq!(extension_payload & 0b0000_0010, 0); // MAC
                assert_eq!(extension_payload & 0b0000_0001, 0); // SICV

                EncryptionFlag::from_repr((extension_payload & 0b0001_1000) >> 3)
            })
            .unwrap_or(EncryptionFlag::Unscrambled);

        let key = match encryption_flag {
            EncryptionFlag::Even | EncryptionFlag::Odd => {
                let Some(key) = self.key.clone() else {
                    return Err(NoDecryptionKeyError.into());
                };

                match encryption_flag {
                    EncryptionFlag::Even => key.even,
                    EncryptionFlag::Odd => key.odd,
                    _ => unreachable!(),
                }
            }
            _ => return Ok(()),
        };

        let iv = [
            &mmtp_packet.packet_id.to_be_bytes()[..],
            &mmtp_packet.packet_sequence_number.to_be_bytes()[..],
            &[0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00],
        ]
        .concat();

        let mut ctr = Ctr128BE::<Aes128>::new_from_slices(&key, &iv)?;

        ctr.apply_keystream(data);

        Ok(())
    }

    /// Takes the key the card has answered with, waiting for it if `wait` is set.
    fn recv_key(&mut self, wait: bool) -> Result<()> {
        let Some(result) = self.cas.receive(wait) else {
            return Ok(());
        };
        let Some(pending) = &self.pending else {
            return Ok(());
        };
        let result =
            result.and_then(|responses| decryption_key(&self.master_key, pending, &responses));
        self.pending = None;

        match result {
            Ok(key) => self.key = Some(key),
            Err(error) if error.is::<EcmRefusedError>() => return Err(error),
            Err(error) => {
                // The card may answer the same ECM next time.
                self.last_ecm = None;
                if wait {
                    return Err(error);
                }
                error!(%error, "Could not decrypt ECM");
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reports_a_programme_the_card_holds_no_contract_for() {
        let refusal = EcmRefusedError {
            return_code: 0xA103,
        };

        assert!(refusal.is_not_contracted());
        assert_eq!(
            refusal.to_string(),
            "the card holds no contract for this programme (0xA103)"
        );
    }

    #[test]
    fn reports_a_card_that_could_not_make_sense_of_the_ecm() {
        let refusal = EcmRefusedError {
            return_code: 0xA106,
        };

        assert!(!refusal.is_not_contracted());
        assert_eq!(refusal.to_string(), "the card handed over no key (0xA106)");
    }

    #[test]
    fn keeps_the_two_sets_of_return_codes_apart() {
        // Bought on a tier, bought as a pay-per-view programme, and being
        // previewed.
        assert_eq!(VIEWABLE_RETURN_CODES, [0x0600, 0x0800, 0x4680]);
        assert!(
            !VIEWABLE_RETURN_CODES
                .iter()
                .any(|code| NOT_CONTRACTED_RETURN_CODES.contains(code))
        );

        // A contract that ran out on the tier it is broadcast in, and one that
        // never reached the pay-per-view programme.
        assert!(
            EcmRefusedError {
                return_code: 0x8902
            }
            .is_not_contracted()
        );
        assert!(
            EcmRefusedError {
                return_code: 0x8701
            }
            .is_not_contracted()
        );
        // Out of the preview of a programme that is there to be bought.
        assert!(
            !EcmRefusedError {
                return_code: 0x8700
            }
            .is_not_contracted()
        );
    }
}
