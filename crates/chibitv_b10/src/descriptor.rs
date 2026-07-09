use std::io::Result;

use bytes::{Buf, Bytes};
use strum::FromRepr;

use crate::read_ext::BytesExt;

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
    NetworkNameDescriptor = 0x40,
    ServiceListDescriptor = 0x41,
    ServiceDescriptor = 0x48,
    ShortEventDescriptor = 0x4D,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Descriptor {
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
