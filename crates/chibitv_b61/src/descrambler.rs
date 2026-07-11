use std::error::Error;
use std::fmt::{Debug, Display, Formatter};
use std::sync::{Arc, Mutex, mpsc};

use aes::Aes128;
use anyhow::{Result, anyhow};
use bytes::{Buf, Bytes};
use chibitv_b60::mmtp::MmtpPacket;
use ctr::Ctr128BE;
use ctr::cipher::{KeyIvInit, StreamCipher};
use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};
use sha2::{Digest, Sha256};
use tracing::{debug, error, info};

use crate::cas::CasClient;
use crate::{CasModule, EncryptionFlag};

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

fn decrypt_ecm(
    cas: &mut CasClient,
    master_key: &[u8; 32],
    rng: &mut StdRng,
    ecm: [u8; 148],
) -> Result<DecryptionKey> {
    let mut a0_init = [0u8; 8];
    rng.fill_bytes(&mut a0_init);

    let setting_data = [
        &[0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x8A, 0xF7],
        &a0_init[..],
    ]
    .concat();

    let (setting_response, ecm_response) =
        cas.scrambling_key_protection_setting_and_ecm_reception(&setting_data, &ecm)?;
    let (a0_response, a0_hash) = setting_response.setting_response_data.split_at(8);
    let kcl = Sha256::digest([&master_key[..], &a0_init[..], a0_response].concat());
    let hash = Sha256::digest([&kcl, &a0_init[..]].concat());
    assert_eq!(hash.as_slice(), a0_hash);

    let ecm_init = &ecm[0x04..0x1B];
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

type EcmSender = mpsc::SyncSender<[u8; 148]>;
type KeyReceiver = mpsc::Receiver<([u8; 148], Result<DecryptionKey>)>;

/// High-level decoder implementation for descrambling payloads.
#[derive(Clone, Debug)]
pub struct Descrambler {
    ecm_tx: EcmSender,
    key_rx: Arc<Mutex<KeyReceiver>>,
    key: Option<([u8; 148], DecryptionKey)>,
    is_async: bool,
}

impl Descrambler {
    pub fn init(module: Arc<dyn CasModule>, master_key: [u8; 32], is_async: bool) -> Result<Self> {
        let mut cas = CasClient::new(module);
        let response = cas.initial_setting_condition()?;
        debug!("CAS module initialized: {:?}", response.cas_module_id);
        let mut rng = StdRng::from_rng(&mut rand::rng());

        let (ecm_tx, ecm_rx) = mpsc::sync_channel(16);
        let (key_tx, key_rx) = mpsc::sync_channel(16);

        std::thread::spawn(move || {
            for ecm in ecm_rx {
                let key = decrypt_ecm(&mut cas, &master_key, &mut rng, ecm);
                if key_tx.send((ecm, key)).is_err() {
                    break;
                }
            }
        });

        Ok(Self {
            ecm_tx,
            key_rx: Arc::new(Mutex::new(key_rx)),
            key: None,
            is_async,
        })
    }

    /// Push an encrypted ECM to the decoder.
    /// The decoder attempts to decrypt the ECM using the CAS module.
    /// At least one ECM must be pushed before decrypting payloads.
    pub fn push_ecm(&mut self, ecm: [u8; 148]) -> Result<()> {
        if let Some((e, _)) = self.key.as_ref()
            && *e == ecm
        {
            return Ok(());
        }

        if self.is_async {
            match self.ecm_tx.try_send(ecm) {
                Ok(()) | Err(mpsc::TrySendError::Full(_)) => {}
                Err(mpsc::TrySendError::Disconnected(_)) => {
                    return Err(anyhow!("CAS worker is not running"));
                }
            }

            return Ok(());
        }

        self.ecm_tx
            .send(ecm)
            .map_err(|_| anyhow!("CAS worker is not running"))?;

        self.recv_key(true)
    }

    pub fn descramble(&mut self, mmtp_packet: &MmtpPacket, data: &mut [u8]) -> Result<()> {
        if self.is_async {
            self.recv_key(false)?;
        }

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
                let Some((_, key)) = self.key.clone() else {
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

    fn recv_key(&mut self, wait: bool) -> Result<()> {
        loop {
            let (ecm, result) = match wait {
                true => self
                    .key_rx
                    .lock()
                    .unwrap()
                    .recv()
                    .map_err(|_| anyhow!("CAS worker is not running"))?,
                false => match self.key_rx.lock().unwrap().try_recv() {
                    Ok(result) => result,
                    Err(mpsc::TryRecvError::Empty) => return Ok(()),
                    Err(mpsc::TryRecvError::Disconnected) => {
                        return Err(anyhow!("CAS worker is not running"));
                    }
                },
            };

            match result {
                Ok(key) => {
                    self.key = Some((ecm, key));
                }
                Err(error) => {
                    error!(%error, "Could not decrypt ECM");
                    if wait {
                        return Err(error);
                    }
                }
            }

            if wait {
                return Ok(());
            }
        }
    }
}
