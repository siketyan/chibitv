#![allow(unused)]

use bytes::{Buf, BufMut, Bytes, BytesMut};

const PICTURE_START_CODE: u8 = 0x00;
const FIRST_SLICE_START_CODE: u8 = 0x01;
const LAST_SLICE_START_CODE: u8 = 0xAF;
const SEQUENCE_HEADER_CODE: u8 = 0xB3;
const EXTENSION_START_CODE: u8 = 0xB5;
const GROUP_START_CODE: u8 = 0xB8;

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum PictureCodingType {
    Intra,
    Predictive,
    BidirectionallyPredictive,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SequenceHeader {
    pub width: u16,
    pub height: u16,
    pub frame_rate_numerator: u32,
    pub frame_rate_denominator: u32,
    pub bit_rate: u32,
    pub vbv_buffer_size: u32,
    pub profile_and_level: Option<u8>,
    pub decoder_config: Bytes,
}

impl SequenceHeader {
    pub fn parse(data: &[u8]) -> anyhow::Result<Self> {
        let sequence_offset = start_codes(data)
            .find(|(_, code)| *code == SEQUENCE_HEADER_CODE)
            .map(|(offset, _)| offset)
            .ok_or_else(|| anyhow::anyhow!("MPEG-2 sequence header is missing"))?;
        let payload = data
            .get(sequence_offset + 4..)
            .ok_or_else(|| anyhow::anyhow!("MPEG-2 sequence header is truncated"))?;
        if payload.len() < 8 {
            anyhow::bail!("MPEG-2 sequence header is truncated");
        }

        let mut width = (u16::from(payload[0]) << 4) | u16::from(payload[1] >> 4);
        let mut height = (u16::from(payload[1] & 0x0F) << 8) | u16::from(payload[2]);
        let frame_rate_code = payload[3] & 0x0F;
        let (mut frame_rate_numerator, mut frame_rate_denominator) = frame_rate(frame_rate_code)
            .ok_or_else(|| anyhow::anyhow!("Invalid MPEG-2 frame rate code: {frame_rate_code}"))?;
        let mut bit_rate_value = (u32::from(payload[4]) << 10)
            | (u32::from(payload[5]) << 2)
            | u32::from(payload[6] >> 6);
        if payload[6] & 0x20 == 0 {
            anyhow::bail!("Invalid MPEG-2 sequence header marker bit");
        }
        let mut vbv_buffer_size_value =
            (u32::from(payload[6] & 0x1F) << 5) | u32::from(payload[7] >> 3);

        let picture_offset = start_codes(data)
            .find(|(offset, code)| *offset > sequence_offset && *code == PICTURE_START_CODE)
            .map(|(offset, _)| offset)
            .unwrap_or(data.len());
        let mut profile_and_level = None;

        for (offset, code) in start_codes(&data[sequence_offset + 4..picture_offset]) {
            if code != EXTENSION_START_CODE {
                continue;
            }
            let extension_offset = sequence_offset + 4 + offset;
            let Some(extension) = data.get(extension_offset + 4..extension_offset + 10) else {
                continue;
            };
            if read_bits(extension, 0, 4) != Some(1) {
                continue;
            }

            profile_and_level = read_bits(extension, 4, 8).map(|value| value as u8);
            let horizontal_size_extension = read_bits(extension, 15, 2).unwrap() as u16;
            let vertical_size_extension = read_bits(extension, 17, 2).unwrap() as u16;
            width |= horizontal_size_extension << 12;
            height |= vertical_size_extension << 12;
            bit_rate_value |= read_bits(extension, 19, 12).unwrap() << 18;
            vbv_buffer_size_value |= read_bits(extension, 32, 8).unwrap() << 10;

            let frame_rate_extension_n = read_bits(extension, 41, 2).unwrap();
            let frame_rate_extension_d = read_bits(extension, 43, 5).unwrap();
            frame_rate_numerator *= frame_rate_extension_n + 1;
            frame_rate_denominator *= frame_rate_extension_d + 1;
            break;
        }

        Ok(Self {
            width,
            height,
            frame_rate_numerator,
            frame_rate_denominator,
            bit_rate: bit_rate_value.saturating_mul(400),
            vbv_buffer_size: vbv_buffer_size_value.saturating_mul(2048),
            profile_and_level,
            decoder_config: Bytes::copy_from_slice(&data[sequence_offset..picture_offset]),
        })
    }

    pub fn sample_duration(&self, timescale: u32) -> u32 {
        ((u64::from(timescale) * u64::from(self.frame_rate_denominator)
            + u64::from(self.frame_rate_numerator) / 2)
            / u64::from(self.frame_rate_numerator)) as u32
    }

    pub fn object_type_indication(&self) -> u8 {
        match self.profile_and_level.map(|value| value >> 4 & 0x07) {
            Some(5) => 0x60,        // Simple Profile
            Some(4) | None => 0x61, // Main Profile
            Some(3) => 0x62,        // SNR Scalable Profile
            Some(2) => 0x63,        // Spatially Scalable Profile
            Some(1) => 0x64,        // High Profile
            Some(0) => 0x65,        // 4:2:2 Profile
            _ => 0x61,
        }
    }
}

pub fn picture_coding_type(data: &[u8]) -> Option<PictureCodingType> {
    let offset = start_codes(data)
        .find(|(_, code)| *code == PICTURE_START_CODE)
        .map(|(offset, _)| offset)?;
    let coding_type = data.get(offset + 5)? >> 3 & 0x07;

    match coding_type {
        1 => Some(PictureCodingType::Intra),
        2 => Some(PictureCodingType::Predictive),
        3 => Some(PictureCodingType::BidirectionallyPredictive),
        _ => None,
    }
}

/// Splits an MPEG-2 Video elementary stream into access units.
///
/// Data preceding the first picture (for example a sequence or GOP header) is
/// kept with that picture. A sequence or GOP header between two pictures is
/// similarly kept with the following picture.
#[derive(Clone, Debug, Default)]
pub struct Mp2Parser {
    buf: BytesMut,
}

impl Mp2Parser {
    pub fn is_empty(&self) -> bool {
        self.buf.is_empty()
    }

    /// Appends elementary-stream bytes and returns the oldest complete access
    /// unit, if one is available.
    ///
    /// At most one access unit is returned per call. Pass an empty slice to
    /// drain further complete access units already buffered by a previous call.
    pub fn push(&mut self, buf: &[u8]) -> Option<Bytes> {
        self.buf.put_slice(buf);

        let boundary = find_access_unit_boundary(&self.buf)?;
        Some(self.buf.split_to(boundary).freeze())
    }

    /// Returns the final access unit when the elementary stream ends.
    ///
    /// Bytes that do not contain a picture are not emitted as an access unit.
    pub fn flush(&mut self) -> Option<Bytes> {
        if find_picture_start(&self.buf).is_none() {
            self.buf.clear();
            return None;
        }

        let remaining = self.buf.remaining();
        (remaining > 0).then(|| self.buf.split_to(remaining).freeze())
    }
}

/// Finds the start of the second access unit in `buf`.
fn find_access_unit_boundary(buf: &[u8]) -> Option<usize> {
    let mut picture_found = false;
    let mut slice_found = false;
    let mut following_header = None;

    for (offset, code) in start_codes(buf) {
        match code {
            PICTURE_START_CODE if picture_found => {
                return Some(following_header.unwrap_or(offset));
            }
            PICTURE_START_CODE => {
                picture_found = true;
                slice_found = false;
            }
            FIRST_SLICE_START_CODE..=LAST_SLICE_START_CODE if picture_found => {
                slice_found = true;
            }
            SEQUENCE_HEADER_CODE | GROUP_START_CODE if picture_found && slice_found => {
                // These headers describe the following picture. Do not split
                // until its picture start code arrives, since the input may end
                // partway through the headers.
                following_header.get_or_insert(offset);
            }
            _ => {}
        }
    }

    None
}

fn find_picture_start(buf: &[u8]) -> Option<usize> {
    start_codes(buf)
        .find(|(_, code)| *code == PICTURE_START_CODE)
        .map(|(offset, _)| offset)
}

fn start_codes(buf: &[u8]) -> impl Iterator<Item = (usize, u8)> + '_ {
    buf.windows(4).enumerate().filter_map(|(offset, bytes)| {
        (bytes[..3] == [0x00, 0x00, 0x01]).then_some((offset, bytes[3]))
    })
}

