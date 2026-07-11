use std::error::Error;
use std::fmt::{Debug, Display, Formatter};
use std::sync::{Arc, Mutex};

use anyhow::Result;
use mpeg2ts::ts::payload::Bytes;
use mpeg2ts::ts::{TransportScramblingControl, TsPacket, TsPayload};

use crate::CasModule;
use crate::cas::CasClient;
use crate::multi2::Multi2;

#[derive(Copy, Clone, Debug)]
pub struct NoDecryptionKeyError;

impl Display for NoDecryptionKeyError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "Decryption key is not provided yet")
    }
}

impl Error for NoDecryptionKeyError {}

pub struct B25Descrambler {
    cas: Mutex<CasClient>,
    multi2: Mutex<Multi2>,
    ca_system_id: u16,
}

impl Debug for B25Descrambler {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("B25Descrambler").finish()
    }
}

impl B25Descrambler {
    pub fn init(module: Arc<dyn CasModule>) -> Result<Self> {
        let mut cas = CasClient::new(module, true);
        let settings = cas.initial_setting_condition()?;

        Ok(Self {
            cas: Mutex::new(cas),
            multi2: Mutex::new(Multi2::new(settings.system_key, settings.init_cbc)),
            ca_system_id: settings.ca_system_id,
        })
    }

    pub fn ca_system_id(&self) -> u16 {
        self.ca_system_id
    }

    pub fn push_ecm(&mut self, ecm: &[u8]) -> Result<()> {
        let response = self.cas.lock().unwrap().ecm_reception(ecm)?;
        let mut key = [0u8; 16];
        key[..8].copy_from_slice(&response.odd);
        key[8..].copy_from_slice(&response.even);

        self.multi2.lock().unwrap().set_scramble_key(key);
        Ok(())
    }

    pub fn descramble(&mut self, packet: &mut TsPacket) -> Result<()> {
        let scrambling_control = packet.header.transport_scrambling_control;
        if scrambling_control == TransportScramblingControl::NotScrambled {
            return Ok(());
        }

        let Some(payload) = &mut packet.payload else {
            return Ok(());
        };

        match payload {
            TsPayload::PesStart(pes) => {
                pes.data = self.descramble_payload(scrambling_control, pes.data.as_ref())?;
            }
            TsPayload::PesContinuation(data) | TsPayload::Raw(data) => {
                *data = self.descramble_payload(scrambling_control, data.as_ref())?;
            }
            _ => {}
        }

        packet.header.transport_scrambling_control = TransportScramblingControl::NotScrambled;

        Ok(())
    }

    fn descramble_payload(
        &self,
        scrambling_control: TransportScramblingControl,
        payload: &[u8],
    ) -> Result<Bytes> {
        let mut payload = payload.to_vec();

        self.multi2
            .lock()
            .unwrap()
            .decrypt(scrambling_control, &mut payload)?;

        Ok(Bytes::new(&payload)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct FakeCasModule;

    impl CasModule for FakeCasModule {
        fn transmit(&self, command: &[u8], response: &mut [u8]) -> anyhow::Result<usize> {
            assert_eq!(&command[..4], &[0x90, 0x30, 0x00, 0x02]);

            let card_response = [
                &[0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x12, 0x34][..],
                &[0x00; 6],
                &[0x02, 0x00],
                &[0x00; 32],
                &[0x00; 8],
                &[0x00, 0x90, 0x00],
            ]
            .concat();
            response[..card_response.len()].copy_from_slice(&card_response);
            Ok(card_response.len())
        }
    }

    #[test]
    fn initializes_with_a_caller_supplied_cas_module() {
        let descrambler = B25Descrambler::init(Arc::new(FakeCasModule)).unwrap();

        assert_eq!(descrambler.ca_system_id(), 0x1234);
    }
}
