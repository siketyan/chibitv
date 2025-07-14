use std::io::Result;

use crate::read_ext::BytesExt;
use bytes::{Buf, Bytes};
use strum::FromRepr;

#[derive(Clone, Debug)]
pub struct MpuTimestamp {
    pub mpu_sequence_number: u32,
    pub mpu_presentation_time: u64,
}

impl MpuTimestamp {
    pub fn read(bytes: &mut Bytes) -> Result<Self> {
        assert!(bytes.remaining() >= 12);

        let mpu_sequence_number = bytes.get_u32();
        let mpu_presentation_time = bytes.get_u64();

        Ok(Self {
            mpu_sequence_number,
            mpu_presentation_time,
        })
    }
}

#[derive(Clone, Debug)]
pub struct MpuTimestampDescriptor {
    pub timestamps: Vec<MpuTimestamp>,
}

impl MpuTimestampDescriptor {
    pub fn read(bytes: &mut Bytes) -> Result<Self> {
        let mut timestamps = Vec::new();
        while bytes.has_remaining() {
            timestamps.push(MpuTimestamp::read(bytes)?);
        }

        Ok(Self { timestamps })
    }
}

#[derive(Clone, Debug)]
pub struct MpuTimestampOffset {
    pub pts_dts_offset: u16,
    pub pts_offset: u16,
}

impl MpuTimestampOffset {
    pub fn read(
        bytes: &mut Bytes,
        pts_offset_type: u8,
        default_pts_offset: Option<u16>,
    ) -> Result<Self> {
        let pts_dts_offset = bytes.get_u16();
        let pts_offset = (pts_offset_type == 2)
            .then(|| bytes.get_u16())
            .or(default_pts_offset)
            .unwrap();

        Ok(Self {
            pts_dts_offset,
            pts_offset,
        })
    }
}

#[derive(Clone, Debug)]
pub struct MpuExtendedTimestamp {
    pub mpu_sequence_number: u32,
    pub mpu_presentation_time_leap_indicator: u8,
    pub mpu_decoding_time_offset: u16,
    pub num_of_au: u8,
    pub offsets: Vec<MpuTimestampOffset>,
}

impl MpuExtendedTimestamp {
    pub fn read(
        bytes: &mut Bytes,
        pts_offset_type: u8,
        default_pts_offset: Option<u16>,
    ) -> Result<Self> {
        let mpu_sequence_number = bytes.get_u32();
        let mpu_presentation_time_leap_indicator = (bytes.get_u8() & 0b1100_0000) >> 6;
        let mpu_decoding_time_offset = bytes.get_u16();
        let num_of_au = bytes.get_u8();

        let mut offsets = Vec::with_capacity(num_of_au as usize);
        for _ in 0..num_of_au {
            offsets.push(MpuTimestampOffset::read(
                bytes,
                pts_offset_type,
                default_pts_offset,
            )?);
        }

        Ok(Self {
            mpu_sequence_number,
            mpu_presentation_time_leap_indicator,
            mpu_decoding_time_offset,
            num_of_au,
            offsets,
        })
    }
}

#[derive(Clone, Debug)]
pub struct MpuExtendedTimestampDescriptor {
    pub pts_offset_type: u8,
    pub timescale: Option<u32>,
    pub timestamps: Vec<MpuExtendedTimestamp>,
}

