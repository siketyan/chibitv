use std::io::{Cursor, ErrorKind, Read, Result};

use byteorder::{BE, ReadBytesExt};
use bytes::{Buf, Bytes};
use strum::FromRepr;

#[derive(Copy, Clone, Debug, Eq, FromRepr, PartialEq)]
#[repr(u8)]
#[allow(clippy::enum_variant_names)]
pub enum FecType {
    NonProtected = 0,
    SourcePacketProtected = 1,
    RepairPacketProtected = 2,
}

#[derive(Clone, Debug)]
pub struct MmtpExtensionHeader {
    pub header_type: u16,
    pub data: Bytes,
}

#[derive(Clone, Debug)]
pub struct MmtpPacket {
    pub fec_type: FecType,
    pub rap_flag: bool,
    pub payload_type: u8,
    pub packet_id: u16,
    pub delivery_timestamp: u32,
    pub packet_sequence_number: u32,
    pub packet_counter: Option<u32>,
    pub extension_header: Option<MmtpExtensionHeader>,
    pub payload: Bytes,
}

impl MmtpPacket {
    pub fn read(bytes: &mut Bytes) -> Result<Self> {
        let head = bytes.get_u8();
        let version = (head & 0b1100_0000) >> 6;
        let packet_counter_flag = ((head & 0b0010_0000) >> 5) == 1;
        let fec_type = FecType::from_repr((head & 0b0001_1000) >> 3).unwrap();
        let extension_header_flag = ((head & 0b0000_0010) >> 1) == 1;
        let rap_flag = (head & 0b0000_0001) == 1;
        assert_eq!(version, 0b00);

        let head = bytes.get_u8();
        let payload_type = head & 0b0011_1111;
        let packet_id = bytes.get_u16();
        let delivery_timestamp = bytes.get_u32();
        let packet_sequence_number = bytes.get_u32();
        let packet_counter = if packet_counter_flag {
            Some(bytes.get_u32())
        } else {
            None
        };
        let extension_header = if extension_header_flag {
            let header_type = bytes.get_u16();
            let data_length = bytes.get_u16();
            let data = bytes.split_to(data_length as usize);
            Some(MmtpExtensionHeader { header_type, data })
        } else {
            None
        };

        Ok(Self {
            fec_type,
            rap_flag,
            payload_type,
            packet_id,
            delivery_timestamp,
            packet_sequence_number,
            packet_counter,
            extension_header,
            payload: bytes.clone(),
        })
    }
}

#[derive(Copy, Clone, Debug, Eq, FromRepr, PartialEq)]
#[repr(u8)]
pub enum FragmentationIndicator {
    NotFragmented = 0b00,
    FragmentHead = 0b01,
    FragmentBody = 0b10,
    FragmentTail = 0b11,
}

#[derive(Copy, Clone, Debug, Eq, FromRepr, PartialEq)]
#[repr(u8)]
pub enum MpuFragmentType {
    MpuMetadata = 0,
    MovieFragmentMetadata = 1,
    Mfu = 2,
}
#[derive(Clone, Debug)]
pub struct MpuFragment {
    pub fragment_type: MpuFragmentType,
    pub timed_flag: bool,
    pub fragmentation_indicator: FragmentationIndicator,
    pub aggregation_flag: bool,
    pub fragment_counter: u8,
    pub mpu_sequence_number: u32,
    pub payload: Vec<u8>,
}

impl MpuFragment {
    pub fn read(mut reader: impl Read) -> Result<Self> {
        let payload_length = reader.read_u16::<BE>()?;
        let head = reader.read_u8()?;
        let fragment_type = MpuFragmentType::from_repr((head & 0b1111_0000) >> 4).unwrap();
        let timed_flag = ((head & 0b0000_1000) >> 3) == 1;
        let fragmentation_indicator = FragmentationIndicator::from_repr((head & 0b0000_0110) >> 1)
            .ok_or(ErrorKind::InvalidData)?;
        let aggregation_flag = (head & 0b0000_0001) == 1;
        let fragment_counter = reader.read_u8()?;
        let mpu_sequence_number = reader.read_u32::<BE>()?;

        let mut payload = vec![0u8; (payload_length - 6) as usize];
        reader.read_exact(&mut payload)?;

        Ok(Self {
            fragment_type,
            timed_flag,
            fragmentation_indicator,
            aggregation_flag,
            fragment_counter,
            mpu_sequence_number,
            payload,
        })
    }
}

#[derive(Clone, Debug)]
pub enum SignalingMessagePayload {
    Aggregated(Vec<Vec<u8>>),
    Default(Vec<u8>),
}

#[derive(Clone, Debug)]
pub struct SignalingMessage {
    pub fragmentation_indicator: FragmentationIndicator,
    pub fragment_counter: u8,
    pub payload: SignalingMessagePayload,
}

impl SignalingMessage {
    pub fn read(buf: &[u8]) -> Result<Self> {
        let mut reader = Cursor::new(buf);

        let head = reader.read_u8()?;
        let fragmentation_indicator = FragmentationIndicator::from_repr((head & 0b1100_0000) >> 6)
            .ok_or(ErrorKind::InvalidData)?;
        let length_extension_flag = ((head & 0b0000_0010) >> 1) == 1;
        let aggregation_flag = (head & 0b0000_0001) == 1;
        let fragment_counter = reader.read_u8()?;

        let payload = if aggregation_flag {
            let mut payloads = Vec::new();

            while (reader.position() as usize) < buf.len() {
                let message_length = if length_extension_flag {
                    reader.read_u32::<BE>()? as usize
                } else {
                    reader.read_u16::<BE>()? as usize
                };

                let remaining_len = buf.len() - (reader.position() as usize);
                assert!(
                    message_length <= remaining_len,
                    "insufficient buffer size: {message_length} > {remaining_len}"
                );

                let mut payload = vec![0u8; message_length];
                reader.read_exact(&mut payload)?;
                payloads.push(payload);
            }

            SignalingMessagePayload::Aggregated(payloads)
        } else {
            let mut buf = Vec::new();
            reader.read_to_end(&mut buf)?;
            SignalingMessagePayload::Default(buf)
        };

        Ok(Self {
            fragmentation_indicator,
            fragment_counter,
            payload,
        })
    }
}

pub enum MmtpPayload {
    MpuFragment(MpuFragment),
    SignalingMessage(SignalingMessage),
}

#[derive(Copy, Clone, Debug, Eq, FromRepr, PartialEq)]
#[repr(u8)]
enum MmtpPayloadType {
    Mpu = 0x00,
    GenericObject = 0x01,
    ControlMessage = 0x02,
    FecRepairSymbol = 0x03,
}

impl TryFrom<&MmtpPacket> for MmtpPayload {
    type Error = std::io::Error;

    fn try_from(value: &MmtpPacket) -> Result<Self> {
        let payload_type =
            MmtpPayloadType::from_repr(value.payload_type).ok_or(ErrorKind::InvalidData)?;

        Ok(match payload_type {
            MmtpPayloadType::Mpu => {
                Self::MpuFragment(MpuFragment::read(Cursor::new(&value.payload))?)
            }
            MmtpPayloadType::ControlMessage => {
                Self::SignalingMessage(SignalingMessage::read(&value.payload)?)
            }
            _ => todo!(),
        })
    }
}
