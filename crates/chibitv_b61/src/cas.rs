//! APDU Commands and responses implemented on a CAS module, and the high-level API to interact
//! with the module.

use std::fmt::{Debug, Formatter};
use std::io::{Cursor, ErrorKind, Read, Result};
use std::sync::{Arc, mpsc};

use anyhow::anyhow;
use apdu_core::{Command, Response};
use byteorder::{BE, ReadBytesExt};
use strum::FromRepr;

use crate::{CasModule, PendingResponses};

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
    pub(crate) fn to_bytes(&self) -> Vec<u8> {
        Command::new_with_le(0x90, 0x30, 0x00, 0x01, 0x00).into()
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
    fn to_bytes(&self) -> Vec<u8> {
        Command::new_with_payload_le(0x90, 0x34, 0x00, 0x01, 0x00, &self.ecm).into()
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
    fn to_bytes(&self) -> Vec<u8> {
        Command::new_with_payload_le(0x90, 0xA0, 0x00, 0x01, 0x00, &self.setting_data).into()
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
}

impl Debug for CasClient {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CasModule").finish()
    }
}

impl CasClient {
    pub fn new(module: Arc<dyn CasModule>) -> Self {
        Self { module }
    }

    pub fn initial_setting_condition(&self) -> anyhow::Result<InitialSettingConditionResponse> {
        let pending = self
            .module
            .transmit(vec![InitialSettingConditionCommand.to_bytes()]);
        let responses = receive(&pending, true).expect("waited for the responses")?;

        Ok(InitialSettingConditionResponse::read(&responses[0])?)
    }

    /// Sends the two commands to the module back to back, so that nothing comes between the
    /// setting and the ECM it protects the key of; their answers are read with
    /// [`read_scrambling_key_protection_setting_and_ecm_reception`].
    pub fn scrambling_key_protection_setting_and_ecm_reception(
        &self,
        setting_data: &[u8],
        ecm: &[u8],
    ) -> PendingResponses {
        let setting_command = ScramblingKeyProtectionSettingCommand {
            setting_data: setting_data.to_vec(),
        };
        let ecm_command = EcmReceptionCommand { ecm: ecm.to_vec() };

        self.module
            .transmit(vec![setting_command.to_bytes(), ecm_command.to_bytes()])
    }
}

pub(crate) fn read_scrambling_key_protection_setting_and_ecm_reception(
    responses: &[Vec<u8>],
) -> anyhow::Result<(ScramblingKeyProtectionSettingResponse, EcmReceptionResponse)> {
    Ok((
        ScramblingKeyProtectionSettingResponse::read(&responses[0])?,
        EcmReceptionResponse::read(&responses[1])?,
    ))
}

/// Takes the responses if they have arrived, waiting for them if `wait` is set, or `None` if they
/// are still on their way.
pub(crate) fn receive(
    pending: &PendingResponses,
    wait: bool,
) -> Option<anyhow::Result<Vec<Vec<u8>>>> {
    let result = match wait {
        true => pending.recv().map_err(|_| mpsc::TryRecvError::Disconnected),
        false => pending.try_recv(),
    };

    match result {
        Ok(responses) => Some(responses),
        Err(mpsc::TryRecvError::Empty) => None,
        Err(mpsc::TryRecvError::Disconnected) => Some(Err(anyhow!("CAS module is not running"))),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;

    #[derive(Default)]
    struct FakeCasModule {
        transmitted: Mutex<Vec<Vec<Vec<u8>>>>,
    }

    impl CasModule for FakeCasModule {
        fn transmit(&self, commands: Vec<Vec<u8>>) -> PendingResponses {
            let responses = commands
                .iter()
                .map(|command| match command[1] {
                    0xA0 => [&[0x00; 6][..], &[0x01, 0x02], &[0x90, 0x00]].concat(),
                    0x34 => [&[0x00; 6][..], &[0x03; 32], &[0x00, 0x90, 0x00]].concat(),
                    instruction => panic!("unexpected instruction: {instruction:#04x}"),
                })
                .collect();
            self.transmitted.lock().unwrap().push(commands);

            let (tx, rx) = mpsc::sync_channel(1);
            tx.send(Ok(responses)).unwrap();
            rx
        }
    }

    #[test]
    fn sends_the_setting_and_ecm_commands_as_one_run() {
        let module = Arc::new(FakeCasModule::default());
        let client = CasClient::new(module.clone());

        let pending = client.scrambling_key_protection_setting_and_ecm_reception(&[0x01], &[0x02]);
        let responses = receive(&pending, true).unwrap().unwrap();
        let (setting, ecm) =
            read_scrambling_key_protection_setting_and_ecm_reception(&responses).unwrap();

        let transmitted = module.transmitted.lock().unwrap();
        assert_eq!(transmitted.len(), 1);
        assert_eq!(transmitted[0].len(), 2);
        assert_eq!(setting.setting_response_data, [0x01, 0x02]);
        assert_eq!(ecm.ks, [0x03; 32]);
    }
}
