use std::fmt::{Debug, Formatter};
use std::sync::Mutex;

use anyhow::{Result, anyhow};

use crate::CasModule;
use crate::multi2::Multi2;

const TS_PACKET_SIZE: usize = 188;

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

    pub fn descramble_packet(&mut self, packet: &mut [u8]) -> Result<bool> {
        if packet.len() != TS_PACKET_SIZE {
            return Err(anyhow!("invalid TS packet size"));
        }

        let scrambling_control = (packet[3] >> 6) & 0x03;
        if scrambling_control == 0 {
            return Ok(true);
        }

        let adaptation_field_control = (packet[3] >> 4) & 0x03;
        if adaptation_field_control & 0x01 == 0 {
            return Ok(true);
        }

        let mut payload_offset = 4;
        if adaptation_field_control & 0x02 != 0 {
            let adaptation_field_length = usize::from(packet[payload_offset]);
            payload_offset += 1 + adaptation_field_length;
        }

        if payload_offset >= packet.len() {
            return Ok(true);
        }

        if !self
            .multi2
            .lock()
            .unwrap()
            .decrypt(scrambling_control, &mut packet[payload_offset..])?
        {
            return Ok(false);
        }
        packet[3] &= 0x3f;

        Ok(true)
    }
}
