use std::fmt::{Debug, Formatter};
use std::sync::Mutex;

use anyhow::Result;
use mpeg2ts::ts::payload::Bytes;
use mpeg2ts::ts::{TransportScramblingControl, TsPacket, TsPayload};

use crate::CasModule;
use crate::multi2::Multi2;

pub struct B25Descrambler {
    cas: Mutex<CasModule>,
    ca_system_id: u16,
    multi2: Mutex<Multi2>,
}

impl Debug for B25Descrambler {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("B25Descrambler").finish()
    }
}

impl B25Descrambler {
    pub fn open() -> Result<Self> {
        let mut cas = CasModule::open_acas()?;
        let settings = cas.initial_setting_condition()?;

        Ok(Self {
            cas: Mutex::new(cas),
            ca_system_id: settings.ca_system_id,
            multi2: Mutex::new(Multi2::new(settings.system_key, settings.init_cbc)),
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
                if let Some(data) =
                    self.descramble_payload(scrambling_control, pes.data.as_ref())?
                {
                    pes.data = data;
                }
            }
            TsPayload::PesContinuation(data) | TsPayload::Raw(data) => {
                if let Some(descrambled) =
                    self.descramble_payload(scrambling_control, data.as_ref())?
                {
                    *data = descrambled;
                };
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
    ) -> Result<Option<Bytes>> {
        let mut payload = payload.to_vec();
        if !self
            .multi2
            .lock()
            .unwrap()
            .decrypt(scrambling_control, &mut payload)?
        {
            return Ok(None);
        }

        Ok(Some(Bytes::new(&payload)?))
    }
}
