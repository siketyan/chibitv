//! TLV-SI, the signalling the transmission control packets of a TLV stream
//! carry.
//!
//! It describes the stream itself rather than what is carried in it: which TLV
//! streams the network is made of, and which transponder each one sits on. Its
//! sections are laid out the way MPEG-2 systems lays its own out, and so are
//! its descriptors, which are a namespace of their own rather than the sixteen
//! bit tagged ones MMT signals with.

use std::io::{Error, ErrorKind, Result};

use bytes::{Buf, Bytes};
use strum::FromRepr;

/// The local oscillator of the BS/CS110 converter every Japanese dish carries,
/// in kHz.
const RIGHT_HANDED_OSCILLATOR_KHZ: u32 = 10_678_000;
const LEFT_HANDED_OSCILLATOR_KHZ: u32 = 9_505_000;
const LEFT_HANDED_CIRCULAR_POLARISATION: u8 = 0b10;

/// TLV-NIT (TLV Network Information Table).
///
/// It names every TLV stream of the network, so one of them is enough to learn
/// where the rest of them are.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TlvNit {
    pub section_syntax_indicator: bool,
    pub section_length: u16,
    pub original_network_id: u16,
    pub version_number: u8,
    pub current_next_indicator: bool,
    pub section_number: u8,
    pub last_section_number: u8,
    pub descriptors: Vec<Descriptor>,
    pub tlv_streams: Vec<TlvStreamInformation>,
    pub crc_32: u32,
}

impl TlvNit {
    pub fn read(bytes: &mut Bytes) -> Result<Self> {
        if bytes.remaining() < 12 {
            return Err(Error::new(
                ErrorKind::UnexpectedEof,
                "TLV-NIT must be at least 12 bytes",
            ));
        }

        let head = bytes.get_u16();
        let section_syntax_indicator = ((head & 0x8000) >> 15) == 1;
        let section_length = head & 0x0FFF;
        let original_network_id = bytes.get_u16();

        let head = bytes.get_u8();
        let version_number = (head & 0b0011_1110) >> 1;
        let current_next_indicator = (head & 0b0000_0001) == 1;

        let section_number = bytes.get_u8();
        let last_section_number = bytes.get_u8();

        let network_descriptors_length = bytes.get_u16() & 0x0FFF;
        let descriptors = read_descriptors(bytes, network_descriptors_length as usize)?;

        let tlv_stream_loop_length = bytes.get_u16() & 0x0FFF;
        let mut stream_bytes = split_to(bytes, tlv_stream_loop_length as usize)?;
        let mut tlv_streams = Vec::new();
        while stream_bytes.has_remaining() {
            tlv_streams.push(TlvStreamInformation::read(&mut stream_bytes)?);
        }

        if bytes.remaining() < 4 {
            return Err(Error::new(
                ErrorKind::UnexpectedEof,
                "TLV-NIT ends before its CRC",
            ));
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
            descriptors,
            tlv_streams,
            crc_32,
        })
    }
}

/// One TLV stream of the network, as its NIT describes it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TlvStreamInformation {
    pub tlv_stream_id: u16,
    pub original_network_id: u16,
    pub descriptors: Vec<Descriptor>,
}

impl TlvStreamInformation {
    pub fn read(bytes: &mut Bytes) -> Result<Self> {
        if bytes.remaining() < 6 {
            return Err(Error::new(
                ErrorKind::UnexpectedEof,
                "TLV stream information must be at least 6 bytes",
            ));
        }

        let tlv_stream_id = bytes.get_u16();
        let original_network_id = bytes.get_u16();
        let descriptors_length = bytes.get_u16() & 0x0FFF;
        let descriptors = read_descriptors(bytes, descriptors_length as usize)?;

        Ok(Self {
            tlv_stream_id,
            original_network_id,
            descriptors,
        })
    }
}

