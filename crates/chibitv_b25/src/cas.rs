//! APDU commands and responses implemented on an ARIB STD-B25 CAS module.

use std::fmt::{Debug, Formatter};
use std::io::{Cursor, ErrorKind, Read, Result};

use anyhow::bail;
use apdu_core::{Command, Response};
use byteorder::{BE, ReadBytesExt};
use pcsc::{Card, Context, Protocols, Scope, ShareMode};
use strum::FromRepr;
use tracing::debug;

trait ReadExt: Read {
    fn read_byte_array<const N: usize>(&mut self) -> Result<[u8; N]> {
        let mut array = [0u8; N];
        self.read_exact(&mut array)?;
        Ok(array)
    }
}

impl<T: Read> ReadExt for T {}

#[derive(Clone, Debug)]
pub struct InitialSettingConditionCommand {
    pub acas: bool,
}

impl InitialSettingConditionCommand {
    pub(crate) fn write(&self, buf: &mut [u8]) -> usize {
        let p2 = if self.acas { 0x02 } else { 0x00 };
        let cmd = Command::new_with_le(0x90, 0x30, 0x00, p2, 0x00);
        cmd.write(buf);
        cmd.len()
    }
}

#[derive(Copy, Clone, Debug, Default, Eq, FromRepr, Hash, PartialEq)]
#[repr(u8)]
pub enum CardType {
    Prepaid = 0x00,
    #[default]
    Standard = 0x01,
    Acas = 0x02,
}

#[derive(Clone, Debug)]
pub struct InitialSettingConditionResponse {
    pub unit_length: u8,
    pub card_instruction: u16,
    pub return_code: u16,
    pub ca_system_id: u16,
    pub card_id: [u8; 6],
    pub card_type: CardType,
    pub message_division_length: u8,
    pub system_key: [u8; 32],
    pub init_cbc: [u8; 8],
    pub system_management_ids: Vec<u16>,
}

impl InitialSettingConditionResponse {
    fn read(buf: &[u8]) -> Result<Self> {
        let response = Response::from(buf);
        assert!(response.is_ok());

        let mut reader = Cursor::new(response.payload);

        let protocol_unit_number = reader.read_u8()?;
        assert_eq!(protocol_unit_number, 0x00);

        let unit_length = reader.read_u8()?;
        let card_instruction = reader.read_u16::<BE>()?;
        let return_code = reader.read_u16::<BE>()?;
        let ca_system_id = reader.read_u16::<BE>()?;
        let card_id = reader.read_byte_array()?;
        let card_type = reader.read_u8()?;
        let message_division_length = reader.read_u8()?;
        let system_key = reader.read_byte_array()?;
        let init_cbc = reader.read_byte_array()?;
        let system_management_id_len = reader.read_u8()?;

        let mut system_management_ids = Vec::with_capacity(system_management_id_len as usize);
        for _ in 0..system_management_id_len {
            system_management_ids.push(reader.read_u16::<BE>()?);
        }

        Ok(Self {
            unit_length,
            card_instruction,
            return_code,
            ca_system_id,
            card_id,
            card_type: CardType::from_repr(card_type).ok_or(ErrorKind::InvalidData)?,
            message_division_length,
            system_key,
            init_cbc,
            system_management_ids,
        })
    }
}

#[derive(Clone, Debug)]
pub struct EcmReceptionCommand {
    pub ecm: Vec<u8>,
    pub acas: bool,
}

impl EcmReceptionCommand {
    fn write(&self, buf: &mut [u8]) -> usize {
        let p2 = if self.acas { 0x02 } else { 0x00 };
        let cmd = Command::new_with_payload_le(0x90, 0x34, 0x00, p2, 0x00, &self.ecm);
        cmd.write(buf);
        cmd.len()
    }
}

#[derive(Clone, Debug)]
pub struct EcmReceptionResponse {
    pub unit_length: u8,
    pub card_instruction: u16,
    pub return_code: u16,
    pub odd: [u8; 8],
    pub even: [u8; 8],
    pub recording_control: u8,
}

impl EcmReceptionResponse {
    fn read(buf: &[u8]) -> Result<Self> {
        let response = Response::from(buf);
        assert!(response.is_ok());

        let mut reader = Cursor::new(response.payload);

        let protocol_unit_number = reader.read_u8()?;
        assert_eq!(protocol_unit_number, 0x00);

        Ok(Self {
            unit_length: reader.read_u8()?,
            card_instruction: reader.read_u16::<BE>()?,
            return_code: reader.read_u16::<BE>()?,
            odd: reader.read_byte_array()?,
            even: reader.read_byte_array()?,
            recording_control: reader.read_u8()?,
        })
    }
}

/// A CAS module, backed by the PC/SC interface.
pub struct CasModule {
    card: Card,
    acas: bool,
    tx_buf: Vec<u8>,
    rx_buf: Vec<u8>,
}

impl Debug for CasModule {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CasModule").finish()
    }
}

impl CasModule {
    pub fn open() -> anyhow::Result<Self> {
        Self::open_with_acas_mode(false)
    }

    pub fn open_acas() -> anyhow::Result<Self> {
        Self::open_with_acas_mode(true)
    }

    pub fn open_with_acas_mode(acas: bool) -> anyhow::Result<Self> {
        let ctx = Context::establish(Scope::System)?;
        let mut buf = vec![0u8; 4096];
        let Some(reader_name) = ctx.list_readers(&mut buf)?.next() else {
            bail!("reader not found");
        };

        debug!(
            "Using reader: {}",
            String::from_utf8_lossy(reader_name.to_bytes())
        );

        let card = ctx.connect(reader_name, ShareMode::Shared, Protocols::ANY)?;

        Ok(Self {
            card,
            acas,
            tx_buf: vec![0u8; 2048],
            rx_buf: vec![0u8; 4096],
        })
    }

    pub fn initial_setting_condition(&mut self) -> anyhow::Result<InitialSettingConditionResponse> {
        let cmd = InitialSettingConditionCommand { acas: self.acas };
        let len = cmd.write(&mut self.tx_buf);
        let response = self.card.transmit(&self.tx_buf[..len], &mut self.rx_buf)?;
        let response = InitialSettingConditionResponse::read(response)?;

        Ok(response)
    }

    pub fn ecm_reception(&mut self, ecm: &[u8]) -> anyhow::Result<EcmReceptionResponse> {
        let cmd = EcmReceptionCommand {
            ecm: ecm.to_vec(),
            acas: self.acas,
        };
        let len = cmd.write(&mut self.tx_buf);
        let response = self.card.transmit(&self.tx_buf[..len], &mut self.rx_buf)?;
        let response = EcmReceptionResponse::read(response)?;

        Ok(response)
    }
}
