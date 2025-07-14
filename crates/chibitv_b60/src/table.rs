use std::io::{ErrorKind, Result};
use std::net::{Ipv4Addr, Ipv6Addr};

use bytes::{Buf, Bytes};
use chrono::{Duration, NaiveDateTime, NaiveTime};
use julianday::ModifiedJulianDay;
use strum::FromRepr;

use crate::descriptor::Descriptor;
use crate::read_ext::BytesExt;

#[derive(Copy, Clone, Debug, Eq, FromRepr, PartialEq)]
#[repr(u8)]
pub enum MmtLocationType {
    None = 0x00,
    Ipv4 = 0x01,
    Ipv6 = 0x02,
    M2ts = 0x03,
    M2Ipv6 = 0x04,
    Url = 0x05,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MmtGeneralLocation {
    None {
        packet_id: u16,
    },
    Ipv4 {
        src_addr: Ipv4Addr,
        dst_addr: Ipv4Addr,
        dst_port: u16,
        packet_id: u16,
    },
    Ipv6 {
        src_addr: Ipv6Addr,
        dst_addr: Ipv6Addr,
        dst_port: u16,
        packet_id: u16,
    },
    M2ts {
        network_id: u16,
        m2_transport_stream_id: u16,
        m2_pid: u16,
    },
    M2Ipv6 {
        src_addr: Ipv6Addr,
        dst_addr: Ipv6Addr,
        dst_port: u16,
        m2_pid: u16,
    },
    Url(Vec<u8>),
}

impl MmtGeneralLocation {
    pub fn packet_id(&self) -> Option<u16> {
        match self {
            Self::None { packet_id } => Some(*packet_id),
            Self::Ipv4 { packet_id, .. } => Some(*packet_id),
            Self::Ipv6 { packet_id, .. } => Some(*packet_id),
            _ => None,
        }
    }
}

impl MmtGeneralLocation {
    pub fn read(bytes: &mut Bytes) -> Result<Self> {
        let location_type =
            MmtLocationType::from_repr(bytes.get_u8()).ok_or(ErrorKind::InvalidData)?;

        Ok(match location_type {
            MmtLocationType::None => {
                let packet_id = bytes.get_u16();

                Self::None { packet_id }
            }
            MmtLocationType::Ipv4 => {
                let src_addr = bytes.get_ipv4_addr();
                let dst_addr = bytes.get_ipv4_addr();
                let dst_port = bytes.get_u16();
                let packet_id = bytes.get_u16();

                Self::Ipv4 {
                    src_addr,
                    dst_addr,
                    dst_port,
                    packet_id,
                }
            }
            MmtLocationType::Ipv6 => {
                let src_addr = bytes.get_ipv6_addr();
                let dst_addr = bytes.get_ipv6_addr();
                let dst_port = bytes.get_u16();
                let packet_id = bytes.get_u16();

                Self::Ipv6 {
                    src_addr,
                    dst_addr,
                    dst_port,
                    packet_id,
                }
            }
            MmtLocationType::M2ts => {
                let network_id = bytes.get_u16();
                let m2_transport_stream_id = bytes.get_u16();
                let m2_pid = bytes.get_u16() & 0b0001_1111_1111_1111;

                Self::M2ts {
                    network_id,
                    m2_transport_stream_id,
                    m2_pid,
                }
            }
            MmtLocationType::M2Ipv6 => {
                let src_addr = bytes.get_ipv6_addr();
                let dst_addr = bytes.get_ipv6_addr();
                let dst_port = bytes.get_u16();
                let m2_pid = bytes.get_u16() & 0b0001_1111_1111_1111;

                Self::M2Ipv6 {
                    src_addr,
                    dst_addr,
                    dst_port,
                    m2_pid,
                }
            }
            MmtLocationType::Url => {
                let url_length = bytes.get_u8();
                let url = bytes.split_to(url_length as usize).to_vec();

                Self::Url(url)
            }
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum IpDeliveryLocation {
    Ipv4 {
        src_addr: Ipv4Addr,
        dst_addr: Ipv4Addr,
        dst_port: u16,
    },
    Ipv6 {
        src_addr: Ipv6Addr,
        dst_addr: Ipv6Addr,
        dst_port: u16,
    },
    Url(Vec<u8>),
}

impl IpDeliveryLocation {
    pub fn read(bytes: &mut Bytes) -> Result<Self> {
        let location_type =
            MmtLocationType::from_repr(bytes.get_u8()).ok_or(ErrorKind::InvalidData)?;

        Ok(match location_type {
            MmtLocationType::Ipv4 => {
                let src_addr = bytes.get_ipv4_addr();
                let dst_addr = bytes.get_ipv4_addr();
                let dst_port = bytes.get_u16();

                Self::Ipv4 {
                    src_addr,
                    dst_addr,
                    dst_port,
                }
            }
            MmtLocationType::Ipv6 => {
                let src_addr = bytes.get_ipv6_addr();
                let dst_addr = bytes.get_ipv6_addr();
                let dst_port = bytes.get_u16();

                Self::Ipv6 {
                    src_addr,
                    dst_addr,
                    dst_port,
                }
            }
            MmtLocationType::Url => {
                let url_length = bytes.get_u8();
                let url = bytes.split_to(url_length as usize).to_vec();

                Self::Url(url)
            }
            _ => unreachable!(),
        })
    }
}

#[derive(Clone, Debug)]
pub struct MmtIpDelivery {
    pub transport_file_id: u32,
    pub location: IpDeliveryLocation,
    pub descriptors: Vec<Descriptor>,
}

impl MmtIpDelivery {
    pub fn read(bytes: &mut Bytes) -> Result<Self> {
        let transport_file_id = bytes.get_u32();
        let location = IpDeliveryLocation::read(bytes)?;

        let descriptor_loop_length = bytes.get_u16();
        let mut descriptors = Vec::with_capacity(descriptor_loop_length as usize);
        for _ in 0..descriptor_loop_length {
            let descriptor = Descriptor::read(bytes)?;
            descriptors.push(descriptor);
        }

        Ok(Self {
            transport_file_id,
            location,
            descriptors,
        })
    }
}

/// Package List Table (PLT).
#[derive(Clone, Debug)]
pub struct Plt {
    pub version: u8,
    pub packages: Vec<(Vec<u8>, MmtGeneralLocation)>,
    pub ip_deliveries: Vec<MmtIpDelivery>,
}

impl Plt {
    pub fn read(bytes: &mut Bytes) -> Result<Self> {
        let version = bytes.get_u8();
        let _length = bytes.get_u16();

        let num_of_package = bytes.get_u8();
        let mut packages = Vec::with_capacity(num_of_package as usize);
        for _ in 0..num_of_package {
            let mmt_package_id_length = bytes.get_u8();
            let mmt_package_id = bytes.split_to(mmt_package_id_length as usize).to_vec();

            let mmt_general_location = MmtGeneralLocation::read(bytes)?;

            packages.push((mmt_package_id, mmt_general_location));
        }

        let num_of_ip_delivery = bytes.get_u8();
        let mut ip_deliveries = Vec::with_capacity(num_of_ip_delivery as usize);
        for _ in 0..num_of_ip_delivery {
            ip_deliveries.push(MmtIpDelivery::read(bytes)?);
        }

        Ok(Self {
            version,
            packages,
            ip_deliveries,
        })
    }
}

#[derive(Clone, Debug)]
pub struct MmtAsset {
    pub identifier_type: u8,
    pub asset_id_scheme: [u8; 4],
    pub asset_id: Vec<u8>,
    pub asset_type: [u8; 4],
    pub asset_clock_relation_flag: bool,
    pub locations: Vec<MmtGeneralLocation>,
    pub asset_descriptors: Vec<Descriptor>,
}

impl MmtAsset {
    pub fn read(bytes: &mut Bytes) -> Result<Self> {
        let identifier_type = bytes.get_u8();
        let asset_id_scheme = bytes.get_byte_array::<4>();

        let asset_id_length = bytes.get_u8();
        let asset_id = bytes.split_to(asset_id_length as usize).to_vec();

        let asset_type = bytes.get_byte_array::<4>();

        let head = bytes.get_u8();
        let asset_clock_relation_flag = (head & 0b0000_0001) == 1;

        let location_count = bytes.get_u8();
        let mut locations = Vec::with_capacity(location_count as usize);
        for _ in 0..location_count {
            locations.push(MmtGeneralLocation::read(bytes)?);
        }

        let asset_descriptors_length = bytes.get_u16();
        assert!(bytes.remaining() >= asset_descriptors_length as usize);

        let mut bytes = bytes.split_to(asset_descriptors_length as usize);
        let mut asset_descriptors = Vec::new();
        while bytes.has_remaining() {
            asset_descriptors.push(Descriptor::read(&mut bytes)?);
        }

        Ok(Self {
            identifier_type,
            asset_id_scheme,
            asset_id,
            asset_type,
            asset_clock_relation_flag,
            locations,
            asset_descriptors,
        })
    }
}

#[derive(Copy, Clone, Debug, Eq, FromRepr, PartialEq)]
#[repr(u8)]
pub enum MptMode {
    Ordered = 0b00,
    AfterZero = 0b01,
    Arbitrary = 0b10,
}

/// MMT Package Table (MPT).
#[derive(Clone, Debug)]
pub struct Mpt {
    pub version: u8,
    pub mpt_mode: MptMode,
    pub mmt_package_id: Vec<u8>,
    pub mmt_descriptors: Vec<u8>,
    pub assets: Vec<MmtAsset>,
}

impl Mpt {
    pub fn read(bytes: &mut Bytes) -> Result<Self> {
        let version = bytes.get_u8();
        let length = bytes.get_u16();
        assert_eq!(bytes.remaining(), length as usize);

        let head = bytes.get_u8();
        let mpt_mode = MptMode::from_repr(head & 0b0000_0011).ok_or(ErrorKind::InvalidData)?;

        let mmt_package_id_length = bytes.get_u8();
        let mmt_package_id = bytes.split_to(mmt_package_id_length as usize).into();

        let mmt_descriptors_length = bytes.get_u16();
        let mmt_descriptors = bytes.split_to(mmt_descriptors_length as usize).into();

        let number_of_assets = bytes.get_u8();
        let mut assets = Vec::with_capacity(number_of_assets as usize);
        for _ in 0..number_of_assets {
            assets.push(MmtAsset::read(bytes)?);
        }

        Ok(Self {
            version,
            mpt_mode,
            mmt_package_id,
            mmt_descriptors,
            assets,
        })
    }
}

#[derive(Copy, Clone, Debug, Eq, FromRepr, PartialEq)]
#[repr(u8)]
pub enum EventRunningStatus {
    Undefined = 0,
    InNonOperation = 1,
    WillStartSoon = 2,
    OutOfOperation = 3,
    InOperation = 4,
}

#[derive(Clone, Debug)]
pub struct EventInformation {
    pub event_id: u16,
    pub start_time: Option<NaiveDateTime>,
    pub duration: Option<Duration>,
    pub running_status: EventRunningStatus,
    pub free_ca_mode: bool,
    pub descriptors: Vec<Descriptor>,
}

impl EventInformation {
    pub fn read(bytes: &mut Bytes) -> Result<Self> {
        let event_id = bytes.get_u16();
        let start_time = parse_start_time(bytes.get_byte_array::<5>());
        let duration = parse_duration(bytes.get_byte_array::<3>());

        let head = bytes.get_u16();
        let running_status = EventRunningStatus::from_repr(((head & 0xE000) >> 13) as u8).unwrap();
        let free_ca_mode = ((head & 0x1000) >> 12) == 1;
        let descriptors_loop_length = head & 0x0FFF;

        let mut bytes = bytes.split_to(descriptors_loop_length as usize);
        let mut descriptors = Vec::new();
        while bytes.has_remaining() {
            descriptors.push(Descriptor::read(&mut bytes)?);
        }

        Ok(Self {
            event_id,
            start_time,
            duration,
            running_status,
            free_ca_mode,
            descriptors,
        })
    }
}

fn parse_start_time(start_time: [u8; 5]) -> Option<NaiveDateTime> {
    if start_time == [0xFF, 0xFF, 0xFF, 0xFF, 0xFF] {
        return None;
    }

    let mjd = u16::from_be_bytes([start_time[0], start_time[1]]);
    let date = ModifiedJulianDay::new(mjd as i32).to_date();

    let hour = parse_bcd(start_time[2]) as u32;
    let minute = parse_bcd(start_time[3]) as u32;
    let second = parse_bcd(start_time[4]) as u32;
    let time = NaiveTime::from_hms_opt(hour, minute, second).unwrap();

    Some(NaiveDateTime::new(date, time))
}

fn parse_duration(duration: [u8; 3]) -> Option<Duration> {
    if duration == [0xFF, 0xFF, 0xFF] {
        return None;
    }

    let hours = parse_bcd(duration[0]) as i64;
    let minutes = parse_bcd(duration[1]) as i64;
    let seconds = parse_bcd(duration[2]) as i64;

    Some(Duration::hours(hours) + Duration::minutes(minutes) + Duration::seconds(seconds))
}

fn parse_bcd(bcd: u8) -> u8 {
    (bcd >> 4) * 10 + (bcd & 0xF)
}

/// MH-EIT (Event Information Table).
#[derive(Clone, Debug)]
pub struct MhEit {
    pub section_syntax_indicator: bool,
    pub section_length: u16,
    pub service_id: u16,
    pub version_number: u8,
    pub current_next_indicator: bool,
    pub section_number: u8,
    pub last_section_number: u8,
    pub tlv_stream_id: u16,
    pub original_network_id: u16,
    pub segment_last_section_number: u8,
    pub last_table_id: u8,
    pub events: Vec<EventInformation>,
    pub crc_32: u32,
}

impl MhEit {
    pub fn read(bytes: &mut Bytes) -> Result<Self> {
        let head = bytes.get_u16();
        let section_syntax_indicator = ((head & 0x8000) >> 15) == 1;
        let section_length = head & 0x0FFF;
        let service_id = bytes.get_u16();

        let head = bytes.get_u8();
        let version_number = (head & 0b0011_1110) >> 1;
        let current_next_indicator = (head & 0b0000_0001) == 1;

        let section_number = bytes.get_u8();
        let last_section_number = bytes.get_u8();
        let tlv_stream_id = bytes.get_u16();
        let original_network_id = bytes.get_u16();
        let segment_last_section_number = bytes.get_u8();
        let last_table_id = bytes.get_u8();

        let mut events = Vec::new();
        while bytes.remaining() > 4 {
            events.push(EventInformation::read(bytes)?);
        }

        // TODO: Verify CRC
        let crc_32 = bytes.get_u32();

        Ok(Self {
            section_syntax_indicator,
            section_length,
            service_id,
            version_number,
            current_next_indicator,
            section_number,
            last_section_number,
            tlv_stream_id,
            original_network_id,
            segment_last_section_number,
            last_table_id,
            events,
            crc_32,
        })
    }
}

#[derive(Clone, Debug)]
pub struct BroadcasterInformation {
    pub broadcaster_id: u8,
    pub descriptors: Vec<Descriptor>,
}

impl BroadcasterInformation {
    pub fn read(bytes: &mut Bytes) -> Result<Self> {
        let broadcaster_id = bytes.get_u8();

        let broadcaster_descriptors_length = bytes.get_u16() & 0xFFF;
        let mut bytes = bytes.split_to(broadcaster_descriptors_length as usize);
        let mut descriptors = Vec::new();
        while bytes.has_remaining() {
            descriptors.push(Descriptor::read(&mut bytes)?);
        }

        Ok(Self {
            broadcaster_id,
            descriptors,
        })
    }
}

/// MH-BIT (Broadcaster Information Table).
#[derive(Clone, Debug)]
pub struct MhBit {
    pub section_syntax_indicator: bool,
    pub section_length: u16,
    pub original_network_id: u16,
    pub version_number: u8,
    pub current_next_indicator: bool,
    pub section_number: u8,
    pub last_section_number: u8,
    pub broadcast_view_propriety: bool,
    pub descriptors: Vec<Descriptor>,
    pub broadcasters: Vec<BroadcasterInformation>,
    pub crc_32: u32,
}

impl MhBit {
    pub fn read(bytes: &mut Bytes) -> Result<Self> {
        let head = bytes.get_u16();
        let section_syntax_indicator = ((head & 0x8000) >> 15) == 1;
        let section_length = head & 0x0FFF;
        let original_network_id = bytes.get_u16();

        let head = bytes.get_u8();
        let version_number = (head & 0b0011_1110) >> 1;
        let current_next_indicator = (head & 0b0000_0001) == 1;

        let section_number = bytes.get_u8();
        let last_section_number = bytes.get_u8();

        let head = bytes.get_u16();
        let broadcast_view_propriety = ((head & 0x1000) >> 12) == 1;

        let descriptors = {
            let first_descriptors_length = head & 0x0FFF;
            let mut bytes = bytes.split_to(first_descriptors_length as usize);
            let mut descriptors = Vec::new();
            while bytes.has_remaining() {
                descriptors.push(Descriptor::read(&mut bytes)?);
            }

            descriptors
        };

        let mut broadcasters = Vec::new();
        while bytes.remaining() > 4 {
            broadcasters.push(BroadcasterInformation::read(bytes)?);
        }

        // TODO: Verify CRC
        let crc_32 = bytes.get_u32();

        Ok(Self {
            section_syntax_indicator,
            section_length,
            original_network_id,
            version_number,
            current_next_indicator,
            section_number,
            last_section_number,
            broadcast_view_propriety,
            descriptors,
            broadcasters,
            crc_32,
        })
    }
}

#[derive(Clone, Debug)]
pub struct ServiceInformation {
    pub service_id: u16,
    pub eit_user_defined_flags: u8,
    pub eit_schedule_flag: bool,
    pub eit_present_following_flag: bool,
    pub running_status: u8,
    pub free_ca_mode: bool,
    pub descriptors: Vec<Descriptor>,
}

impl ServiceInformation {
    pub fn read(bytes: &mut Bytes) -> Result<Self> {
        let service_id = bytes.get_u16();

        let head = bytes.get_u8();
        let eit_user_defined_flags = (head & 0b0001_1100) >> 2;
        let eit_schedule_flag = ((head & 0b0000_0010) >> 1) == 1;
        let eit_present_following_flag = (head & 0b0000_0001) == 1;

        let head = bytes.get_u16();
        let running_status = ((head & 0xE000) >> 13) as u8;
        let free_ca_mode = ((head & 0x1000) >> 12) == 1;
        let descriptors_loop_length = head & 0x0FFF;

        let mut bytes = bytes.split_to(descriptors_loop_length as usize);
        let mut descriptors = Vec::new();
        while bytes.has_remaining() {
            descriptors.push(Descriptor::read(&mut bytes)?);
        }

        Ok(Self {
            service_id,
            eit_user_defined_flags,
            eit_schedule_flag,
            eit_present_following_flag,
            running_status,
            free_ca_mode,
            descriptors,
        })
    }
}

/// MH-SDT (Service Description Table).
#[derive(Clone, Debug)]
pub struct MhSdt {
    pub section_syntax_indicator: bool,
    pub section_length: u16,
    pub tlv_stream_id: u16,
    pub version_number: u8,
    pub current_next_indicator: bool,
    pub section_number: u8,
    pub last_section_number: u8,
    pub original_network_id: u16,
    pub services: Vec<ServiceInformation>,
    pub crc_32: u32,
}

impl MhSdt {
    pub fn read(bytes: &mut Bytes) -> Result<Self> {
        let head = bytes.get_u16();
        let section_syntax_indicator = ((head & 0x8000) >> 15) == 1;
        let section_length = head & 0x0FFF;
        let tlv_stream_id = bytes.get_u16();

        let head = bytes.get_u8();
        let version_number = (head & 0b0011_1110) >> 1;
        let current_next_indicator = (head & 0b0000_0001) == 1;

        let section_number = bytes.get_u8();
        let last_section_number = bytes.get_u8();
        let original_network_id = bytes.get_u16();

        _ = bytes.get_u8(); // reserved_future_use

        let mut services = Vec::new();
        while bytes.remaining() > 4 {
            services.push(ServiceInformation::read(bytes)?);
        }

        // TODO: Verify CRC
        let crc_32 = bytes.get_u32();

        Ok(Self {
            section_syntax_indicator,
            section_length,
            tlv_stream_id,
            version_number,
            current_next_indicator,
            section_number,
            last_section_number,
            original_network_id,
            services,
            crc_32,
        })
    }
}

#[derive(Clone, Debug)]
pub struct SelectionInformation {
    pub service_id: u16,
    pub running_status: u8,
    pub descriptors: Vec<Descriptor>,
}

impl SelectionInformation {
    pub fn read(bytes: &mut Bytes) -> Result<Self> {
        let service_id = bytes.get_u16();

        let head = bytes.get_u16();
        let running_status = ((head & 0x7000) >> 12) as u8;
        let service_loop_length = head & 0x0FFF;

        let mut bytes = bytes.split_to(service_loop_length as usize);
        let mut descriptors = Vec::new();
        while bytes.has_remaining() {
            descriptors.push(Descriptor::read(&mut bytes)?)
        }

        Ok(Self {
            service_id,
            running_status,
            descriptors,
        })
    }
}

/// MH-SIT (Selection Information Table).
#[derive(Clone, Debug)]
pub struct MhSit {
    pub section_syntax_indicator: bool,
    pub section_length: u16,
    pub version_number: u8,
    pub current_next_indicator: bool,
    pub section_number: u8,
    pub last_section_number: u8,
    pub descriptors: Vec<Descriptor>,
    pub selections: Vec<SelectionInformation>,
    pub crc_32: u32,
}

impl MhSit {
    pub fn read(bytes: &mut Bytes) -> Result<Self> {
        let head = bytes.get_u16();
        let section_syntax_indicator = ((head & 0x8000) >> 15) == 1;
        let section_length = head & 0x0FFF;

        _ = bytes.get_u16(); // reserved_future_use

        let head = bytes.get_u8();
        let version_number = (head & 0b0011_1110) >> 1;
        let current_next_indicator = (head & 0b0000_0001) == 1;

        let section_number = bytes.get_u8();
        let last_section_number = bytes.get_u8();

        let head = bytes.get_u16();
        let transmission_info_loop_length = head & 0xFFF;

        let descriptors = {
            let mut bytes = bytes.split_to(transmission_info_loop_length as usize);
            let mut descriptors = Vec::new();
            while bytes.has_remaining() {
                descriptors.push(Descriptor::read(&mut bytes)?);
            }

            descriptors
        };

        let mut selections = Vec::new();
        while bytes.remaining() > 4 {
            selections.push(SelectionInformation::read(bytes)?);
        }

        // TODO: Verify CRC
        let crc_32 = bytes.get_u32();

        Ok(Self {
            section_syntax_indicator,
            section_length,
            version_number,
            current_next_indicator,
            section_number,
            last_section_number,
            descriptors,
            selections,
            crc_32,
        })
    }
}

const MPT_ID: u8 = 0x20;
const PLT_ID: u8 = 0x80;
const MH_EIT_ID: u8 = 0x8B;
const MH_EIT_SCHEDULE_ID_START: u8 = 0x8C;
const MH_EIT_SCHEDULE_ID_END: u8 = 0x9B;
const MH_BIT_ID: u8 = 0x9D;
const MH_SDT_ID: u8 = 0x9F;
const MH_SDT_OTHER_ID: u8 = 0xA0;
const MH_SIT_ID: u8 = 0xA8;

#[derive(Clone, Debug)]
pub enum Table {
    Mpt(Mpt),
    Plt(Plt),
    MhEit(MhEit),
    MhBit(MhBit),
    MhSdt(MhSdt),
    MhSit(MhSit),
    Unknown(u8, Vec<u8>),
}

impl Table {
    pub fn read(bytes: &mut Bytes) -> Result<Self> {
        let table_id = bytes.get_u8();

        Ok(match table_id {
            MPT_ID => Self::Mpt(Mpt::read(bytes)?),
            PLT_ID => Self::Plt(Plt::read(bytes)?),
            MH_EIT_ID | MH_EIT_SCHEDULE_ID_START..=MH_EIT_SCHEDULE_ID_END => {
                Self::MhEit(MhEit::read(bytes)?)
            }
            MH_BIT_ID => Self::MhBit(MhBit::read(bytes)?),
            MH_SDT_ID | MH_SDT_OTHER_ID => Self::MhSdt(MhSdt::read(bytes)?),
            MH_SIT_ID => Self::MhSit(MhSit::read(bytes)?),
            _ => Self::Unknown(table_id, bytes.to_vec()),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;

    #[test]
    fn test_parse_start_time() {
        assert_eq!(
            parse_start_time([0xC0, 0x79, 0x12, 0x45, 0x00]),
            Some(NaiveDateTime::new(
                NaiveDate::from_ymd_opt(1993, 10, 13).unwrap(),
                NaiveTime::from_hms_opt(12, 45, 0).unwrap()
            )),
        );
    }

    #[test]
    fn test_parse_duration() {
        let duration = parse_duration([0x01, 0x45, 0x30]).unwrap();

        assert_eq!(duration.num_hours(), 1);
        assert_eq!(duration.num_minutes() % 60, 45);
        assert_eq!(duration.num_seconds() % 60, 30);
    }
}