#[derive(Clone, Debug, FromRepr)]
#[repr(u8)]
pub enum DescriptorTag {
    ServiceListDescriptor = 0x41,
    SatelliteDeliverySystemDescriptor = 0x43,
    NetworkNameDescriptor = 0x40,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Descriptor {
    NetworkName(NetworkNameDescriptor),
    ServiceList(ServiceListDescriptor),
    SatelliteDeliverySystem(SatelliteDeliverySystemDescriptor),
    Unknown(u8, Vec<u8>),
}

impl Descriptor {
    pub fn read(bytes: &mut Bytes) -> Result<Self> {
        if bytes.remaining() < 2 {
            return Err(Error::new(
                ErrorKind::UnexpectedEof,
                "descriptor must be at least 2 bytes",
            ));
        }

        let descriptor_tag = bytes.get_u8();
        let descriptor_length = bytes.get_u8();
        let mut bytes = split_to(bytes, descriptor_length as usize)?;

        let Some(descriptor_tag) = DescriptorTag::from_repr(descriptor_tag) else {
            return Ok(Self::Unknown(descriptor_tag, bytes.into()));
        };

        Ok(match descriptor_tag {
            DescriptorTag::NetworkNameDescriptor => Self::NetworkName(NetworkNameDescriptor {
                network_name: bytes.to_vec(),
            }),
            DescriptorTag::ServiceListDescriptor => {
                Self::ServiceList(ServiceListDescriptor::read(&mut bytes)?)
            }
            DescriptorTag::SatelliteDeliverySystemDescriptor => {
                Self::SatelliteDeliverySystem(SatelliteDeliverySystemDescriptor::read(&mut bytes)?)
            }
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NetworkNameDescriptor {
    pub network_name: Vec<u8>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ServiceListItem {
    pub service_id: u16,
    pub service_type: u8,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ServiceListDescriptor {
    pub services: Vec<ServiceListItem>,
}

impl ServiceListDescriptor {
    pub fn read(bytes: &mut Bytes) -> Result<Self> {
        let mut services = Vec::new();
        while bytes.remaining() >= 3 {
            services.push(ServiceListItem {
                service_id: bytes.get_u16(),
                service_type: bytes.get_u8(),
            });
        }

        Ok(Self { services })
    }
}

/// The transponder one TLV stream is carried on.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SatelliteDeliverySystemDescriptor {
    /// Downlink frequency in kHz, which is what the satellite radiates rather
    /// than what reaches the tuner: see
    /// [`SatelliteDeliverySystemDescriptor::intermediate_frequency_khz`].
    pub frequency_khz: u32,
    /// Orbital position in tenths of a degree, so that 110.0°E is 1100.
    pub orbital_position: u16,
    /// Whether the satellite sits east of the prime meridian, as every one
    /// Japan broadcasts from does.
    pub west_east_flag: bool,
    pub polarisation: u8,
    pub modulation: u8,
    pub symbol_rate: u32,
    pub fec_inner: u8,
}

impl SatelliteDeliverySystemDescriptor {
    pub fn read(bytes: &mut Bytes) -> Result<Self> {
        if bytes.remaining() < 11 {
            return Err(Error::new(
                ErrorKind::UnexpectedEof,
                "satellite delivery system descriptor must be at least 11 bytes",
            ));
        }

        // The frequency is eight BCD digits of GHz with the point after the
        // second one, which is hundredths of a MHz, so ten kHz each.
        let frequency_khz = read_bcd(bytes.get_u32()) * 10;
        let orbital_position = read_bcd(u32::from(bytes.get_u16())) as u16;

        let flags = bytes.get_u8();
        let west_east_flag = flags & 0x80 != 0;
        let polarisation = (flags >> 5) & 0x03;
        let modulation = flags & 0x1F;

        // Twenty-eight BCD digits of the symbol rate in units of 100 sym/s,
        // followed by the inner FEC in the low nibble.
        let rate_and_fec = bytes.get_u32();
        let symbol_rate = read_bcd(rate_and_fec >> 4);
        let fec_inner = (rate_and_fec & 0x0F) as u8;

        Ok(Self {
            frequency_khz,
            orbital_position,
            west_east_flag,
            polarisation,
            modulation,
            symbol_rate,
            fec_inner,
        })
    }

    /// The frequency the tuner is given, which is what is left of the downlink
    /// once the converter on the dish has shifted it down.
    ///
    /// A descriptor naming a frequency the converter cannot reach has none.
    pub fn intermediate_frequency_khz(&self) -> Option<u32> {
        // The two senses of circular polarisation are shifted down by
        // converters of their own, so which one carries the stream decides
        // where it lands.
        let oscillator = match self.polarisation {
            LEFT_HANDED_CIRCULAR_POLARISATION => LEFT_HANDED_OSCILLATOR_KHZ,
            _ => RIGHT_HANDED_OSCILLATOR_KHZ,
        };

        self.frequency_khz.checked_sub(oscillator)
    }
}

/// A table of the TLV-SI.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Table {
    TlvNit(TlvNit),
    Unknown(u8, Vec<u8>),
}

const TLV_NIT_ID: u8 = 0x40;
/// What a packet with room left over is padded with.
const STUFFING_ID: u8 = 0xFF;

impl Table {
    /// Reads one section, which is as much of the packet as it takes up.
    pub fn read(table_id: u8, bytes: &mut Bytes) -> Result<Self> {
        Ok(match table_id {
            TLV_NIT_ID => Self::TlvNit(TlvNit::read(bytes)?),
            _ => Self::Unknown(table_id, bytes.to_vec()),
        })
    }
}

/// Reads every section a transmission control signal packet carries.
///
/// The packet is one buffer of sections, each saying how long it is, and it is
/// padded once they run out. A section that cannot be read ends the packet
/// rather than the stream: what follows it can only be found by reading it.
pub fn read_sections(mut bytes: Bytes) -> Vec<Table> {
    let mut sections = Vec::new();

    while bytes.remaining() >= 3 {
        let table_id = bytes.get_u8();
        if table_id == STUFFING_ID {
            break;
        }

        // The section length counts what follows the two bytes carrying it,
        // which are read again by the table itself for the syntax indicator.
        let section_length = (u16::from(bytes[0]) << 8 | u16::from(bytes[1])) & 0x0FFF;
        let Ok(mut section) = split_to(&mut bytes, usize::from(section_length) + 2) else {
            break;
        };

        match Table::read(table_id, &mut section) {
            Ok(table) => sections.push(table),
            Err(_) => break,
        }
    }

    sections
}

fn read_descriptors(bytes: &mut Bytes, length: usize) -> Result<Vec<Descriptor>> {
    let mut bytes = split_to(bytes, length)?;
    let mut descriptors = Vec::new();
    while bytes.has_remaining() {
        descriptors.push(Descriptor::read(&mut bytes)?);
    }

    Ok(descriptors)
}

fn split_to(bytes: &mut Bytes, length: usize) -> Result<Bytes> {
    if bytes.remaining() < length {
        return Err(Error::new(
            ErrorKind::UnexpectedEof,
            "field runs past the end of the section",
        ));
    }

    Ok(bytes.split_to(length))
}

/// Reads a field of binary coded decimal digits as the number it spells.
///
/// A nibble that is not a digit is read as nothing rather than refused: the
/// field is one number, and the rest of it is still worth having.
fn read_bcd(value: u32) -> u32 {
    (0..8)
        .rev()
        .fold(0, |number, digit| match (value >> (digit * 4)) & 0x0F {
            nibble if nibble < 10 => number * 10 + nibble,
            _ => number,
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A field of the given bytes behind the twelve bit length that counts
    /// them, with the four bits above it set as the reserved ones are.
    fn with_length(bytes: Vec<u8>) -> Vec<u8> {
        let length = u16::try_from(bytes.len()).unwrap() | 0xF000;

        [length.to_be_bytes().to_vec(), bytes].concat()
    }

    /// A TLV-NIT naming one TLV stream on BS-15, as the section reaches the
    /// demultiplexer.
    fn tlv_nit_section() -> Vec<u8> {
        let stream_descriptors = [
            vec![
                0x43, 0x0B, // satellite_delivery_system_descriptor
                0x01, 0x19, 0x96, 0x00, // frequency, as 011.99600 GHz
                0x11, 0x00, // orbital_position
                0xEB, // west_east_flag, polarisation, modulation
                0x02, 0x88, 0x60, 0x0F, // symbol_rate and FEC_inner
            ],
            vec![
                0x41, 0x03, // service_list_descriptor
                0x40, 0x65, 0x01, // service_id, service_type
            ],
        ]
        .concat();

        let stream_loop = [
            vec![
                0x40, 0xF1, // tlv_stream_id
                0x00, 0x04, // original_network_id
            ],
            with_length(stream_descriptors),
        ]
        .concat();

        let network_descriptors = vec![
            0x40, 0x03, // network_name_descriptor
            b'B', b'S', b'4',
        ];

        let body = [
            vec![
                0x00, 0x04, // original_network_id
                0xC1, // reserved, version_number, current_next_indicator
                0x00, 0x00, // section_number, last_section_number
            ],
            with_length(network_descriptors),
            with_length(stream_loop),
            vec![0xDE, 0xAD, 0xBE, 0xEF], // CRC_32
        ]
        .concat();

        [vec![TLV_NIT_ID], with_length(body)].concat()
    }

    #[test]
    fn reads_the_tlv_streams_of_a_network() {
        let sections = read_sections(Bytes::from(tlv_nit_section()));

        let [Table::TlvNit(nit)] = sections.as_slice() else {
            panic!("the packet was read as {sections:?}");
        };
        assert_eq!(nit.original_network_id, 4);
        assert_eq!(nit.version_number, 0);
        assert!(nit.current_next_indicator);
        assert_eq!(
            nit.descriptors,
            [Descriptor::NetworkName(NetworkNameDescriptor {
                network_name: b"BS4".to_vec(),
            })]
        );

        let [stream] = nit.tlv_streams.as_slice() else {
            panic!("the network named {:?}", nit.tlv_streams);
        };
        assert_eq!(stream.tlv_stream_id, 0x40F1);
        assert_eq!(stream.original_network_id, 4);
        assert_eq!(
            stream.descriptors,
            [
                Descriptor::SatelliteDeliverySystem(SatelliteDeliverySystemDescriptor {
                    frequency_khz: 11_996_000,
                    orbital_position: 1100,
                    west_east_flag: true,
                    polarisation: 0b11,
                    modulation: 0x0B,
                    symbol_rate: 288_600,
                    fec_inner: 0x0F,
                }),
                Descriptor::ServiceList(ServiceListDescriptor {
                    services: vec![ServiceListItem {
                        service_id: 0x4065,
                        service_type: 0x01,
                    }],
                }),
            ]
        );
    }

    #[test]
    fn converts_a_downlink_frequency_to_the_one_the_tuner_takes() {
        let sections = read_sections(Bytes::from(tlv_nit_section()));
        let [Table::TlvNit(nit)] = sections.as_slice() else {
            panic!("the packet was read as {sections:?}");
        };
        let Descriptor::SatelliteDeliverySystem(descriptor) = nit.tlv_streams[0].descriptors[0]
        else {
            panic!("the transponder was read as {:?}", nit.tlv_streams[0]);
        };

        // BS-15, right-handed, which the dish hands to the tuner at 1318 MHz.
        assert_eq!(descriptor.intermediate_frequency_khz(), Some(1_318_000));

        // BS-14, left-handed, where the 8K is: a converter of its own shifts
        // it down to 2471.82 MHz instead.
        assert_eq!(
            SatelliteDeliverySystemDescriptor {
                frequency_khz: 11_976_820,
                polarisation: 0b10,
                ..descriptor
            }
            .intermediate_frequency_khz(),
            Some(2_471_820),
        );
    }

    #[test]
    fn stops_at_the_padding_after_the_last_section() {
        let packet = [tlv_nit_section(), vec![0xFF; 16]].concat();

        assert_eq!(read_sections(Bytes::from(packet)).len(), 1);
    }

    #[test]
    fn reads_a_packet_of_several_sections() {
        let packet = [tlv_nit_section(), tlv_nit_section()].concat();

        assert_eq!(read_sections(Bytes::from(packet)).len(), 2);
    }

    #[test]
    fn gives_up_on_a_section_that_is_cut_short() {
        let mut packet = tlv_nit_section();
        packet.truncate(packet.len() - 8);

        assert!(read_sections(Bytes::from(packet)).is_empty());
    }
}
