use std::io::Result;
use std::net::{Ipv4Addr, Ipv6Addr};

use bytes::{Buf, Bytes};
use chrono::{Duration, NaiveDateTime, NaiveTime};
use julianday::ModifiedJulianDay;
use strum::FromRepr;

use crate::read_ext::BytesExt;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Descriptor {
    pub tag: u8,
    pub data: Vec<u8>,
}

impl Descriptor {
    pub fn read(bytes: &mut Bytes) -> Result<Self> {
        let tag = bytes.get_u8();
        let length = bytes.get_u8();
        let data = bytes.split_to(length as usize).into();

        Ok(Self { tag, data })
    }
}

fn read_descriptors(bytes: &mut Bytes, length: usize) -> Result<Vec<Descriptor>> {
    let mut bytes = bytes.split_to(length);
    let mut descriptors = Vec::new();
    while bytes.has_remaining() {
        descriptors.push(Descriptor::read(&mut bytes)?);
    }

    Ok(descriptors)
}

fn read_section_header(bytes: &mut Bytes) -> (bool, u16) {
    let head = bytes.get_u16();
    (((head & 0x8000) >> 15) == 1, head & 0x0FFF)
}

fn read_version(bytes: &mut Bytes) -> (u8, bool) {
    let head = bytes.get_u8();
    ((head & 0b0011_1110) >> 1, (head & 0b0000_0001) == 1)
}

