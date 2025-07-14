use std::collections::HashMap;
use std::error::Error;
use std::fmt::{Display, Formatter};

use anyhow::Result;
use bytes::{Buf, Bytes};
use openssl::symm::{Cipher, decrypt};
use rand::rngs::StdRng;
use rand::{RngCore, SeedableRng};
use sha2::{Digest, Sha256};
use tracing::{debug, info};

use chibitv_b60::mmtp::MmtpPacket;
use chibitv_b61::{CasModule, EncryptionFlag};

#[derive(Copy, Clone, Debug)]
pub struct NoDecryptionKeyError;

impl Display for NoDecryptionKeyError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "Decryption key is not provided yet")
    }
}

impl Error for NoDecryptionKeyError {}

#[derive(Clone, Debug)]
struct DecryptionKey {
    odd: [u8; 16],
    even: [u8; 16],
}

/// High-level decoder implementation for descrambling payloads.
#[derive(Debug)]
pub struct Descrambler {
    cas: CasModule,
    master_key: [u8; 32],
    rng: StdRng,
    key: Option<DecryptionKey>,
    key_cache: HashMap<[u8; 148], DecryptionKey>,
}

impl Descrambler {
    /// Initialize a decoder using the CAS module and the master key.
    pub fn init(mut cas: CasModule, master_key: [u8; 32]) -> Result<Self> {
        let response = cas.initial_setting_condition()?;
        debug!("CAS module initialized: {:?}", response.cas_module_id);

        Ok(Self {
            cas,
            master_key,
            rng: StdRng::from_os_rng(),
            key: None,
            key_cache: HashMap::new(),
        })
    }

    /// Push an encrypted ECM to the decoder.
    /// The decoder attempts to decrypt the ECM using the CAS module.
    /// At least one ECM must be pushed before decrypting payloads.
    pub fn push_ecm(&mut self, ecm: [u8; 148]) -> Result<()> {
        if let Some(ecm) = self.key_cache.get(&ecm) {
            self.key = Some(ecm.clone());
            return Ok(());
        }

        let mut a0_init = [0u8; 8];
        self.rng.fill_bytes(&mut a0_init);

        let setting_data = [
            &[0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x8A, 0xF7],
            &a0_init[..],
        ]
        .concat();

        let response = self.cas.scrambling_key_protection_setting(&setting_data)?;
        let (a0_response, a0_hash) = response.setting_response_data.split_at(8);
        let kcl = Sha256::digest([&self.master_key[..], &a0_init[..], a0_response].concat());
        let hash = Sha256::digest([&kcl, &a0_init[..]].concat());
        assert_eq!(hash.as_slice(), a0_hash);

        let response = self.cas.ecm_reception(&ecm)?;
        let ecm_init = &ecm[0x04..0x1B];
        let mut hash = Sha256::digest([&kcl, ecm_init].concat()).to_vec();
        for (i, byte) in hash.iter_mut().enumerate() {
            *byte ^= response.ks[i];
        }

        let (odd, even) = hash.split_at(0x10);
        info!("Decrypted ECM, Odd: {:?}, Even: {:?}", odd, even);

        let key = DecryptionKey {
            odd: odd.try_into()?,
            even: even.try_into()?,
        };

        self.key = Some(key.clone());
        self.key_cache.insert(ecm, key);

        Ok(())
    }

    pub fn descramble(&self, mmtp_packet: &MmtpPacket, data: &mut [u8]) -> Result<()> {
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
                let Some(ecm) = &self.key else {
                    return Err(NoDecryptionKeyError.into());
                };

                match encryption_flag {
                    EncryptionFlag::Even => &ecm.even[..],
                    EncryptionFlag::Odd => &ecm.odd[..],
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

        let cipher = Cipher::aes_128_ctr();
        let plaintext = decrypt(cipher, key, Some(&iv), data)?;

        data.copy_from_slice(&plaintext);

        Ok(())
    }

    pub fn clear(&mut self) {
        self.key = None;
        self.key_cache.clear();
    }
}
