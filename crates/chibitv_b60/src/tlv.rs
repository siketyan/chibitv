use std::io::{Read, Result};

use byteorder::{BE, ReadBytesExt};
use bytes::Bytes;
use strum::FromRepr;

#[derive(Copy, Clone, Debug, Eq, FromRepr, PartialEq)]
#[repr(u8)]
pub enum TlvPacketType {
    IPv4 = 0x01,
    IPv6 = 0x02,
    CompressedIP = 0x03,
    TransmissionControlSignal = 0xFE,
    Null = 0xFF,
}

#[derive(Clone, Debug)]
pub struct TlvPacket {
    pub packet_type: TlvPacketType,
    pub data: Bytes,
}

impl TlvPacket {
    pub fn try_read(mut reader: impl Read) -> Result<Option<Self>> {
        let head = reader.read_u8()?;
        assert_eq!(head, 0x7F);

        let packet_type = reader.read_u8()?;
        let Some(packet_type) = TlvPacketType::from_repr(packet_type) else {
            return Ok(None);
        };

        let data_length = reader.read_u16::<BE>()?;
        let mut data = vec![0u8; data_length as usize];
        reader.read_exact(&mut data)?;

        Ok(Some(Self {
            packet_type,
            data: Bytes::from(data),
        }))
    }
}