#[derive(Copy, Clone, Debug, Eq, FromRepr, PartialEq)]
#[repr(u8)]
pub enum RunningStatus {
    Undefined = 0,
    NotRunning = 1,
    StartsSoon = 2,
    Pausing = 3,
    Running = 4,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ProgramAssociation {
    Network {
        program_number: u16,
        network_pid: u16,
    },
    ProgramMap {
        program_number: u16,
        program_map_pid: u16,
    },
}

impl ProgramAssociation {
    pub fn read(bytes: &mut Bytes) -> Result<Self> {
        let program_number = bytes.get_u16();
        let pid = bytes.get_u16() & 0x1FFF;

        Ok(if program_number == 0 {
            Self::Network {
                program_number,
                network_pid: pid,
            }
        } else {
            Self::ProgramMap {
                program_number,
                program_map_pid: pid,
            }
        })
    }
}

/// PAT (Program Association Table).
#[derive(Clone, Debug)]
pub struct Pat {
    pub section_syntax_indicator: bool,
    pub section_length: u16,
    pub transport_stream_id: u16,
    pub version_number: u8,
    pub current_next_indicator: bool,
    pub section_number: u8,
    pub last_section_number: u8,
    pub programs: Vec<ProgramAssociation>,
    pub crc_32: u32,
}

impl Pat {
    pub fn read(bytes: &mut Bytes) -> Result<Self> {
        let (section_syntax_indicator, section_length) = read_section_header(bytes);
        let transport_stream_id = bytes.get_u16();
        let (version_number, current_next_indicator) = read_version(bytes);
        let section_number = bytes.get_u8();
        let last_section_number = bytes.get_u8();

        let mut programs = Vec::new();
        while bytes.remaining() > 4 {
            programs.push(ProgramAssociation::read(bytes)?);
        }

        let crc_32 = bytes.get_u32();

        Ok(Self {
            section_syntax_indicator,
            section_length,
            transport_stream_id,
            version_number,
            current_next_indicator,
            section_number,
            last_section_number,
            programs,
            crc_32,
        })
    }
}

/// CAT (Conditional Access Table).
#[derive(Clone, Debug)]
pub struct Cat {
    pub section_syntax_indicator: bool,
    pub section_length: u16,
    pub version_number: u8,
    pub current_next_indicator: bool,
    pub section_number: u8,
    pub last_section_number: u8,
    pub descriptors: Vec<Descriptor>,
    pub crc_32: u32,
}

impl Cat {
    pub fn read(bytes: &mut Bytes) -> Result<Self> {
        let (section_syntax_indicator, section_length) = read_section_header(bytes);
        _ = bytes.get_u8();
        _ = bytes.get_u8();
        let (version_number, current_next_indicator) = read_version(bytes);
        let section_number = bytes.get_u8();
        let last_section_number = bytes.get_u8();

        let mut descriptors = Vec::new();
        while bytes.remaining() > 4 {
            descriptors.push(Descriptor::read(bytes)?);
        }

        let crc_32 = bytes.get_u32();

        Ok(Self {
            section_syntax_indicator,
            section_length,
            version_number,
            current_next_indicator,
            section_number,
            last_section_number,
            descriptors,
            crc_32,
        })
    }
}

#[derive(Clone, Debug)]
pub struct ElementaryStreamInfo {
    pub stream_type: u8,
    pub elementary_pid: u16,
    pub descriptors: Vec<Descriptor>,
}

impl ElementaryStreamInfo {
    pub fn read(bytes: &mut Bytes) -> Result<Self> {
        let stream_type = bytes.get_u8();
        let elementary_pid = bytes.get_u16() & 0x1FFF;
        let descriptors_loop_length = bytes.get_u16() & 0x0FFF;
        let descriptors = read_descriptors(bytes, descriptors_loop_length as usize)?;

        Ok(Self {
            stream_type,
            elementary_pid,
            descriptors,
        })
    }
}

/// PMT (Program Map Table).
#[derive(Clone, Debug)]
pub struct Pmt {
    pub section_syntax_indicator: bool,
    pub section_length: u16,
    pub program_number: u16,
    pub version_number: u8,
    pub current_next_indicator: bool,
    pub section_number: u8,
    pub last_section_number: u8,
    pub pcr_pid: u16,
    pub descriptors: Vec<Descriptor>,
    pub streams: Vec<ElementaryStreamInfo>,
    pub crc_32: u32,
}

impl Pmt {
    pub fn read(bytes: &mut Bytes) -> Result<Self> {
        let (section_syntax_indicator, section_length) = read_section_header(bytes);
        let program_number = bytes.get_u16();
        let (version_number, current_next_indicator) = read_version(bytes);
        let section_number = bytes.get_u8();
        let last_section_number = bytes.get_u8();
        let pcr_pid = bytes.get_u16() & 0x1FFF;
        let program_info_length = bytes.get_u16() & 0x0FFF;
        let descriptors = read_descriptors(bytes, program_info_length as usize)?;

        let mut streams = Vec::new();
        while bytes.remaining() > 4 {
            streams.push(ElementaryStreamInfo::read(bytes)?);
        }

        let crc_32 = bytes.get_u32();

        Ok(Self {
            section_syntax_indicator,
            section_length,
            program_number,
            version_number,
            current_next_indicator,
            section_number,
            last_section_number,
            pcr_pid,
            descriptors,
            streams,
            crc_32,
        })
    }
}

#[derive(Clone, Debug)]
pub struct TransportStreamInformation {
    pub transport_stream_id: u16,
    pub original_network_id: u16,
    pub descriptors: Vec<Descriptor>,
}

impl TransportStreamInformation {
    pub fn read(bytes: &mut Bytes) -> Result<Self> {
        let transport_stream_id = bytes.get_u16();
        let original_network_id = bytes.get_u16();
        let transport_descriptors_length = bytes.get_u16() & 0x0FFF;
        let descriptors = read_descriptors(bytes, transport_descriptors_length as usize)?;

        Ok(Self {
            transport_stream_id,
            original_network_id,
            descriptors,
        })
    }
}

/// NIT (Network Information Table).
#[derive(Clone, Debug)]
pub struct Nit {
    pub section_syntax_indicator: bool,
    pub section_length: u16,
    pub network_id: u16,
    pub version_number: u8,
    pub current_next_indicator: bool,
    pub section_number: u8,
    pub last_section_number: u8,
    pub descriptors: Vec<Descriptor>,
    pub transport_streams: Vec<TransportStreamInformation>,
    pub crc_32: u32,
}

impl Nit {
    pub fn read(bytes: &mut Bytes) -> Result<Self> {
        let (section_syntax_indicator, section_length) = read_section_header(bytes);
        let network_id = bytes.get_u16();
        let (version_number, current_next_indicator) = read_version(bytes);
        let section_number = bytes.get_u8();
        let last_section_number = bytes.get_u8();

        let network_descriptors_length = bytes.get_u16() & 0x0FFF;
        let descriptors = read_descriptors(bytes, network_descriptors_length as usize)?;

        let transport_stream_loop_length = bytes.get_u16() & 0x0FFF;
        let mut transport_bytes = bytes.split_to(transport_stream_loop_length as usize);
        let mut transport_streams = Vec::new();
        while transport_bytes.has_remaining() {
            transport_streams.push(TransportStreamInformation::read(&mut transport_bytes)?);
        }

        let crc_32 = bytes.get_u32();

        Ok(Self {
            section_syntax_indicator,
            section_length,
            network_id,
            version_number,
            current_next_indicator,
            section_number,
            last_section_number,
            descriptors,
            transport_streams,
            crc_32,
        })
    }
}

/// BAT (Bouquet Association Table).
#[derive(Clone, Debug)]
pub struct Bat {
    pub section_syntax_indicator: bool,
    pub section_length: u16,
    pub bouquet_id: u16,
    pub version_number: u8,
    pub current_next_indicator: bool,
    pub section_number: u8,
    pub last_section_number: u8,
    pub descriptors: Vec<Descriptor>,
    pub transport_streams: Vec<TransportStreamInformation>,
    pub crc_32: u32,
}

impl Bat {
    pub fn read(bytes: &mut Bytes) -> Result<Self> {
        let (section_syntax_indicator, section_length) = read_section_header(bytes);
        let bouquet_id = bytes.get_u16();
        let (version_number, current_next_indicator) = read_version(bytes);
        let section_number = bytes.get_u8();
        let last_section_number = bytes.get_u8();

        let bouquet_descriptors_length = bytes.get_u16() & 0x0FFF;
        let descriptors = read_descriptors(bytes, bouquet_descriptors_length as usize)?;

        let transport_stream_loop_length = bytes.get_u16() & 0x0FFF;
        let mut transport_bytes = bytes.split_to(transport_stream_loop_length as usize);
        let mut transport_streams = Vec::new();
        while transport_bytes.has_remaining() {
            transport_streams.push(TransportStreamInformation::read(&mut transport_bytes)?);
        }

        let crc_32 = bytes.get_u32();

        Ok(Self {
            section_syntax_indicator,
            section_length,
            bouquet_id,
            version_number,
            current_next_indicator,
            section_number,
            last_section_number,
            descriptors,
            transport_streams,
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
        let descriptors = read_descriptors(bytes, descriptors_loop_length as usize)?;

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

/// SDT (Service Description Table).
#[derive(Clone, Debug)]
pub struct Sdt {
    pub section_syntax_indicator: bool,
    pub section_length: u16,
    pub transport_stream_id: u16,
    pub version_number: u8,
    pub current_next_indicator: bool,
    pub section_number: u8,
    pub last_section_number: u8,
    pub original_network_id: u16,
    pub services: Vec<ServiceInformation>,
    pub crc_32: u32,
}

impl Sdt {
    pub fn read(bytes: &mut Bytes) -> Result<Self> {
        let (section_syntax_indicator, section_length) = read_section_header(bytes);
        let transport_stream_id = bytes.get_u16();
        let (version_number, current_next_indicator) = read_version(bytes);
        let section_number = bytes.get_u8();
        let last_section_number = bytes.get_u8();
        let original_network_id = bytes.get_u16();
        _ = bytes.get_u8();

        let mut services = Vec::new();
        while bytes.remaining() > 4 {
            services.push(ServiceInformation::read(bytes)?);
        }

        let crc_32 = bytes.get_u32();

        Ok(Self {
            section_syntax_indicator,
            section_length,
            transport_stream_id,
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
pub struct EventInformation {
    pub event_id: u16,
    pub start_time: Option<NaiveDateTime>,
    pub duration: Option<Duration>,
    pub running_status: u8,
    pub free_ca_mode: bool,
    pub descriptors: Vec<Descriptor>,
}

impl EventInformation {
    pub fn read(bytes: &mut Bytes) -> Result<Self> {
        let event_id = bytes.get_u16();
        let start_time = parse_jst_time(bytes.get_byte_array::<5>());
        let duration = parse_duration(bytes.get_byte_array::<3>());

        let head = bytes.get_u16();
        let running_status = ((head & 0xE000) >> 13) as u8;
        let free_ca_mode = ((head & 0x1000) >> 12) == 1;
        let descriptors_loop_length = head & 0x0FFF;
        let descriptors = read_descriptors(bytes, descriptors_loop_length as usize)?;

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

/// EIT (Event Information Table).
#[derive(Clone, Debug)]
pub struct Eit {
    pub section_syntax_indicator: bool,
    pub section_length: u16,
    pub service_id: u16,
    pub version_number: u8,
    pub current_next_indicator: bool,
    pub section_number: u8,
    pub last_section_number: u8,
    pub transport_stream_id: u16,
    pub original_network_id: u16,
    pub segment_last_section_number: u8,
    pub last_table_id: u8,
    pub events: Vec<EventInformation>,
    pub crc_32: u32,
}

impl Eit {
    pub fn read(bytes: &mut Bytes) -> Result<Self> {
        let (section_syntax_indicator, section_length) = read_section_header(bytes);
        let service_id = bytes.get_u16();
        let (version_number, current_next_indicator) = read_version(bytes);
        let section_number = bytes.get_u8();
        let last_section_number = bytes.get_u8();
        let transport_stream_id = bytes.get_u16();
        let original_network_id = bytes.get_u16();
        let segment_last_section_number = bytes.get_u8();
        let last_table_id = bytes.get_u8();

        let mut events = Vec::new();
        while bytes.remaining() > 4 {
            events.push(EventInformation::read(bytes)?);
        }

        let crc_32 = bytes.get_u32();

        Ok(Self {
            section_syntax_indicator,
            section_length,
            service_id,
            version_number,
            current_next_indicator,
            section_number,
            last_section_number,
            transport_stream_id,
            original_network_id,
            segment_last_section_number,
            last_table_id,
            events,
            crc_32,
        })
    }
}

/// TDT (Time and Date Table).
#[derive(Clone, Debug)]
pub struct Tdt {
    pub section_syntax_indicator: bool,
    pub section_length: u16,
    pub jst_time: Option<NaiveDateTime>,
}

impl Tdt {
    pub fn read(bytes: &mut Bytes) -> Result<Self> {
        let (section_syntax_indicator, section_length) = read_section_header(bytes);
        let jst_time = parse_jst_time(bytes.get_byte_array::<5>());

        Ok(Self {
            section_syntax_indicator,
            section_length,
            jst_time,
        })
    }
}

/// TOT (Time Offset Table).
#[derive(Clone, Debug)]
pub struct Tot {
    pub section_syntax_indicator: bool,
    pub section_length: u16,
    pub jst_time: Option<NaiveDateTime>,
    pub descriptors: Vec<Descriptor>,
    pub crc_32: u32,
}

impl Tot {
    pub fn read(bytes: &mut Bytes) -> Result<Self> {
        let (section_syntax_indicator, section_length) = read_section_header(bytes);
        let jst_time = parse_jst_time(bytes.get_byte_array::<5>());
        let descriptors_loop_length = bytes.get_u16() & 0x0FFF;
        let descriptors = read_descriptors(bytes, descriptors_loop_length as usize)?;
        let crc_32 = bytes.get_u32();

        Ok(Self {
            section_syntax_indicator,
            section_length,
            jst_time,
            descriptors,
            crc_32,
        })
    }
}

#[derive(Clone, Debug)]
pub struct RunningStatusInformation {
    pub transport_stream_id: u16,
    pub original_network_id: u16,
    pub service_id: u16,
    pub event_id: u16,
    pub running_status: u8,
}

impl RunningStatusInformation {
    pub fn read(bytes: &mut Bytes) -> Result<Self> {
        let transport_stream_id = bytes.get_u16();
        let original_network_id = bytes.get_u16();
        let service_id = bytes.get_u16();
        let event_id = bytes.get_u16();
        let running_status = bytes.get_u8() & 0b0000_0111;

        Ok(Self {
            transport_stream_id,
            original_network_id,
            service_id,
            event_id,
            running_status,
        })
    }
}

/// RST (Running Status Table).
#[derive(Clone, Debug)]
pub struct Rst {
    pub section_syntax_indicator: bool,
    pub section_length: u16,
    pub statuses: Vec<RunningStatusInformation>,
}

impl Rst {
    pub fn read(bytes: &mut Bytes) -> Result<Self> {
        let (section_syntax_indicator, section_length) = read_section_header(bytes);
        let mut statuses = Vec::new();
        while bytes.has_remaining() {
            statuses.push(RunningStatusInformation::read(bytes)?);
        }

        Ok(Self {
            section_syntax_indicator,
            section_length,
            statuses,
        })
    }
}

/// ST (Stuffing Table).
#[derive(Clone, Debug)]
pub struct St {
    pub section_syntax_indicator: bool,
    pub section_length: u16,
    pub data: Vec<u8>,
}

impl St {
    pub fn read(bytes: &mut Bytes) -> Result<Self> {
        let (section_syntax_indicator, section_length) = read_section_header(bytes);
        let data = bytes.to_vec();

        Ok(Self {
            section_syntax_indicator,
            section_length,
            data,
        })
    }
}

#[derive(Clone, Debug)]
pub struct ContentSchedule {
    pub start_time: Option<NaiveDateTime>,
    pub duration: Option<Duration>,
}

impl ContentSchedule {
    pub fn read(bytes: &mut Bytes) -> Result<Self> {
        let start_time = parse_jst_time(bytes.get_byte_array::<5>());
        let duration = parse_duration(bytes.get_byte_array::<3>());

        Ok(Self {
            start_time,
            duration,
        })
    }
}

#[derive(Clone, Debug)]
pub struct ContentVersionInformation {
    pub content_version: u16,
    pub content_minor_version: u16,
    pub version_indicator: u8,
    pub schedules: Vec<ContentSchedule>,
    pub descriptors: Vec<Descriptor>,
}

impl ContentVersionInformation {
    pub fn read(bytes: &mut Bytes) -> Result<Self> {
        let content_version = bytes.get_u16();
        let content_minor_version = bytes.get_u16();
        let head = bytes.get_u16();
        let version_indicator = ((head & 0xC000) >> 14) as u8;
        let content_descriptor_length = head & 0x0FFF;

        let mut content_bytes = bytes.split_to(content_descriptor_length as usize);
        let schedule_description_length = content_bytes.get_u16() & 0x0FFF;

        let mut schedule_bytes = content_bytes.split_to(schedule_description_length as usize);
        let mut schedules = Vec::new();
        while schedule_bytes.has_remaining() {
            schedules.push(ContentSchedule::read(&mut schedule_bytes)?);
        }

        let mut descriptors = Vec::new();
        while content_bytes.has_remaining() {
            descriptors.push(Descriptor::read(&mut content_bytes)?);
        }

        Ok(Self {
            content_version,
            content_minor_version,
            version_indicator,
            schedules,
            descriptors,
        })
    }
}

/// PCAT (Partial Content Announcement Table).
#[derive(Clone, Debug)]
pub struct Pcat {
    pub section_syntax_indicator: bool,
    pub section_length: u16,
    pub service_id: u16,
    pub version_number: u8,
    pub current_next_indicator: bool,
    pub section_number: u8,
    pub last_section_number: u8,
    pub transport_stream_id: u16,
    pub original_network_id: u16,
    pub content_id: u32,
    pub contents: Vec<ContentVersionInformation>,
    pub crc_32: u32,
}

impl Pcat {
    pub fn read(bytes: &mut Bytes) -> Result<Self> {
        let (section_syntax_indicator, section_length) = read_section_header(bytes);
        let service_id = bytes.get_u16();
        let (version_number, current_next_indicator) = read_version(bytes);
        let section_number = bytes.get_u8();
        let last_section_number = bytes.get_u8();
        let transport_stream_id = bytes.get_u16();
        let original_network_id = bytes.get_u16();
        let content_id = bytes.get_u32();

        let num_of_content_version = bytes.get_u8();
        let mut contents = Vec::with_capacity(num_of_content_version as usize);
        for _ in 0..num_of_content_version {
            contents.push(ContentVersionInformation::read(bytes)?);
        }

        let crc_32 = bytes.get_u32();

        Ok(Self {
            section_syntax_indicator,
            section_length,
            service_id,
            version_number,
            current_next_indicator,
            section_number,
            last_section_number,
            transport_stream_id,
            original_network_id,
            content_id,
            contents,
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
        let broadcaster_descriptors_length = bytes.get_u16() & 0x0FFF;
        let descriptors = read_descriptors(bytes, broadcaster_descriptors_length as usize)?;

        Ok(Self {
            broadcaster_id,
            descriptors,
        })
    }
}

/// BIT (Broadcaster Information Table).
#[derive(Clone, Debug)]
pub struct Bit {
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

impl Bit {
    pub fn read(bytes: &mut Bytes) -> Result<Self> {
        let (section_syntax_indicator, section_length) = read_section_header(bytes);
        let original_network_id = bytes.get_u16();
        let (version_number, current_next_indicator) = read_version(bytes);
        let section_number = bytes.get_u8();
        let last_section_number = bytes.get_u8();

        let head = bytes.get_u16();
        let broadcast_view_propriety = ((head & 0x1000) >> 12) == 1;
        let first_descriptors_length = head & 0x0FFF;
        let descriptors = read_descriptors(bytes, first_descriptors_length as usize)?;

        let mut broadcasters = Vec::new();
        while bytes.remaining() > 4 {
            broadcasters.push(BroadcasterInformation::read(bytes)?);
        }

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
pub struct NetworkBoardInformation {
    pub information_id: u16,
    pub information_type: u8,
    pub description_body_location: u8,
    pub user_defined: u8,
    pub key_ids: Vec<u16>,
    pub descriptors: Vec<Descriptor>,
}

impl NetworkBoardInformation {
    pub fn read(bytes: &mut Bytes) -> Result<Self> {
        let information_id = bytes.get_u16();
        let head = bytes.get_u8();
        let information_type = (head & 0xF0) >> 4;
        let description_body_location = (head & 0x0C) >> 2;
        let user_defined = bytes.get_u8();

        let number_of_keys = bytes.get_u8();
        let mut key_ids = Vec::with_capacity(number_of_keys as usize);
        for _ in 0..number_of_keys {
            key_ids.push(bytes.get_u16());
        }

        let descriptors_loop_length = bytes.get_u16() & 0x0FFF;
        let descriptors = read_descriptors(bytes, descriptors_loop_length as usize)?;

        Ok(Self {
            information_id,
            information_type,
            description_body_location,
            user_defined,
            key_ids,
            descriptors,
        })
    }
}

/// NBIT (Network Board Information Table).
#[derive(Clone, Debug)]
pub struct Nbit {
    pub section_syntax_indicator: bool,
    pub section_length: u16,
    pub original_network_id: u16,
    pub version_number: u8,
    pub current_next_indicator: bool,
    pub section_number: u8,
    pub last_section_number: u8,
    pub information: Vec<NetworkBoardInformation>,
    pub crc_32: u32,
}

impl Nbit {
    pub fn read(bytes: &mut Bytes) -> Result<Self> {
        let (section_syntax_indicator, section_length) = read_section_header(bytes);
        let original_network_id = bytes.get_u16();
        let (version_number, current_next_indicator) = read_version(bytes);
        let section_number = bytes.get_u8();
        let last_section_number = bytes.get_u8();

        let mut information = Vec::new();
        while bytes.remaining() > 4 {
            information.push(NetworkBoardInformation::read(bytes)?);
        }

        let crc_32 = bytes.get_u32();

        Ok(Self {
            section_syntax_indicator,
            section_length,
            original_network_id,
            version_number,
            current_next_indicator,
            section_number,
            last_section_number,
            information,
            crc_32,
        })
    }
}

#[derive(Clone, Debug)]
pub struct LinkedDescription {
    pub description_id: u16,
    pub descriptors: Vec<Descriptor>,
}

impl LinkedDescription {
    pub fn read(bytes: &mut Bytes) -> Result<Self> {
        let description_id = bytes.get_u16();
        _ = bytes.get_u8();
        let descriptors_loop_length = bytes.get_u16() & 0x0FFF;
        let descriptors = read_descriptors(bytes, descriptors_loop_length as usize)?;

        Ok(Self {
            description_id,
            descriptors,
        })
    }
}

/// LDT (Linked Description Table).
#[derive(Clone, Debug)]
pub struct Ldt {
    pub section_syntax_indicator: bool,
    pub section_length: u16,
    pub original_service_id: u16,
    pub version_number: u8,
    pub current_next_indicator: bool,
    pub section_number: u8,
    pub last_section_number: u8,
    pub transport_stream_id: u16,
    pub original_network_id: u16,
    pub descriptions: Vec<LinkedDescription>,
    pub crc_32: u32,
}

impl Ldt {
    pub fn read(bytes: &mut Bytes) -> Result<Self> {
        let (section_syntax_indicator, section_length) = read_section_header(bytes);
        let original_service_id = bytes.get_u16();
        let (version_number, current_next_indicator) = read_version(bytes);
        let section_number = bytes.get_u8();
        let last_section_number = bytes.get_u8();
        let transport_stream_id = bytes.get_u16();
        let original_network_id = bytes.get_u16();

        let mut descriptions = Vec::new();
        while bytes.remaining() > 4 {
            descriptions.push(LinkedDescription::read(bytes)?);
        }

        let crc_32 = bytes.get_u32();

        Ok(Self {
            section_syntax_indicator,
            section_length,
            original_service_id,
            version_number,
            current_next_indicator,
            section_number,
            last_section_number,
            transport_stream_id,
            original_network_id,
            descriptions,
            crc_32,
        })
    }
}

#[derive(Clone, Debug)]
pub struct LocalEventInformation {
    pub local_event_id: u16,
    pub descriptors: Vec<Descriptor>,
}

impl LocalEventInformation {
    pub fn read(bytes: &mut Bytes) -> Result<Self> {
        let local_event_id = bytes.get_u16();
        let descriptors_loop_length = bytes.get_u16() & 0x0FFF;
        let descriptors = read_descriptors(bytes, descriptors_loop_length as usize)?;

        Ok(Self {
            local_event_id,
            descriptors,
        })
    }
}

/// LIT (Local Event Information Table).
#[derive(Clone, Debug)]
pub struct Lit {
    pub section_syntax_indicator: bool,
    pub section_length: u16,
    pub event_id: u16,
    pub version_number: u8,
    pub current_next_indicator: bool,
    pub section_number: u8,
    pub last_section_number: u8,
    pub service_id: u16,
    pub transport_stream_id: u16,
    pub original_network_id: u16,
    pub local_events: Vec<LocalEventInformation>,
    pub crc_32: u32,
}

impl Lit {
    pub fn read(bytes: &mut Bytes) -> Result<Self> {
        let (section_syntax_indicator, section_length) = read_section_header(bytes);
        let event_id = bytes.get_u16();
        let (version_number, current_next_indicator) = read_version(bytes);
        let section_number = bytes.get_u8();
        let last_section_number = bytes.get_u8();
        let service_id = bytes.get_u16();
        let transport_stream_id = bytes.get_u16();
        let original_network_id = bytes.get_u16();

        let mut local_events = Vec::new();
        while bytes.remaining() > 4 {
            local_events.push(LocalEventInformation::read(bytes)?);
        }

        let crc_32 = bytes.get_u32();

        Ok(Self {
            section_syntax_indicator,
            section_length,
            event_id,
            version_number,
            current_next_indicator,
            section_number,
            last_section_number,
            service_id,
            transport_stream_id,
            original_network_id,
            local_events,
            crc_32,
        })
    }
}

#[derive(Clone, Debug)]
pub struct EventRelationNode {
    pub node_id: u16,
    pub collection_mode: u8,
    pub parent_node_id: u16,
    pub reference_number: u8,
    pub descriptors: Vec<Descriptor>,
}

impl EventRelationNode {
    pub fn read(bytes: &mut Bytes) -> Result<Self> {
        let node_id = bytes.get_u16();
        let collection_mode = (bytes.get_u8() & 0xF0) >> 4;
        let parent_node_id = bytes.get_u16();
        let reference_number = bytes.get_u8();
        let descriptors_loop_length = bytes.get_u16() & 0x0FFF;
        let descriptors = read_descriptors(bytes, descriptors_loop_length as usize)?;

        Ok(Self {
            node_id,
            collection_mode,
            parent_node_id,
            reference_number,
            descriptors,
        })
    }
}

/// ERT (Event Relation Table).
#[derive(Clone, Debug)]
pub struct Ert {
    pub section_syntax_indicator: bool,
    pub section_length: u16,
    pub event_relation_id: u16,
    pub version_number: u8,
    pub current_next_indicator: bool,
    pub section_number: u8,
    pub last_section_number: u8,
    pub information_provider_id: u16,
    pub relation_type: u8,
    pub nodes: Vec<EventRelationNode>,
    pub crc_32: u32,
}

impl Ert {
    pub fn read(bytes: &mut Bytes) -> Result<Self> {
        let (section_syntax_indicator, section_length) = read_section_header(bytes);
        let event_relation_id = bytes.get_u16();
        let (version_number, current_next_indicator) = read_version(bytes);
        let section_number = bytes.get_u8();
        let last_section_number = bytes.get_u8();
        let information_provider_id = bytes.get_u16();
        let relation_type = (bytes.get_u8() & 0xF0) >> 4;

        let mut nodes = Vec::new();
        while bytes.remaining() > 4 {
            nodes.push(EventRelationNode::read(bytes)?);
        }

        let crc_32 = bytes.get_u32();

        Ok(Self {
            section_syntax_indicator,
            section_length,
            event_relation_id,
            version_number,
            current_next_indicator,
            section_number,
            last_section_number,
            information_provider_id,
            relation_type,
            nodes,
            crc_32,
        })
    }
}

/// ITT (Index Transmission Information Table).
#[derive(Clone, Debug)]
pub struct Itt {
    pub section_syntax_indicator: bool,
    pub section_length: u16,
    pub event_id: u16,
    pub version_number: u8,
    pub current_next_indicator: bool,
    pub section_number: u8,
    pub last_section_number: u8,
    pub descriptors: Vec<Descriptor>,
    pub crc_32: u32,
}

impl Itt {
    pub fn read(bytes: &mut Bytes) -> Result<Self> {
        let (section_syntax_indicator, section_length) = read_section_header(bytes);
        let event_id = bytes.get_u16();
        let (version_number, current_next_indicator) = read_version(bytes);
        let section_number = bytes.get_u8();
        let last_section_number = bytes.get_u8();
        let descriptors_loop_length = bytes.get_u16() & 0x0FFF;
        let descriptors = read_descriptors(bytes, descriptors_loop_length as usize)?;
        let crc_32 = bytes.get_u32();

        Ok(Self {
            section_syntax_indicator,
            section_length,
            event_id,
            version_number,
            current_next_indicator,
            section_number,
            last_section_number,
            descriptors,
            crc_32,
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum IpAddressInformation {
    Ipv4 {
        src_addr: Ipv4Addr,
        src_mask: u8,
        dst_addr: Ipv4Addr,
        dst_mask: u8,
    },
    Ipv6 {
        src_addr: Ipv6Addr,
        src_mask: u8,
        dst_addr: Ipv6Addr,
        dst_mask: u8,
    },
}

impl IpAddressInformation {
    pub fn read(bytes: &mut Bytes, ip_version: bool) -> Result<Self> {
        Ok(if ip_version {
            let src_addr = bytes.get_ipv6_addr();
            let src_mask = bytes.get_u8();
            let dst_addr = bytes.get_ipv6_addr();
            let dst_mask = bytes.get_u8();

            Self::Ipv6 {
                src_addr,
                src_mask,
                dst_addr,
                dst_mask,
            }
        } else {
            let src_addr = bytes.get_ipv4_addr();
            let src_mask = bytes.get_u8();
            let dst_addr = bytes.get_ipv4_addr();
            let dst_mask = bytes.get_u8();

            Self::Ipv4 {
                src_addr,
                src_mask,
                dst_addr,
                dst_mask,
            }
        })
    }
}

#[derive(Clone, Debug)]
pub struct AddressMapService {
    pub service_id: u16,
    pub address: IpAddressInformation,
    pub private_data: Vec<u8>,
}

impl AddressMapService {
    pub fn read(bytes: &mut Bytes) -> Result<Self> {
        let service_id = bytes.get_u16();
        let head = bytes.get_u16();
        let ip_version = ((head & 0x8000) >> 15) == 1;
        let service_loop_length = head & 0x03FF;

        let mut service_bytes = bytes.split_to(service_loop_length as usize);
        let address = IpAddressInformation::read(&mut service_bytes, ip_version)?;
        let private_data = service_bytes.to_vec();

        Ok(Self {
            service_id,
            address,
            private_data,
        })
    }
}

/// AMT (Address Map Table).
#[derive(Clone, Debug)]
pub struct Amt {
    pub section_syntax_indicator: bool,
    pub section_length: u16,
    pub table_id_extension: u16,
    pub version_number: u8,
    pub current_next_indicator: bool,
    pub section_number: u8,
    pub last_section_number: u8,
    pub services: Vec<AddressMapService>,
    pub crc_32: u32,
}

impl Amt {
    pub fn read(bytes: &mut Bytes) -> Result<Self> {
        let (section_syntax_indicator, section_length) = read_section_header(bytes);
        let table_id_extension = bytes.get_u16();
        let (version_number, current_next_indicator) = read_version(bytes);
        let section_number = bytes.get_u8();
        let last_section_number = bytes.get_u8();

        let head = bytes.get_u16();
        let num_of_service_id = (head & 0xFFC0) >> 6;
        let mut services = Vec::with_capacity(num_of_service_id as usize);
        for _ in 0..num_of_service_id {
            services.push(AddressMapService::read(bytes)?);
        }

        let crc_32 = bytes.get_u32();

        Ok(Self {
            section_syntax_indicator,
            section_length,
            table_id_extension,
            version_number,
            current_next_indicator,
            section_number,
            last_section_number,
            services,
            crc_32,
        })
    }
}

#[derive(Clone, Debug)]
pub struct IpMacPlatformInformation {
    pub target_descriptors: Vec<Descriptor>,
    pub operational_descriptors: Vec<Descriptor>,
}

impl IpMacPlatformInformation {
    pub fn read(bytes: &mut Bytes) -> Result<Self> {
        let target_descriptor_loop_length = bytes.get_u16() & 0x0FFF;
        let target_descriptors = read_descriptors(bytes, target_descriptor_loop_length as usize)?;

        let operational_descriptor_loop_length = bytes.get_u16() & 0x0FFF;
        let operational_descriptors =
            read_descriptors(bytes, operational_descriptor_loop_length as usize)?;

        Ok(Self {
            target_descriptors,
            operational_descriptors,
        })
    }
}

/// INT (IP/MAC Notification Table).
#[derive(Clone, Debug)]
pub struct Int {
    pub section_syntax_indicator: bool,
    pub section_length: u16,
    pub action_type: u8,
    pub platform_id_hash: u8,
    pub version_number: u8,
    pub current_next_indicator: bool,
    pub section_number: u8,
    pub last_section_number: u8,
    pub platform_id: u32,
    pub processing_order: u8,
    pub platform_descriptors: Vec<Descriptor>,
    pub platforms: Vec<IpMacPlatformInformation>,
    pub crc_32: u32,
}

impl Int {
    pub fn read(bytes: &mut Bytes) -> Result<Self> {
        let (section_syntax_indicator, section_length) = read_section_header(bytes);
        let action_type = bytes.get_u8();
        let platform_id_hash = bytes.get_u8();
        let (version_number, current_next_indicator) = read_version(bytes);
        let section_number = bytes.get_u8();
        let last_section_number = bytes.get_u8();
        let platform_id = bytes.get_uint(3) as u32;
        let processing_order = bytes.get_u8();
        let platform_descriptor_loop_length = bytes.get_u16() & 0x0FFF;
        let platform_descriptors =
            read_descriptors(bytes, platform_descriptor_loop_length as usize)?;

        let mut platforms = Vec::new();
        while bytes.remaining() > 4 {
            platforms.push(IpMacPlatformInformation::read(bytes)?);
        }

        let crc_32 = bytes.get_u32();

        Ok(Self {
            section_syntax_indicator,
            section_length,
            action_type,
            platform_id_hash,
            version_number,
            current_next_indicator,
            section_number,
            last_section_number,
            platform_id,
            processing_order,
            platform_descriptors,
            platforms,
            crc_32,
        })
    }
}

fn parse_jst_time(jst_time: [u8; 5]) -> Option<NaiveDateTime> {
    if jst_time == [0xFF, 0xFF, 0xFF, 0xFF, 0xFF] {
        return None;
    }

    let mjd = u16::from_be_bytes([jst_time[0], jst_time[1]]);
    let date = ModifiedJulianDay::new(mjd as i32).to_date();
    let hour = parse_bcd(jst_time[2]) as u32;
    let minute = parse_bcd(jst_time[3]) as u32;
    let second = parse_bcd(jst_time[4]) as u32;
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
    (bcd >> 4) * 10 + (bcd & 0x0F)
}

const PAT_ID: u8 = 0x00;
const CAT_ID: u8 = 0x01;
const PMT_ID: u8 = 0x02;
const NIT_ACTUAL_ID: u8 = 0x40;
const NIT_OTHER_ID: u8 = 0x41;
const SDT_ACTUAL_ID: u8 = 0x42;
const SDT_OTHER_ID: u8 = 0x46;
const BAT_ID: u8 = 0x4A;
const INT_ID: u8 = 0x4C;
const EIT_ACTUAL_PRESENT_FOLLOWING_ID: u8 = 0x4E;
const EIT_OTHER_PRESENT_FOLLOWING_ID: u8 = 0x4F;
const EIT_ACTUAL_SCHEDULE_ID_START: u8 = 0x50;
const EIT_ACTUAL_SCHEDULE_ID_END: u8 = 0x5F;
const EIT_OTHER_SCHEDULE_ID_START: u8 = 0x60;
const EIT_OTHER_SCHEDULE_ID_END: u8 = 0x6F;
const TDT_ID: u8 = 0x70;
const RST_ID: u8 = 0x71;
const ST_ID: u8 = 0x72;
const TOT_ID: u8 = 0x73;
const PCAT_ID: u8 = 0xC2;
const BIT_ID: u8 = 0xC4;
const NBIT_BODY_ID: u8 = 0xC5;
const NBIT_REFERENCE_ID: u8 = 0xC6;
const LDT_ID: u8 = 0xC7;
const LIT_ID: u8 = 0xD0;
const ERT_ID: u8 = 0xD1;
const ITT_ID: u8 = 0xD2;
const AMT_ID: u8 = 0xFE;

#[derive(Clone, Debug)]
pub enum Table {
    Pat(Pat),
    Cat(Cat),
    Pmt(Pmt),
    Nit(Nit),
    Bat(Bat),
    Sdt(Sdt),
    Eit(Eit),
    Tdt(Tdt),
    Tot(Tot),
    Rst(Rst),
    St(St),
    Pcat(Pcat),
    Bit(Bit),
    Nbit(Nbit),
    Ldt(Ldt),
    Lit(Lit),
    Ert(Ert),
    Itt(Itt),
    Amt(Amt),
    Int(Int),
    Unknown(u8, Vec<u8>),
}

impl Table {
    pub fn read(bytes: &mut Bytes) -> Result<Self> {
        let table_id = bytes.get_u8();

        Ok(match table_id {
            PAT_ID => Self::Pat(Pat::read(bytes)?),
            CAT_ID => Self::Cat(Cat::read(bytes)?),
            PMT_ID => Self::Pmt(Pmt::read(bytes)?),
            NIT_ACTUAL_ID | NIT_OTHER_ID => Self::Nit(Nit::read(bytes)?),
            BAT_ID => Self::Bat(Bat::read(bytes)?),
            SDT_ACTUAL_ID | SDT_OTHER_ID => Self::Sdt(Sdt::read(bytes)?),
            EIT_ACTUAL_PRESENT_FOLLOWING_ID
            | EIT_OTHER_PRESENT_FOLLOWING_ID
            | EIT_ACTUAL_SCHEDULE_ID_START..=EIT_ACTUAL_SCHEDULE_ID_END
            | EIT_OTHER_SCHEDULE_ID_START..=EIT_OTHER_SCHEDULE_ID_END => {
                Self::Eit(Eit::read(bytes)?)
            }
            TDT_ID => Self::Tdt(Tdt::read(bytes)?),
            TOT_ID => Self::Tot(Tot::read(bytes)?),
            RST_ID => Self::Rst(Rst::read(bytes)?),
            ST_ID => Self::St(St::read(bytes)?),
            PCAT_ID => Self::Pcat(Pcat::read(bytes)?),
            BIT_ID => Self::Bit(Bit::read(bytes)?),
            NBIT_BODY_ID | NBIT_REFERENCE_ID => Self::Nbit(Nbit::read(bytes)?),
            LDT_ID => Self::Ldt(Ldt::read(bytes)?),
            LIT_ID => Self::Lit(Lit::read(bytes)?),
            ERT_ID => Self::Ert(Ert::read(bytes)?),
            ITT_ID => Self::Itt(Itt::read(bytes)?),
            AMT_ID => Self::Amt(Amt::read(bytes)?),
            INT_ID => Self::Int(Int::read(bytes)?),
            _ => Self::Unknown(table_id, bytes.to_vec()),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;

    #[test]
    fn test_parse_jst_time() {
        assert_eq!(
            parse_jst_time([0xC0, 0x79, 0x12, 0x45, 0x00]),
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
