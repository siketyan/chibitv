//! APDU commands and responses implemented on an ARIB STD-B25 CAS module.

use std::fmt::{Debug, Formatter};
use std::io::{Cursor, ErrorKind, Read, Result};
use std::sync::{Arc, Mutex, mpsc};

use anyhow::anyhow;
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
pub struct InitialSettingConditionCommand {
    pub acas: bool,
}

impl InitialSettingConditionCommand {
    pub(crate) fn to_bytes(&self) -> Vec<u8> {
        let p2 = if self.acas { 0x02 } else { 0x00 };
        Command::new_with_le(0x90, 0x30, 0x00, p2, 0x00).into()
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
    fn to_bytes(&self) -> Vec<u8> {
        let p2 = if self.acas { 0x02 } else { 0x00 };
        Command::new_with_payload_le(0x90, 0x34, 0x00, p2, 0x00, &self.ecm).into()
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
    pub(crate) fn read(buf: &[u8]) -> Result<Self> {
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

/// ARIB STD-B25 commands executed on a physical CAS module.
pub(crate) struct CasClient {
    worker: CasWorker,
    acas: bool,
}

impl Debug for CasClient {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CasModule").finish()
    }
}

impl CasClient {
    pub fn new(module: Arc<Mutex<dyn CasModule>>, acas: bool) -> Self {
        Self {
            worker: CasWorker::spawn(module),
            acas,
        }
    }

    pub fn initial_setting_condition(&mut self) -> anyhow::Result<InitialSettingConditionResponse> {
        let command = InitialSettingConditionCommand { acas: self.acas }.to_bytes();
        self.worker.send(vec![command]);
        let responses = self.receive(true).expect("waited for the responses")?;

        Ok(InitialSettingConditionResponse::read(&responses[0])?)
    }

    /// Sends the ECM to the module, whose answer is taken with [`CasClient::receive`] and read
    /// with [`EcmReceptionResponse::read`].
    pub fn ecm_reception(&mut self, ecm: &[u8]) {
        let command = EcmReceptionCommand {
            ecm: ecm.to_vec(),
            acas: self.acas,
        };
        self.worker.send(vec![command.to_bytes()]);
    }

    /// Takes the responses to the commands sent last if they have arrived, waiting for them if
    /// `wait` is set, or `None` if they are still on their way or nothing was sent.
    pub fn receive(&mut self, wait: bool) -> Option<anyhow::Result<Vec<Vec<u8>>>> {
        self.worker.receive(wait)
    }
}

/// Talks to the CAS module on a thread of its own, so that whoever sends it commands goes on
/// while it answers.
struct CasWorker {
    jobs: mpsc::Sender<Vec<Vec<u8>>>,
    responses: mpsc::Receiver<anyhow::Result<Vec<Vec<u8>>>>,
    /// How many runs of commands have been sent and not answered yet.
    in_flight: usize,
}

impl CasWorker {
    fn spawn(module: Arc<Mutex<dyn CasModule>>) -> Self {
        let (jobs, jobs_rx) = mpsc::channel::<Vec<Vec<u8>>>();
        let (responses_tx, responses) = mpsc::channel();

        // The thread ends once the worker is dropped along with its sender.
        std::thread::spawn(move || {
            for commands in jobs_rx {
                if responses_tx.send(transmit_all(&module, &commands)).is_err() {
                    break;
                }
            }
        });

        Self {
            jobs,
            responses,
            in_flight: 0,
        }
    }

    fn send(&mut self, commands: Vec<Vec<u8>>) {
        // Should the thread have gone, receiving tells so.
        let _ = self.jobs.send(commands);
        self.in_flight += 1;
    }

    /// Takes the responses to the run sent last, passing over those to the runs before it.
    fn receive(&mut self, wait: bool) -> Option<anyhow::Result<Vec<Vec<u8>>>> {
        while self.in_flight > 0 {
            let result = match wait {
                true => self
                    .responses
                    .recv()
                    .map_err(|_| mpsc::TryRecvError::Disconnected),
                false => self.responses.try_recv(),
            };

            match result {
                Ok(responses) => {
                    self.in_flight -= 1;
                    if self.in_flight == 0 {
                        return Some(responses);
                    }
                }
                Err(mpsc::TryRecvError::Empty) => return None,
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.in_flight = 0;
                    return Some(Err(anyhow!("CAS module is not running")));
                }
            }
        }

        None
    }
}

/// Sends the commands to the module one after another, with nothing else sent to it in between.
fn transmit_all(
    module: &Mutex<dyn CasModule>,
    commands: &[Vec<u8>],
) -> anyhow::Result<Vec<Vec<u8>>> {
    let mut module = module.lock().map_err(|_| anyhow!("CAS module panicked"))?;

    commands
        .iter()
        .map(|command| module.transmit(command))
        .collect()
}
