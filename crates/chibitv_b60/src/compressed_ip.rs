use std::io::Result;
use std::net::Ipv6Addr;

use bytes::{Buf, Bytes};
use strum::FromRepr;

use crate::read_ext::BytesExt;

#[derive(Copy, Clone, Debug, Eq, FromRepr, PartialEq)]
#[repr(u8)]
pub enum HcfbHeaderType {
    PartialIpv4UdpHeader = 0x20,
    Ipv4HeaderIdentifier = 0x21,
    PartialIpv6UdpHeader = 0x60,
    NoCompressedHeader = 0x61,
}

#[derive(Clone, Debug)]
pub struct PartialIpv6UdpHeader {
    pub traffic_class: u8,
    pub flow_label: u32,
    pub next_header: u8,
    pub hop_limit: u8,
    pub source_address: Ipv6Addr,
    pub destination_address: Ipv6Addr,
    pub source_port: u16,
    pub destination_port: u16,
}

impl PartialIpv6UdpHeader {
    pub fn read(bytes: &mut Bytes) -> Result<Self> {
        // IPv6 (without payload length)
        let head = bytes.get_u32();
        let version = ((head & 0xF000_0000) >> 28) as u8;
        let traffic_class = ((head & 0x0FF0_0000) >> 20) as u8;
        let flow_label = head & 0x000F_FFFF;
        assert_eq!(version, 6);

        let next_header = bytes.get_u8();
        let hop_limit = bytes.get_u8();
        let source_address = bytes.get_ipv6_addr();
        let destination_address = bytes.get_ipv6_addr();

        // UDP (without payload length and checksum)
        let source_port = bytes.get_u16();
        let destination_port = bytes.get_u16();

        Ok(Self {
            traffic_class,
            flow_label,
            next_header,
            hop_limit,
            source_address,
            destination_address,
            source_port,
            destination_port,
        })
    }
}

#[derive(Clone, Debug)]
pub enum HcfbHeader {
    // TODO
    PartialIpv6UdpHeader(PartialIpv6UdpHeader),
    NoCompressedHeader,
}

#[derive(Clone, Debug)]
pub struct HcfbPacket {
    pub context_id: u16,
    pub sequence_number: u8,
    pub header: HcfbHeader,
}

impl HcfbPacket {
    pub fn read(bytes: &mut Bytes) -> Result<Self> {
        let head = bytes.get_u16();
        let context_id = (head & 0xFFF0) >> 4;
        let sequence_number = (head & 0x000F) as u8;
        let header_type = HcfbHeaderType::from_repr(bytes.get_u8()).unwrap();

        let header = match header_type {
            HcfbHeaderType::PartialIpv6UdpHeader => {
                HcfbHeader::PartialIpv6UdpHeader(PartialIpv6UdpHeader::read(bytes)?)
            }
            HcfbHeaderType::NoCompressedHeader => HcfbHeader::NoCompressedHeader,
            _ => unimplemented!("Sorry, not implemented yet!"),
        };

        Ok(Self {
            context_id,
            sequence_number,
            header,
        })
    }
}