fn frame_rate(code: u8) -> Option<(u32, u32)> {
    match code {
        1 => Some((24_000, 1001)),
        2 => Some((24, 1)),
        3 => Some((25, 1)),
        4 => Some((30_000, 1001)),
        5 => Some((30, 1)),
        6 => Some((50, 1)),
        7 => Some((60_000, 1001)),
        8 => Some((60, 1)),
        _ => None,
    }
}

fn read_bits(data: &[u8], bit_offset: usize, bit_count: usize) -> Option<u32> {
    if bit_count > 32 || bit_offset.checked_add(bit_count)? > data.len() * 8 {
        return None;
    }

    let mut value = 0;
    for bit in bit_offset..bit_offset + bit_count {
        value = value << 1 | u32::from(data[bit / 8] >> (7 - bit % 8) & 1);
    }
    Some(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    const PICTURE: &[u8] = &[0x00, 0x00, 0x01, PICTURE_START_CODE, 0x00, 0x08];
    const SLICE: &[u8] = &[0x00, 0x00, 0x01, FIRST_SLICE_START_CODE, 0xAA];

    fn write_bits(data: &mut [u8], bit_offset: usize, bit_count: usize, value: u32) {
        for index in 0..bit_count {
            let bit = value >> (bit_count - index - 1) & 1;
            data[(bit_offset + index) / 8] |= (bit as u8) << (7 - (bit_offset + index) % 8);
        }
    }

    fn sequence() -> Vec<u8> {
        let mut data = vec![
            0x00,
            0x00,
            0x01,
            SEQUENCE_HEADER_CODE,
            0x78,
            0x04,
            0x38, // 1920 x 1080
            0x34, // 16:9, 30000/1001
            0x09,
            0xC4,
            0x23,
            0x80, // bitrate, marker, VBV
            0x00,
            0x00,
            0x01,
            EXTENSION_START_CODE,
        ];
        let mut extension = [0; 6];
        write_bits(&mut extension, 0, 4, 1);
        write_bits(&mut extension, 4, 8, 0x48); // Main Profile @ High Level
        write_bits(&mut extension, 12, 1, 1);
        write_bits(&mut extension, 13, 2, 1);
        write_bits(&mut extension, 31, 1, 1);
        data.extend_from_slice(&extension);
        data
    }

    #[test]
    fn splits_at_the_next_picture() {
        let mut parser = Mp2Parser::default();
        let mut input = Vec::new();
        input.extend_from_slice(PICTURE);
        input.extend_from_slice(SLICE);
        input.extend_from_slice(PICTURE);

        let frame = parser.push(&input).unwrap();

        assert_eq!(&frame[..], [PICTURE, SLICE].concat());
        assert_eq!(&parser.flush().unwrap()[..], PICTURE);
    }

    #[test]
    fn detects_a_start_code_split_across_pushes() {
        let mut parser = Mp2Parser::default();

        assert!(parser.push(&PICTURE[..3]).is_none());
        assert!(parser.push(&PICTURE[3..]).is_none());
        assert!(parser.push(SLICE).is_none());
        assert!(parser.push(&PICTURE[..2]).is_none());
        assert!(parser.push(&PICTURE[2..]).is_some());
    }

    #[test]
    fn keeps_initial_headers_with_the_first_picture() {
        let mut parser = Mp2Parser::default();
        let sequence_header = [0x00, 0x00, 0x01, SEQUENCE_HEADER_CODE, 0x2D, 0x02];
        let mut input = sequence_header.to_vec();
        input.extend_from_slice(PICTURE);
        input.extend_from_slice(SLICE);
        input.extend_from_slice(PICTURE);

        let frame = parser.push(&input).unwrap();

        assert!(frame.starts_with(&sequence_header));
        assert!(frame.ends_with(SLICE));
    }

    #[test]
    fn keeps_sequence_and_gop_headers_with_the_following_picture() {
        let mut parser = Mp2Parser::default();
        let sequence_header = [0x00, 0x00, 0x01, SEQUENCE_HEADER_CODE, 0x2D];
        let gop_header = [0x00, 0x00, 0x01, GROUP_START_CODE, 0x00];
        let mut input = Vec::new();
        input.extend_from_slice(PICTURE);
        input.extend_from_slice(SLICE);
        input.extend_from_slice(&sequence_header);
        input.extend_from_slice(&gop_header);
        input.extend_from_slice(PICTURE);

        let first = parser.push(&input).unwrap();
        let second = parser.flush().unwrap();

        assert_eq!(&first[..], [PICTURE, SLICE].concat());
        assert!(second.starts_with(&sequence_header));
        assert!(second[sequence_header.len()..].starts_with(&gop_header));
    }

    #[test]
    fn drains_multiple_buffered_access_units() {
        let mut parser = Mp2Parser::default();
        let mut input = Vec::new();
        for _ in 0..3 {
            input.extend_from_slice(PICTURE);
            input.extend_from_slice(SLICE);
        }

        assert!(parser.push(&input).is_some());
        assert!(parser.push(&[]).is_some());
        assert!(parser.push(&[]).is_none());
        assert!(parser.flush().is_some());
    }

    #[test]
    fn does_not_flush_data_without_a_picture() {
        let mut parser = Mp2Parser::default();

        assert!(
            parser
                .push(&[0x00, 0x00, 0x01, SEQUENCE_HEADER_CODE])
                .is_none()
        );
        assert!(parser.flush().is_none());
    }

    #[test]
    fn parses_sequence_and_picture_headers() {
        let mut data = sequence();
        data.extend_from_slice(PICTURE);
        data.extend_from_slice(SLICE);

        let header = SequenceHeader::parse(&data).unwrap();

        assert_eq!(header.width, 1920);
        assert_eq!(header.height, 1080);
        assert_eq!(header.frame_rate_numerator, 30_000);
        assert_eq!(header.frame_rate_denominator, 1001);
        assert_eq!(header.sample_duration(90_000), 3003);
        assert_eq!(header.profile_and_level, Some(0x48));
        assert_eq!(header.object_type_indication(), 0x61);
        assert_eq!(picture_coding_type(&data), Some(PictureCodingType::Intra));
        assert_eq!(
            header.decoder_config,
            &data[..data.len() - PICTURE.len() - SLICE.len()]
        );
    }
}