impl MpuExtendedTimestampDescriptor {
    pub fn read(bytes: &mut Bytes) -> Result<Self> {
        let head = bytes.get_u8();
        let pts_offset_type = (head & 0b0000_0110) >> 1;
        let timescale_flag = (head & 0b0000_0001) == 1;

        let timescale = timescale_flag.then(|| bytes.get_u32());
        let default_pts_offset = (pts_offset_type == 1).then(|| bytes.get_u16());

        let mut timestamps = Vec::new();
        while bytes.has_remaining() {
            timestamps.push(MpuExtendedTimestamp::read(
                bytes,
                pts_offset_type,
                default_pts_offset,
            )?);
        }

        Ok(Self {
            pts_offset_type,
            timescale,
            timestamps,
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MhShortEventDescriptor {
    pub iso_639_language_code: [u8; 3],
    pub event_name: Vec<u8>,
    pub text: Vec<u8>,
}

impl MhShortEventDescriptor {
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
        let item_description = bytes.split_to(item_description_length as usize).into();

        let item_length = bytes.get_u16();
        let item = bytes.split_to(item_length as usize).into();

        Ok(Self {
            item_description,
            item,
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MhExtendedEventDescriptor {
    pub descriptor_number: u8,
    pub last_descriptor_number: u8,
    pub iso_639_language_code: [u8; 3],
    pub items: Vec<ExtendedEventItem>,
    pub text: Vec<u8>,
}

impl MhExtendedEventDescriptor {
    pub fn read(bytes: &mut Bytes) -> Result<Self> {
        let head = bytes.get_u8();
        let descriptor_number = (head & 0xF0) >> 4;
        let last_descriptor_number = head & 0x0F;

        let iso_639_language_code = bytes.get_byte_array::<3>();

        let items = {
            let length_of_items = bytes.get_u16();
            let mut bytes = bytes.split_to(length_of_items as usize);
            let mut items = Vec::new();
            while bytes.has_remaining() {
                items.push(ExtendedEventItem::read(&mut bytes)?);
            }

            items
        };

        let text_length = bytes.get_u16();
        let text = bytes.split_to(text_length as usize).into();

        Ok(Self {
            descriptor_number,
            last_descriptor_number,
            iso_639_language_code,
            items,
            text,
        })
    }
}

#[derive(Clone, Debug)]
pub struct MhBroadcasterNameDescriptor {
    pub name: Vec<u8>,
}

impl MhBroadcasterNameDescriptor {
    pub fn read(bytes: &mut Bytes) -> Result<Self> {
        Ok(Self {
            name: bytes.to_vec(),
        })
    }
}

#[derive(Clone, Debug)]
pub struct MhServiceDescriptor {
    pub service_type: u8,
    pub service_provider_name: Vec<u8>,
    pub service_name: Vec<u8>,
}

impl MhServiceDescriptor {
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

#[derive(Clone, Debug)]
pub struct MhBroadcastIdDescriptor {
    pub original_network_id: u16,
    pub tlv_stream_id: u16,
    pub event_id: u16,
    pub broadcaster_id: u8,
}

impl MhBroadcastIdDescriptor {
    pub fn read(bytes: &mut Bytes) -> Result<Self> {
        let original_network_id = bytes.get_u16();
        let tlv_stream_id = bytes.get_u16();
        let event_id = bytes.get_u16();
        let broadcaster_id = bytes.get_u8();

        Ok(Self {
            original_network_id,
            tlv_stream_id,
            event_id,
            broadcaster_id,
        })
    }
}

#[derive(Clone, Debug, FromRepr)]
#[repr(u16)]
pub enum DescriptorTag {
    MpuTimestampDescriptor = 0x0001,
    MpuExtendedTimestampDescriptor = 0x8026,
    MhBroadcasterNameDescriptor = 0x8018,
    MhServiceDescriptor = 0x8019,
    MhShortEventDescriptor = 0xF001,
    MhExtendedEventDescriptor = 0xF002,
    MhBroadcastIdDescriptor = 0xF005,
}

#[derive(Clone, Debug)]
pub enum Descriptor {
    MpuTimestamp(MpuTimestampDescriptor),
    MpuExtendedTimestamp(MpuExtendedTimestampDescriptor),
    MhBroadcasterName(MhBroadcasterNameDescriptor),
    MhService(MhServiceDescriptor),
    MhShortEvent(MhShortEventDescriptor),
    MhExtendedEvent(MhExtendedEventDescriptor),
    MhBroadcastIdDescriptor(MhBroadcastIdDescriptor),
    Unknown(u16, Vec<u8>),
}

impl Descriptor {
    pub fn read(bytes: &mut Bytes) -> Result<Self> {
        let descriptor_tag = bytes.get_u16();
        let descriptor_length = if descriptor_tag <= 0x3FFF {
            bytes.get_u8() as usize
        } else if descriptor_tag <= 0x6FFF {
            bytes.get_u16() as usize
        } else if descriptor_tag <= 0x7FFF {
            bytes.get_u32() as usize
        } else if descriptor_tag <= 0xEFFF {
            bytes.get_u8() as usize
        } else {
            bytes.get_u16() as usize
        };

        let mut bytes = bytes.split_to(descriptor_length);
        let Some(descriptor_tag) = DescriptorTag::from_repr(descriptor_tag) else {
            return Ok(Self::Unknown(descriptor_tag, bytes.into()));
        };

        Ok(match descriptor_tag {
            DescriptorTag::MpuTimestampDescriptor => {
                Self::MpuTimestamp(MpuTimestampDescriptor::read(&mut bytes)?)
            }
            DescriptorTag::MpuExtendedTimestampDescriptor => {
                Self::MpuExtendedTimestamp(MpuExtendedTimestampDescriptor::read(&mut bytes)?)
            }
            DescriptorTag::MhBroadcasterNameDescriptor => {
                Self::MhBroadcasterName(MhBroadcasterNameDescriptor::read(&mut bytes)?)
            }
            DescriptorTag::MhServiceDescriptor => {
                Self::MhService(MhServiceDescriptor::read(&mut bytes)?)
            }
            DescriptorTag::MhShortEventDescriptor => {
                Self::MhShortEvent(MhShortEventDescriptor::read(&mut bytes)?)
            }
            DescriptorTag::MhExtendedEventDescriptor => {
                Self::MhExtendedEvent(MhExtendedEventDescriptor::read(&mut bytes)?)
            }
            DescriptorTag::MhBroadcastIdDescriptor => {
                Self::MhBroadcastIdDescriptor(MhBroadcastIdDescriptor::read(&mut bytes)?)
            }
        })
    }
}
