use std::io::{Read, Result};

use byteorder::{BE, ReadBytesExt};
use bytes::{Buf, Bytes};
use strum::FromRepr;

use crate::table::Table;

#[derive(Clone, Debug)]
pub struct PaMessage {
    pub version: u8,
    pub tables: Vec<Table>,
}

impl PaMessage {
    pub fn read(mut reader: impl Read) -> Result<Self> {
        let version = reader.read_u8().unwrap();
        let length = reader.read_u32::<BE>().unwrap();

        let mut buf = vec![0; length as usize];
        reader.read_exact(&mut buf)?;

        let mut bytes = Bytes::from(buf);

        #[allow(unused)]
        struct TableMeta {
            table_id: u8,
            table_version: u8,
            table_length: u16,
        }

        let number_of_tables = bytes.get_u8() as usize;
        let mut table_meta = Vec::with_capacity(number_of_tables);
        for _ in 0..number_of_tables {
            let table_id = bytes.get_u8();
            let table_version = bytes.get_u8();
            let table_length = bytes.get_u16_ne();

            table_meta.push(TableMeta {
                table_id,
                table_version,
                table_length,
            });
        }

        let mut tables = Vec::with_capacity(number_of_tables);
        while bytes.has_remaining() {
            tables.push(Table::read(&mut bytes)?);
        }

        Ok(Self { version, tables })
    }
}

#[derive(Clone, Debug)]
pub struct M2SectionMessage {
    pub version: u8,
    pub table: Table,
}

impl M2SectionMessage {
    pub fn read(mut reader: impl Read) -> Result<Self> {
        let version = reader.read_u8()?;
        let length = reader.read_u16::<BE>()?;

        let mut buf = vec![0; length as usize];
        reader.read_exact(&mut buf)?;

        let mut bytes = Bytes::from(buf);
        let table = Table::read(&mut bytes)?;

        Ok(Self { version, table })
    }
}

#[derive(Copy, Clone, Debug, Eq, FromRepr, PartialEq)]
#[repr(u16)]
pub enum MessageId {
    Pa = 0x0000,
    M2Section = 0x8000,
}

#[derive(Clone, Debug)]
pub enum Message {
    Pa(PaMessage),
    M2Section(M2SectionMessage),
    Unknown(u16, Vec<u8>),
}

impl Message {
    pub fn read(mut reader: impl Read) -> Result<Self> {
        let message_id = reader.read_u16::<BE>()?;
        let Some(message_id) = MessageId::from_repr(message_id) else {
            let mut buf = Vec::new();
            reader.read_to_end(&mut buf)?;
            return Ok(Self::Unknown(message_id, buf));
        };

        Ok(match message_id {
            MessageId::Pa => Self::Pa(PaMessage::read(&mut reader)?),
            MessageId::M2Section => Self::M2Section(M2SectionMessage::read(&mut reader)?),
        })
    }
}
