use std::error::Error;
use std::fmt::{Debug, Display, Formatter};
use std::sync::Arc;

use anyhow::Result;
use mpeg2ts::ts::payload::Bytes;
use mpeg2ts::ts::{TransportScramblingControl, TsPacket, TsPayload};
use tracing::error;

use crate::cas::{self, CasClient, EcmReceptionResponse};
use crate::multi2::Multi2;
use crate::{CasModule, PendingResponses};

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

fn scramble_key(responses: &[Vec<u8>]) -> Result<[u8; 16]> {
    let response = EcmReceptionResponse::read(&responses[0])?;
    if !VIEWABLE_RETURN_CODES.contains(&response.return_code) {
        return Err(EcmRefusedError {
            return_code: response.return_code,
        }
        .into());
    }

    let mut key = [0u8; 16];
    key[..8].copy_from_slice(&response.odd);
    key[8..].copy_from_slice(&response.even);
    Ok(key)
}

pub struct B25Descrambler {
    cas: CasClient,
    multi2: Multi2,
    ca_system_id: u16,
    /// The ECM last handed to the card, so that the same one sent again is not asked about twice.
    last_ecm: Option<Vec<u8>>,
    /// The card's answer to the last ECM, until it arrives.
    pending: Option<PendingResponses>,
    /// Whether the stream goes on while the card works on an ECM, rather than waiting for it.
    is_async: bool,
}

impl Debug for B25Descrambler {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("B25Descrambler").finish()
    }
}

impl B25Descrambler {
    pub fn init(module: Arc<dyn CasModule>, is_async: bool) -> Result<Self> {
        let cas = CasClient::new(module, true);
        let settings = cas.initial_setting_condition()?;

        Ok(Self {
            cas,
            multi2: Multi2::new(settings.system_key, settings.init_cbc),
            ca_system_id: settings.ca_system_id,
            last_ecm: None,
            pending: None,
            is_async,
        })
    }

    pub fn ca_system_id(&self) -> u16 {
        self.ca_system_id
    }

    pub fn push_ecm(&mut self, ecm: &[u8]) -> Result<()> {
        self.recv_key(false)?;
        if self.last_ecm.as_deref() == Some(ecm) {
            return Ok(());
        }

        // Whatever the card has yet to answer about an older ECM is not waited for.
        self.pending = Some(self.cas.ecm_reception(ecm));
        self.last_ecm = Some(ecm.to_vec());
        if self.is_async {
            return Ok(());
        }

        self.recv_key(true)
    }

    pub fn descramble(&mut self, packet: &mut TsPacket) -> Result<()> {
        self.recv_key(false)?;

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
        self.multi2.decrypt(scrambling_control, &mut payload)?;

        Ok(Bytes::new(&payload)?)
    }

    /// Takes the key the card has answered with, waiting for it if `wait` is set.
    fn recv_key(&mut self, wait: bool) -> Result<()> {
        let Some(pending) = &self.pending else {
            return Ok(());
        };
        let Some(result) = cas::receive(pending, wait) else {
            return Ok(());
        };
        self.pending = None;

        match result.and_then(|responses| scramble_key(&responses)) {
            Ok(key) => self.multi2.set_scramble_key(key),
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

    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Mutex, mpsc};

    /// An answer to an ECM held back, and where it goes once let through.
    type HeldAnswer = (mpsc::SyncSender<Result<Vec<Vec<u8>>>>, Vec<Vec<u8>>);

    /// A card that answers the ECM with whatever return code the test is about.
    #[derive(Default)]
    struct FakeCasModule {
        ecm_return_code: u16,
        ecm_count: AtomicUsize,
        /// Whether its answers to ECMs are held back until the test lets them through.
        holds_ecm: bool,
        held: Mutex<Vec<HeldAnswer>>,
    }

    impl FakeCasModule {
        fn answer(&self, command: &[u8]) -> Vec<u8> {
            match command[..4] {
                [0x90, 0x30, 0x00, 0x02] => [
                    &[0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x12, 0x34][..],
                    &[0x00; 6],
                    &[0x02, 0x00],
                    &[0x00; 32],
                    &[0x00; 8],
                    &[0x00, 0x90, 0x00],
                ]
                .concat(),
                [0x90, 0x34, 0x00, 0x02] => {
                    self.ecm_count.fetch_add(1, Ordering::Relaxed);
                    [
                        // protocol unit number, unit length, card instruction
                        &[0x00, 0x00, 0x00, 0x00][..],
                        &self.ecm_return_code.to_be_bytes(),
                        &[0x11; 8], // odd
                        &[0x22; 8], // even
                        &[0x00],    // recording control
                        &[0x90, 0x00],
                    ]
                    .concat()
                }
                ref command => panic!("the card was sent {command:02X?}"),
            }
        }

        fn let_ecm_answers_through(&self) {
            for (tx, responses) in self.held.lock().unwrap().drain(..) {
                tx.send(Ok(responses)).unwrap();
            }
        }
    }

    impl CasModule for FakeCasModule {
        fn transmit(&self, commands: Vec<Vec<u8>>) -> PendingResponses {
            let (tx, rx) = mpsc::sync_channel(1);
            let responses = commands
                .iter()
                .map(|command| self.answer(command))
                .collect();
            if self.holds_ecm && commands[0][1] == 0x34 {
                self.held.lock().unwrap().push((tx, responses));
            } else {
                tx.send(Ok(responses)).unwrap();
            }
            rx
        }
    }

    fn descrambler(ecm_return_code: u16) -> B25Descrambler {
        B25Descrambler::init(
            Arc::new(FakeCasModule {
                ecm_return_code,
                ..Default::default()
            }),
            false,
        )
        .unwrap()
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

    #[test]
    fn asks_the_card_about_an_ecm_sent_again_only_once() {
        let module = Arc::new(FakeCasModule {
            ecm_return_code: 0x0800,
            ..Default::default()
        });
        let mut descrambler = B25Descrambler::init(module.clone(), false).unwrap();

        descrambler.push_ecm(&[0u8; 8]).unwrap();
        descrambler.push_ecm(&[0u8; 8]).unwrap();
        descrambler.push_ecm(&[1u8; 8]).unwrap();

        assert_eq!(module.ecm_count.load(Ordering::Relaxed), 2);
    }

    #[test]
    fn goes_on_while_the_card_works_on_the_ecm_when_asynchronous() {
        let module = Arc::new(FakeCasModule {
            ecm_return_code: 0x0800,
            holds_ecm: true,
            ..Default::default()
        });
        let mut descrambler = B25Descrambler::init(module.clone(), true).unwrap();

        descrambler.push_ecm(&[0u8; 8]).unwrap();
        descrambler.recv_key(false).unwrap();
        assert!(!descrambler.multi2.has_key());

        module.let_ecm_answers_through();
        descrambler.recv_key(false).unwrap();
        assert!(descrambler.multi2.has_key());
    }

    #[test]
    fn reports_a_refusal_after_the_fact_when_asynchronous() {
        let module = Arc::new(FakeCasModule {
            ecm_return_code: 0xA103,
            ..Default::default()
        });
        let mut descrambler = B25Descrambler::init(module, true).unwrap();

        descrambler.push_ecm(&[0u8; 8]).unwrap();
        let error = descrambler.recv_key(false).unwrap_err();
        assert!(error.is::<EcmRefusedError>());
    }
}
