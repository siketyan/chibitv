//! APDU Commands and responses implemented on a CAS module, and the high-level API to interact
//! with the module.

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
pub struct InitialSettingConditionCommand;

impl InitialSettingConditionCommand {
    pub(crate) fn write(&self, buf: &mut [u8]) -> usize {
        let cmd = Command::new_with_le(0x90, 0x30, 0x00, 0x01, 0x00);
        cmd.write(buf);
        cmd.len()
    }
}

#[derive(Copy, Clone, Debug, Default, Eq, FromRepr, Hash, PartialEq)]
#[repr(u8)]
pub enum KindOfCasModule {
    #[default]
    General = 0x02,
}

#[derive(Clone, Debug)]
pub struct InitialSettingConditionResponse {
    pub unit_length: u8,
    pub cas_module_instruction: u16,
    pub return_code: u16,
    pub ca_system_id: u16,
    pub cas_module_id: [u8; 6],
    pub kind_of_cas_module: KindOfCasModule,
    pub message_division_length: u8,
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
        let cas_module_instruction = reader.read_u16::<BE>()?;
        let return_code = reader.read_u16::<BE>()?;
        let ca_system_id = reader.read_u16::<BE>()?;
        let cas_module_id = reader.read_byte_array()?;
        let kind_of_cas_module = reader.read_u8()?;
        let message_division_length = reader.read_u8()?;
        let system_management_id_len = reader.read_u8()?;

        let mut system_management_ids = Vec::with_capacity(system_management_id_len as usize);
        for _ in 0..system_management_id_len {
            system_management_ids.push(reader.read_u16::<BE>()?);
        }

        Ok(Self {
            unit_length,
            cas_module_instruction,
            return_code,
            ca_system_id,
            cas_module_id,
            kind_of_cas_module: KindOfCasModule::from_repr(kind_of_cas_module)
                .ok_or(ErrorKind::InvalidData)?,
            message_division_length,
            system_management_ids,
        })
    }
}

#[derive(Clone, Debug)]
pub struct EcmReceptionCommand {
    pub ecm: Vec<u8>,
}

impl EcmReceptionCommand {
    fn write(&self, buf: &mut [u8]) -> usize {
        let cmd = Command::new_with_payload_le(0x90, 0x34, 0x00, 0x01, 0x00, &self.ecm);
        cmd.write(buf);
        cmd.len()
    }
}

#[derive(Clone, Debug)]
pub struct EcmReceptionResponse {
    pub unit_length: u8,
    pub cas_module_instruction: u16,
    pub return_code: u16,
    pub ks: [u8; 32],
    pub broadcaster_identifier: u8,
    pub extension_response_data: Vec<u8>,
}

impl EcmReceptionResponse {
    fn read(buf: &[u8]) -> Result<Self> {
        let response = Response::from(buf);
        assert!(response.is_ok());

        let mut reader = Cursor::new(response.payload);

        let protocol_unit_number = reader.read_u8()?;
        assert_eq!(protocol_unit_number, 0x00);

        let unit_length = reader.read_u8()?;
        let cas_module_instruction = reader.read_u16::<BE>()?;
        let return_code = reader.read_u16::<BE>()?;
        let ks = reader.read_byte_array()?;
        let broadcaster_identifier = reader.read_u8()?;

        let mut extension_response_data = Vec::new();
        let _ = reader.read_to_end(&mut extension_response_data)?;

        Ok(Self {
            unit_length,
            cas_module_instruction,
            return_code,
            ks,
            broadcaster_identifier,
            extension_response_data,
        })
    }
}

#[derive(Clone, Debug)]
pub struct ScramblingKeyProtectionSettingCommand {
    pub setting_data: Vec<u8>,
}

impl ScramblingKeyProtectionSettingCommand {
    fn write(&self, buf: &mut [u8]) -> usize {
        let cmd = Command::new_with_payload_le(0x90, 0xA0, 0x00, 0x01, 0x00, &self.setting_data);
        cmd.write(buf);
        cmd.len()
    }
}

#[derive(Clone, Debug)]
pub struct ScramblingKeyProtectionSettingResponse {
    pub unit_number: u8,
    pub cas_module_direction: u16,
    pub return_code: u16,
    pub setting_response_data: Vec<u8>,
}

impl ScramblingKeyProtectionSettingResponse {
    fn read(buf: &[u8]) -> Result<Self> {
        let response = Response::from(buf);
        assert!(response.is_ok());

        let mut reader = Cursor::new(response.payload);

        let protocol_unit_number = reader.read_u8()?;
        assert_eq!(protocol_unit_number, 0x00);

        let unit_number = reader.read_u8()?;
        let cas_module_direction = reader.read_u16::<BE>()?;
        let return_code = reader.read_u16::<BE>()?;

        let mut setting_response_data = Vec::new();
        let _ = reader.read_to_end(&mut setting_response_data)?;

        Ok(Self {
            unit_number,
            cas_module_direction,
            return_code,
            setting_response_data,
        })
    }
}

/// A CAS module, backed by the PC/SC interface.
pub struct CasModule {
    card: Card,
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
            tx_buf: vec![0u8; 2048],
            rx_buf: vec![0u8; 4096],
        })
    }

    pub fn initial_setting_condition(&mut self) -> anyhow::Result<InitialSettingConditionResponse> {
        let len = InitialSettingConditionCommand.write(&mut self.tx_buf);
        let response = self.card.transmit(&self.tx_buf[..len], &mut self.rx_buf)?;
        let response = InitialSettingConditionResponse::read(response)?;

        Ok(response)
    }

    pub fn ecm_reception(&mut self, ecm: &[u8]) -> anyhow::Result<EcmReceptionResponse> {
        let cmd = EcmReceptionCommand { ecm: ecm.to_vec() };
        let len = cmd.write(&mut self.tx_buf);
        let response = self.card.transmit(&self.tx_buf[..len], &mut self.rx_buf)?;
        let response = EcmReceptionResponse::read(response)?;

        Ok(response)
    }

    pub fn scrambling_key_protection_setting(
        &mut self,
        setting_data: &[u8],
    ) -> anyhow::Result<ScramblingKeyProtectionSettingResponse> {
        let cmd = ScramblingKeyProtectionSettingCommand {
            setting_data: setting_data.to_vec(),
        };

        let len = cmd.write(&mut self.tx_buf);
        let response = self.card.transmit(&self.tx_buf[..len], &mut self.rx_buf)?;
        let response = ScramblingKeyProtectionSettingResponse::read(response)?;

        Ok(response)
    }
}
