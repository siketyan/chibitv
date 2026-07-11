//! APDU Commands and responses implemented on a CAS module, and the high-level API to interact
//! with the module.

use std::fmt::{Debug, Formatter};
use std::io::{Cursor, ErrorKind, Read, Result};
use std::sync::Arc;

use apdu_core::{Command, Response};
use byteorder::{BE, ReadBytesExt};
use strum::FromRepr;

use crate::CasModule;

trait ReadExt: Read {
    fn read_byte_array<const N: usize>(&mut self) -> Result<[u8; N]> {
        let mut array = [0u8; N];
        self.read_exact(&mut array)?;
        Ok(array)
    }
}

impl<T: Read> ReadExt for T {}

#[derive(Clone, Debug)]
pub(crate) struct InitialSettingConditionCommand;

impl InitialSettingConditionCommand {
    pub(crate) fn write(&self, buf: &mut [u8]) -> usize {
        let cmd = Command::new_with_le(0x90, 0x30, 0x00, 0x01, 0x00);
        cmd.write(buf);
        cmd.len()
    }
}

#[derive(Copy, Clone, Debug, Default, Eq, FromRepr, Hash, PartialEq)]
#[repr(u8)]
pub(crate) enum KindOfCasModule {
    #[default]
    General = 0x02,
}

#[derive(Clone, Debug)]
#[allow(dead_code)]
pub(crate) struct InitialSettingConditionResponse {
    pub(crate) unit_length: u8,
    pub(crate) cas_module_instruction: u16,
    pub(crate) return_code: u16,
    pub(crate) ca_system_id: u16,
    pub(crate) cas_module_id: [u8; 6],
    pub(crate) kind_of_cas_module: KindOfCasModule,
    pub(crate) message_division_length: u8,
    pub(crate) system_management_ids: Vec<u16>,
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
pub(crate) struct EcmReceptionCommand {
    pub(crate) ecm: Vec<u8>,
}

impl EcmReceptionCommand {
    fn write(&self, buf: &mut [u8]) -> usize {
        let cmd = Command::new_with_payload_le(0x90, 0x34, 0x00, 0x01, 0x00, &self.ecm);
        cmd.write(buf);
        cmd.len()
    }
}

#[derive(Clone, Debug)]
#[allow(dead_code)]
pub(crate) struct EcmReceptionResponse {
    pub(crate) unit_length: u8,
    pub(crate) cas_module_instruction: u16,
    pub(crate) return_code: u16,
    pub(crate) ks: [u8; 32],
    pub(crate) broadcaster_identifier: u8,
    pub(crate) extension_response_data: Vec<u8>,
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
pub(crate) struct ScramblingKeyProtectionSettingCommand {
    pub(crate) setting_data: Vec<u8>,
}

impl ScramblingKeyProtectionSettingCommand {
    fn write(&self, buf: &mut [u8]) -> usize {
        let cmd = Command::new_with_payload_le(0x90, 0xA0, 0x00, 0x01, 0x00, &self.setting_data);
        cmd.write(buf);
        cmd.len()
    }
}

#[derive(Clone, Debug)]
#[allow(dead_code)]
pub(crate) struct ScramblingKeyProtectionSettingResponse {
    pub(crate) unit_number: u8,
    pub(crate) cas_module_direction: u16,
    pub(crate) return_code: u16,
    pub(crate) setting_response_data: Vec<u8>,
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

/// ARIB STD-B61 commands executed on a physical CAS module.
pub(crate) struct CasClient {
    module: Arc<dyn CasModule>,
    tx_buf: Vec<u8>,
    rx_buf: Vec<u8>,
}

impl Debug for CasClient {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CasModule").finish()
    }
}

impl CasClient {
    pub fn new(module: Arc<dyn CasModule>) -> Self {
        Self {
            module,
            tx_buf: vec![0u8; 2048],
            rx_buf: vec![0u8; 4096],
        }
    }

    pub fn initial_setting_condition(&mut self) -> anyhow::Result<InitialSettingConditionResponse> {
        let len = InitialSettingConditionCommand.write(&mut self.tx_buf);
        let response_len = self
            .module
            .transmit(&self.tx_buf[..len], &mut self.rx_buf)?;
        let response = InitialSettingConditionResponse::read(&self.rx_buf[..response_len])?;

        Ok(response)
    }

    pub fn scrambling_key_protection_setting_and_ecm_reception(
        &mut self,
        setting_data: &[u8],
        ecm: &[u8],
    ) -> anyhow::Result<(ScramblingKeyProtectionSettingResponse, EcmReceptionResponse)> {
        let setting_command = ScramblingKeyProtectionSettingCommand {
            setting_data: setting_data.to_vec(),
        };
        let setting_len = setting_command.write(&mut self.tx_buf);
        let mut module = self.module.lock()?;
        let setting_response_len =
            module.transmit(&self.tx_buf[..setting_len], &mut self.rx_buf)?;
        let setting_response =
            ScramblingKeyProtectionSettingResponse::read(&self.rx_buf[..setting_response_len])?;

        let ecm_command = EcmReceptionCommand { ecm: ecm.to_vec() };
        let ecm_len = ecm_command.write(&mut self.tx_buf);
        let ecm_response_len = module.transmit(&self.tx_buf[..ecm_len], &mut self.rx_buf)?;
        let ecm_response = EcmReceptionResponse::read(&self.rx_buf[..ecm_response_len])?;

        Ok((setting_response, ecm_response))
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;
    use crate::CasModuleGuard;

    #[derive(Default)]
    struct FakeCasModule {
        lock_count: AtomicUsize,
        guarded_transmit_count: AtomicUsize,
    }

    struct FakeCasModuleGuard<'a> {
        module: &'a FakeCasModule,
    }

    impl CasModule for FakeCasModule {
        fn transmit(&self, _command: &[u8], _response: &mut [u8]) -> anyhow::Result<usize> {
            anyhow::bail!("unexpected single command")
        }

        fn lock(&self) -> anyhow::Result<Box<dyn CasModuleGuard + '_>> {
            self.lock_count.fetch_add(1, Ordering::Relaxed);
            Ok(Box::new(FakeCasModuleGuard { module: self }))
        }
    }

    impl CasModuleGuard for FakeCasModuleGuard<'_> {
        fn transmit(&mut self, command: &[u8], response: &mut [u8]) -> anyhow::Result<usize> {
            self.module
                .guarded_transmit_count
                .fetch_add(1, Ordering::Relaxed);

            let card_response = match command[1] {
                0xA0 => [&[0x00; 6][..], &[0x01, 0x02], &[0x90, 0x00]].concat(),
                0x34 => [&[0x00; 6][..], &[0x03; 32], &[0x00, 0x90, 0x00]].concat(),
                instruction => anyhow::bail!("unexpected instruction: {instruction:#04x}"),
            };
            response[..card_response.len()].copy_from_slice(&card_response);
            Ok(card_response.len())
        }
    }

    #[test]
    fn holds_one_lock_across_the_setting_and_ecm_commands() {
        let module = Arc::new(FakeCasModule::default());
        let mut client = CasClient::new(module.clone());

        let (setting, ecm) = client
            .scrambling_key_protection_setting_and_ecm_reception(&[0x01], &[0x02])
            .unwrap();

        assert_eq!(module.lock_count.load(Ordering::Relaxed), 1);
        assert_eq!(module.guarded_transmit_count.load(Ordering::Relaxed), 2);
        assert_eq!(setting.setting_response_data, [0x01, 0x02]);
        assert_eq!(ecm.ks, [0x03; 32]);
    }
}
