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

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExtendedEventItem {
    pub item_description: Vec<u8>,
    pub item: Vec<u8>,
}

impl ExtendedEventItem {
    pub fn read(bytes: &mut Bytes) -> Result<Self> {
        let item_description_length = bytes.get_u8();
        let item_description = split_to(bytes, item_description_length as usize)?.into();

        let item_length = bytes.get_u8();
        let item = split_to(bytes, item_length as usize)?.into();

        Ok(Self {
            item_description,
            item,
        })
    }
}

/// Carries the detailed description of an event, in items keyed by their
/// description (e.g. the cast of a programme).
///
/// One event may be described by up to 16 of these descriptors: an item longer
/// than a single descriptor can hold continues in the next one, as an item
/// whose `item_description` is empty.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExtendedEventDescriptor {
    pub descriptor_number: u8,
    pub last_descriptor_number: u8,
    pub iso_639_language_code: [u8; 3],
    pub items: Vec<ExtendedEventItem>,
    pub text: Vec<u8>,
}

impl ExtendedEventDescriptor {
    pub fn read(bytes: &mut Bytes) -> Result<Self> {
        if bytes.remaining() < 5 {
            return Err(Error::new(
                ErrorKind::UnexpectedEof,
                "extended event descriptor must be at least 5 bytes",
            ));
        }

        let head = bytes.get_u8();
        let descriptor_number = (head & 0xF0) >> 4;
        let last_descriptor_number = head & 0x0F;

        let iso_639_language_code = bytes.get_byte_array::<3>();

        let items = {
            let length_of_items = bytes.get_u8();
            let mut bytes = split_to(bytes, length_of_items as usize)?;
            let mut items = Vec::new();
            while bytes.has_remaining() {
                items.push(ExtendedEventItem::read(&mut bytes)?);
            }

            items
        };

        let text_length = bytes.get_u8();
        let text = split_to(bytes, text_length as usize)?.into();

        Ok(Self {
            descriptor_number,
            last_descriptor_number,
            iso_639_language_code,
            items,
            text,
        })
    }
}

fn split_to(bytes: &mut Bytes, length: usize) -> Result<Bytes> {
    if bytes.remaining() < length {
        return Err(Error::new(
            ErrorKind::UnexpectedEof,
            "descriptor field runs past the end of the descriptor",
        ));
    }

    Ok(bytes.split_to(length))
}

#[derive(Clone, Debug, FromRepr)]
#[repr(u8)]
pub enum DescriptorTag {
    CaDescriptor = 0x09,
    NetworkNameDescriptor = 0x40,
    ServiceListDescriptor = 0x41,
    ServiceDescriptor = 0x48,
    ShortEventDescriptor = 0x4D,
    ExtendedEventDescriptor = 0x4E,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Descriptor {
    Ca(CaDescriptor),
    NetworkName(NetworkNameDescriptor),
    ServiceList(ServiceListDescriptor),
    Service(ServiceDescriptor),
    ShortEvent(ShortEventDescriptor),
    ExtendedEvent(ExtendedEventDescriptor),
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
            DescriptorTag::ExtendedEventDescriptor => {
                Self::ExtendedEvent(ExtendedEventDescriptor::read(&mut bytes)?)
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
    fn read_extended_event_descriptor() {
        let descriptor = Descriptor::read(&mut Bytes::from_static(&[
            0x4E, 0x19, // descriptor_tag, descriptor_length
            0x01, // descriptor_number, last_descriptor_number
            b'j', b'p', b'n', // ISO_639_language_code
            0x0E, // length_of_items
            0x04, b'C', b'a', b's', b't', // item_description_length, item_description
            0x03, b'B', b'o', b'b', // item_length, item
            0x00, // item_description_length (a continued item)
            0x03, b'a', b'n', b'd', // item_length, item
            0x05, b'H', b'e', b'l', b'l', b'o', // text_length, text
        ]))
        .unwrap();

        assert_eq!(
            descriptor,
            Descriptor::ExtendedEvent(ExtendedEventDescriptor {
                descriptor_number: 0,
                last_descriptor_number: 1,
                iso_639_language_code: *b"jpn",
                items: vec![
                    ExtendedEventItem {
                        item_description: b"Cast".to_vec(),
                        item: b"Bob".to_vec(),
                    },
                    ExtendedEventItem {
                        item_description: vec![],
                        item: b"and".to_vec(),
                    },
                ],
                text: b"Hello".to_vec(),
            })
        );
    }

    #[test]
    fn reject_extended_event_descriptor_with_an_item_running_past_its_end() {
        let error = Descriptor::read(&mut Bytes::from_static(&[
            0x4E, 0x08, // descriptor_tag, descriptor_length
            0x00, // descriptor_number, last_descriptor_number
            b'j', b'p', b'n', // ISO_639_language_code
            0x03, // length_of_items
            0x04, b'C', b'a', // item_description_length, truncated item_description
        ]))
        .unwrap_err();

        assert_eq!(error.kind(), ErrorKind::UnexpectedEof);
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
