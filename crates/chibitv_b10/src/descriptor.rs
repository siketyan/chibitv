use std::io::{Error, ErrorKind, Result};

use bytes::{Buf, Bytes};
use strum::FromRepr;

use crate::read_ext::BytesExt;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CaDescriptor {
    pub ca_system_id: u16,
    pub ca_pid: u16,
    pub private_data: Vec<u8>,
}

impl CaDescriptor {
    pub fn read(bytes: &mut Bytes) -> Result<Self> {
        if bytes.remaining() < 4 {
            return Err(Error::new(
                ErrorKind::UnexpectedEof,
                "CA descriptor must be at least 4 bytes",
            ));
        }

        let ca_system_id = bytes.get_u16();
        let ca_pid = bytes.get_u16() & 0x1FFF;
        let private_data = bytes.to_vec();

        Ok(Self {
            ca_system_id,
            ca_pid,
            private_data,
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NetworkNameDescriptor {
    pub network_name: Vec<u8>,
}

impl NetworkNameDescriptor {
    pub fn read(bytes: &mut Bytes) -> Result<Self> {
        Ok(Self {
            network_name: bytes.to_vec(),
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ServiceListItem {
    pub service_id: u16,
    pub service_type: u8,
}

impl ServiceListItem {
    pub fn read(bytes: &mut Bytes) -> Result<Self> {
        let service_id = bytes.get_u16();
        let service_type = bytes.get_u8();

        Ok(Self {
            service_id,
            service_type,
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ServiceListDescriptor {
    pub services: Vec<ServiceListItem>,
}

impl ServiceListDescriptor {
    pub fn read(bytes: &mut Bytes) -> Result<Self> {
        let mut services = Vec::new();
        while bytes.has_remaining() {
            services.push(ServiceListItem::read(bytes)?);
        }

        Ok(Self { services })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ServiceDescriptor {
    pub service_type: u8,
    pub service_provider_name: Vec<u8>,
    pub service_name: Vec<u8>,
}

impl ServiceDescriptor {
    pub fn read(bytes: &mut Bytes) -> Result<Self> {
        let service_type = bytes.get_u8();

        let service_provider_name_length = bytes.get_u8();
        let service_provider_name = bytes.split_to(service_provider_name_length as usize).into();

        let service_name_length = bytes.get_u8();
        let service_name = bytes.split_to(service_name_length as usize).into();

        Ok(Self {
            service_type,
            service_provider_name,
            service_name,
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ShortEventDescriptor {
    pub iso_639_language_code: [u8; 3],
    pub event_name: Vec<u8>,
    pub text: Vec<u8>,
}

impl ShortEventDescriptor {
    pub fn read(bytes: &mut Bytes) -> Result<Self> {
        let iso_639_language_code = bytes.get_byte_array::<3>();

        let event_name_length = bytes.get_u8();
        let event_name = bytes.split_to(event_name_length as usize).into();

        let text_length = bytes.get_u8();
        let text = bytes.split_to(text_length as usize).into();

        Ok(Self {
            iso_639_language_code,
            event_name,
            text,
        })
    }
}

#[derive(Clone, Debug, FromRepr)]
#[repr(u8)]
pub enum DescriptorTag {
    CaDescriptor = 0x09,
    NetworkNameDescriptor = 0x40,
    ServiceListDescriptor = 0x41,
    ServiceDescriptor = 0x48,
    ShortEventDescriptor = 0x4D,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Descriptor {
    Ca(CaDescriptor),
    NetworkName(NetworkNameDescriptor),
    ServiceList(ServiceListDescriptor),
    Service(ServiceDescriptor),
    ShortEvent(ShortEventDescriptor),
    Unknown(u8, Vec<u8>),
}

impl Descriptor {
    pub fn read(bytes: &mut Bytes) -> Result<Self> {
        let descriptor_tag = bytes.get_u8();
        let descriptor_length = bytes.get_u8();
        let mut bytes = bytes.split_to(descriptor_length as usize);

        let Some(descriptor_tag) = DescriptorTag::from_repr(descriptor_tag) else {
            return Ok(Self::Unknown(descriptor_tag, bytes.into()));
        };

        Ok(match descriptor_tag {
            DescriptorTag::CaDescriptor => Self::Ca(CaDescriptor::read(&mut bytes)?),
            DescriptorTag::NetworkNameDescriptor => {
                Self::NetworkName(NetworkNameDescriptor::read(&mut bytes)?)
            }
            DescriptorTag::ServiceListDescriptor => {
                Self::ServiceList(ServiceListDescriptor::read(&mut bytes)?)
            }
            DescriptorTag::ServiceDescriptor => Self::Service(ServiceDescriptor::read(&mut bytes)?),
            DescriptorTag::ShortEventDescriptor => {
                Self::ShortEvent(ShortEventDescriptor::read(&mut bytes)?)
            }
        })
    }
}

impl TryFrom<&mpeg2ts::ts::Descriptor> for Descriptor {
    type Error = Error;

    fn try_from(descriptor: &mpeg2ts::ts::Descriptor) -> Result<Self> {
        let descriptor_length = u8::try_from(descriptor.data.len()).map_err(|_| {
            Error::new(
                ErrorKind::InvalidInput,
                "descriptor payload must be at most 255 bytes",
            )
        })?;

        let mut bytes = Vec::with_capacity(2 + descriptor.data.len());
        bytes.push(descriptor.tag);
        bytes.push(descriptor_length);
        bytes.extend_from_slice(&descriptor.data);

        Self::read(&mut Bytes::from(bytes))
    }
}

impl TryFrom<mpeg2ts::ts::Descriptor> for Descriptor {
    type Error = Error;

    fn try_from(descriptor: mpeg2ts::ts::Descriptor) -> Result<Self> {
        Self::try_from(&descriptor)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_ca_descriptor() {
        let descriptor = Descriptor::read(&mut Bytes::from_static(&[
            0x09, 0x06, // descriptor_tag, descriptor_length
            0x12, 0x34, // CA_system_ID
            0xE1, 0x23, // reserved + CA_PID
            0x45, 0x67, // private data
        ]))
        .unwrap();

        assert_eq!(
            descriptor,
            Descriptor::Ca(CaDescriptor {
                ca_system_id: 0x1234,
                ca_pid: 0x0123,
                private_data: vec![0x45, 0x67],
            })
        );
    }

    #[test]
    fn read_ca_descriptor_in_table_descriptor_loop() {
        let descriptor = Descriptor::try_from(mpeg2ts::ts::Descriptor {
            tag: 0x09,
            data: vec![
                0x12, 0x34, // CA_system_ID
                0xFF, 0xFF, // reserved + CA_PID
            ],
        })
        .unwrap();

        assert_eq!(
            descriptor,
            Descriptor::Ca(CaDescriptor {
                ca_system_id: 0x1234,
                ca_pid: 0x1FFF,
                private_data: vec![],
            })
        );
    }

    #[test]
    fn reject_short_ca_descriptor() {
        let error = Descriptor::read(&mut Bytes::from_static(&[
            0x09, 0x03, // descriptor_tag, descriptor_length
            0x12, 0x34, 0xE1,
        ]))
        .unwrap_err();

        assert_eq!(error.kind(), ErrorKind::UnexpectedEof);
    }
}
