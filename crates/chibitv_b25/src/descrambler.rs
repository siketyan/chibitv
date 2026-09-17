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

/// The codes the ECM reception command answers with when it hands over a key
/// that descrambles: the programme has been purchased, or it is being
/// previewed.
const VIEWABLE_RETURN_CODES: [u16; 5] = [0x0200, 0x0400, 0x0800, 0x4280, 0x4480];

/// The codes it answers with when no contract covers the programme: the card
/// holds no key for it at all, or the contract does not reach the tier or the
/// pay-per-view programme it is broadcast in, has run out, or is restricted.
const NOT_CONTRACTED_RETURN_CODES: [u16; 10] = [
    0x8301, 0x8302, 0x8303, 0x8501, 0x8502, 0x8503, 0x8901, 0x8902, 0x8903, 0xA103,
];

/// The card handed over no key to descramble a programme with.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct EcmRefusedError {
    /// What the card answered, as ARIB STD-B25 numbers it.
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
        if !VIEWABLE_RETURN_CODES.contains(&response.return_code) {
            return Err(EcmRefusedError {
                return_code: response.return_code,
            }
            .into());
        }

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

    /// A card that answers the ECM with whatever return code the test is about.
    struct FakeCasModule {
        ecm_return_code: u16,
    }

    impl CasModule for FakeCasModule {
        fn transmit(&self, command: &[u8], response: &mut [u8]) -> anyhow::Result<usize> {
            let card_response = match command[..4] {
                [0x90, 0x30, 0x00, 0x02] => [
                    &[0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x12, 0x34][..],
                    &[0x00; 6],
                    &[0x02, 0x00],
                    &[0x00; 32],
                    &[0x00; 8],
                    &[0x00, 0x90, 0x00],
                ]
                .concat(),
                [0x90, 0x34, 0x00, 0x02] => [
                    // protocol unit number, unit length, card instruction
                    &[0x00, 0x00, 0x00, 0x00][..],
                    &self.ecm_return_code.to_be_bytes(),
                    &[0x11; 8], // odd
                    &[0x22; 8], // even
                    &[0x00],    // recording control
                    &[0x90, 0x00],
                ]
                .concat(),
                ref command => panic!("the card was sent {command:02X?}"),
            };

            response[..card_response.len()].copy_from_slice(&card_response);
            Ok(card_response.len())
        }
    }

    fn descrambler(ecm_return_code: u16) -> B25Descrambler {
        B25Descrambler::init(Arc::new(FakeCasModule { ecm_return_code })).unwrap()
    }

    fn refusal(ecm_return_code: u16) -> EcmRefusedError {
        let error = descrambler(ecm_return_code)
            .push_ecm(&[0u8; 8])
            .unwrap_err();

        *error
            .downcast_ref::<EcmRefusedError>()
            .unwrap_or_else(|| panic!("the card was refused with {error}"))
    }

    #[test]
    fn initializes_with_a_caller_supplied_cas_module() {
        assert_eq!(descrambler(0x0800).ca_system_id(), 0x1234);
    }

    #[test]
    fn takes_the_key_of_a_programme_the_card_lets_through() {
        // Bought, on a tier or as either kind of pay-per-view programme, and
        // being previewed.
        for return_code in [0x0800, 0x0400, 0x0200, 0x4480, 0x4280] {
            assert!(descrambler(return_code).push_ecm(&[0u8; 8]).is_ok());
        }
    }

    #[test]
    fn reports_a_programme_the_card_holds_no_contract_for() {
        // The card holds no key for it at all, which is what it answers a
        // channel nobody subscribed to with.
        let no_key = refusal(0xA103);

        assert!(no_key.is_not_contracted());
        assert_eq!(
            no_key.to_string(),
            "the card holds no contract for this programme (0xA103)"
        );

        // A contract that ran out on the tier it is broadcast in is the same
        // answer by another name.
        assert!(refusal(0x8902).is_not_contracted());
    }

    #[test]
    fn reports_a_card_that_could_not_make_sense_of_the_ecm() {
        let refusal = refusal(0xA106);

        assert!(!refusal.is_not_contracted());
        assert_eq!(refusal.to_string(), "the card handed over no key (0xA106)");
    }

    #[test]
    fn reports_a_programme_the_card_would_not_sell() {
        // Outside the preview of a programme that is there to be bought.
        let refusal = refusal(0x8500);

        assert!(!refusal.is_not_contracted());
        assert_eq!(refusal.to_string(), "the card handed over no key (0x8500)");
    }
}
