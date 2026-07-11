#![allow(unused)]

use bytes::{Buf, BufMut, Bytes, BytesMut};

const PICTURE_START_CODE: u8 = 0x00;
const FIRST_SLICE_START_CODE: u8 = 0x01;
const LAST_SLICE_START_CODE: u8 = 0xAF;
const SEQUENCE_HEADER_CODE: u8 = 0xB3;
const GROUP_START_CODE: u8 = 0xB8;

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

#[cfg(test)]
mod tests {
    use super::*;

    const PICTURE: &[u8] = &[0x00, 0x00, 0x01, PICTURE_START_CODE, 0x00, 0x08];
    const SLICE: &[u8] = &[0x00, 0x00, 0x01, FIRST_SLICE_START_CODE, 0xAA];

    #[test]
    fn splits_at_the_next_picture() {
        let mut parser = Mp2Parser::default();
        let mut input = Vec::new();
        input.extend_from_slice(PICTURE);
        input.extend_from_slice(SLICE);
        input.extend_from_slice(PICTURE);

        let frame = parser.push(&input).unwrap();

        assert_eq!(&frame[..], [&PICTURE[..], &SLICE[..]].concat());
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

        assert_eq!(&first[..], [&PICTURE[..], &SLICE[..]].concat());
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
}
