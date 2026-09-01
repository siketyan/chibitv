use std::borrow::Cow;
use std::io::Cursor;

use bitstream_io::{BigEndian, BitRead, BitReader};
use bytes::{Buf, BufMut, Bytes, BytesMut};
use shiguredo_mp4::bitstream::aac::SamplingFrequency;
use tracing::warn;

/// The length of an ADTS header without the CRC (`protection_absent == 1`).
const ADTS_HEADER_LENGTH: usize = 7;

/// The length of an ADTS header with the CRC (`protection_absent == 0`).
const ADTS_HEADER_WITH_CRC_LENGTH: usize = 9;

/// Returns the frame length of the ADTS frame starting at `data`.
///
/// Only the fields needed for framing are read: the payload itself is handed to
/// `shiguredo_mp4::bitstream::aac::parse_adts_frame`, which validates the rest.
fn adts_frame_length(data: &[u8]) -> Option<usize> {
    if data.len() < ADTS_HEADER_LENGTH {
        return None;
    }
    // syncword (0xFFF) and layer (0)
    if data[0] != 0xFF || data[1] & 0xF6 != 0xF0 {
        return None;
    }

    let protection_absent = data[1] & 0x01 != 0;
    let header_length = if protection_absent {
        ADTS_HEADER_LENGTH
    } else {
        ADTS_HEADER_WITH_CRC_LENGTH
    };
    let frame_length = (usize::from(data[3] & 0x03) << 11)
        | (usize::from(data[4]) << 3)
        | usize::from(data[5] >> 5);
    (frame_length >= header_length).then_some(frame_length)
}

/// Splits an ADTS byte stream into complete frames.
#[derive(Clone, Debug, Default)]
pub struct AdtsParser {
    buf: BytesMut,
}

impl AdtsParser {
    pub fn is_empty(&self) -> bool {
        self.buf.is_empty()
    }

    /// Appends bytes and returns the oldest complete ADTS frame, including its
    /// header. Pass an empty slice to drain additional buffered frames.
    pub fn push(&mut self, data: &[u8]) -> Option<Bytes> {
        self.buf.put_slice(data);

        loop {
            let Some(offset) = find_adts_sync_word(&self.buf) else {
                // A sync word may be split immediately after its first byte.
                let keep_last_byte = self.buf.last() == Some(&0xFF);
                let remaining = usize::from(keep_last_byte);
                let discard = self.buf.len() - remaining;
                self.buf.advance(discard);
                return None;
            };

            self.buf.advance(offset);
            if self.buf.len() < ADTS_HEADER_LENGTH {
                return None;
            }

            let Some(frame_length) = adts_frame_length(&self.buf) else {
                // A false sync word: resync from the next byte.
                self.buf.advance(1);
                continue;
            };
            if self.buf.len() < frame_length {
                return None;
            }

            return Some(self.buf.split_to(frame_length).freeze());
        }
    }
}

fn find_adts_sync_word(data: &[u8]) -> Option<usize> {
    data.windows(2)
        .position(|bytes| bytes[0] == 0xFF && bytes[1] & 0xF6 == 0xF0)
}

#[derive(Clone, Debug)]
pub struct LoasFrame {
    pub audio_object_type: u8,
    pub sampling_frequency_index: u8,
    pub sampling_frequency: SamplingFrequency,
    pub channel_configuration: u8,
    pub other_data_present: bool,
    pub data: Option<Bytes>,
}

impl LoasFrame {
    pub fn next(cursor: &mut Cursor<&[u8]>, previous: Option<&Self>) -> anyhow::Result<Self> {
        Ok(loop {
            cursor.set_position({
                let data = cursor.get_ref();
                let Some(offset) = find_sync_word(data, usize::try_from(cursor.position())?) else {
                    anyhow::bail!("No LOAS frame found");
                };

                let length =
                    (usize::from(data[offset + 1] & 0x1F) << 8) | usize::from(data[offset + 2]);
                if offset + 3 + length > data.len() {
                    anyhow::bail!("EOF");
                }

                (offset + 3) as u64
            });

            let mut r = BitReader::endian(&mut *cursor, BigEndian);

            let use_same_stream_mux = r.read_bit()?;
            let stream_mux_config = if !use_same_stream_mux {
                let audio_mux_version = r.read_bit()?;
                let audio_mux_version_a = audio_mux_version && r.read_bit()?;
                if audio_mux_version_a {
                    anyhow::bail!("audioMuxVersionA is not supported yet");
                }
                if audio_mux_version {
                    get_latm_value(&mut r)?;
                }

                let all_streams_same_time_framing = r.read_bit()?;
                if !all_streams_same_time_framing {
                    anyhow::bail!("allStreamsSameTimeFraming must not be zero");
                }

                let num_sub_frames = r.read_unsigned::<6, u8>()?;
                if num_sub_frames != 0 {
                    anyhow::bail!("numSubFrames must be 0");
                }

                let num_program = r.read_unsigned::<4, u8>()?;
                if num_program != 0 {
                    anyhow::bail!("numProgram must be 0");
                }

                let num_layer = r.read_unsigned::<3, u8>()?;
                if num_layer != 0 {
                    anyhow::bail!("numLayer must be 0");
                }

                let total_bits = if audio_mux_version {
                    get_latm_value(&mut r)?
                } else {
                    0
                };

                let audio_object_type = r.read_unsigned::<5, u8>()?;
                let sampling_frequency_index = r.read_unsigned::<4, u8>()?;
                let channel_configuration = r.read_unsigned::<4, u8>()?;

                r.skip(3)?; // GASpecificConfig

                let read_bits = 5 + 4 + 4 + 3;
                if total_bits > read_bits {
                    r.skip(total_bits - read_bits)?;
                }

                let frame_length_type = r.read_unsigned::<3, u8>()?;
                if frame_length_type != 0 {
                    anyhow::bail!("frameLengthType must be 0");
                }
                r.skip(8)?;

                let other_data_present = r.read_bit()?;
                if other_data_present {
                    if audio_mux_version {
                        get_latm_value(&mut r)?;
                    } else {
                        loop {
                            let esc = r.read_bit()?;
                            r.skip(8)?;
                            if !esc {
                                break;
                            }
                        }
                    }
                }

                let crc_check_present = r.read_bit()?;
                if crc_check_present {
                    r.skip(8)?;
                }

                Cow::Owned(Self {
                    audio_object_type,
                    sampling_frequency_index,
                    sampling_frequency: SamplingFrequency::from_index(sampling_frequency_index)?,
                    channel_configuration,
                    other_data_present,
                    data: None,
                })
            } else {
                match previous {
                    Some(previous) => Cow::Borrowed(previous),
                    None => {
                        warn!("StreamMuxConfig is missing");
                        continue;
                    }
                }
            };

            let mut length: usize = 0;
            loop {
                let b = r.read_unsigned::<8, u8>()?;
                length += usize::from(b);
                if b != 0xFF {
                    break;
                }
            }

            let mut data = BytesMut::zeroed(length);
            r.read_bytes(&mut data)?;

            break Self {
                audio_object_type: stream_mux_config.audio_object_type,
                sampling_frequency_index: stream_mux_config.sampling_frequency_index,
                sampling_frequency: stream_mux_config.sampling_frequency,
                channel_configuration: stream_mux_config.channel_configuration,
                other_data_present: stream_mux_config.other_data_present,
                data: Some(data.freeze()),
            };
        })
    }
}

fn find_sync_word(data: &[u8], mut offset: usize) -> Option<usize> {
    loop {
        if offset + 1 >= data.len() {
            return None; // EOF
        }

        let sync_word = u16::from(data[offset]) << 3 | u16::from(data[offset + 1]) >> 5;
        if sync_word == 0x2B7 {
            return Some(offset);
        }

        offset += 1;
    }
}

fn get_latm_value(r: &mut impl BitRead) -> anyhow::Result<u32> {
    let bytes_for_value = r.read_unsigned::<2, u8>()?;
    let mut value: u32 = 0;
    for _ in 0..bytes_for_value {
        value <<= 8;
        value |= u32::from(r.read_unsigned::<8, u8>()?);
    }

    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn adts_frame(
        payload: &[u8],
        protection_absent: bool,
        sampling_frequency_index: u8,
        channel_configuration: u8,
        raw_data_blocks: u8,
    ) -> Vec<u8> {
        let header_length = if protection_absent {
            ADTS_HEADER_LENGTH
        } else {
            ADTS_HEADER_WITH_CRC_LENGTH
        };
        let frame_length = header_length + payload.len();
        let mut frame = vec![
            0xFF,
            0xF0 | u8::from(protection_absent),
            0x40 | sampling_frequency_index << 2 | (channel_configuration >> 2 & 0x01),
            (channel_configuration & 0x03) << 6 | ((frame_length >> 11) & 0x03) as u8,
            (frame_length >> 3) as u8,
            ((frame_length & 0x07) << 5) as u8 | 0x1F,
            0xFC | raw_data_blocks,
        ];
        if !protection_absent {
            frame.extend_from_slice(&[0x12, 0x34]);
        }
        frame.extend_from_slice(payload);
        frame
    }

    #[test]
    fn reads_the_frame_length_of_an_adts_header() {
        let frame = adts_frame(&[0xDE, 0xAD, 0xBE, 0xEF], true, 4, 2, 0);

        assert_eq!(adts_frame_length(&frame), Some(frame.len()));
    }

    #[test]
    fn counts_the_crc_in_the_frame_length() {
        let frame = adts_frame(&[0xCA, 0xFE], false, 3, 1, 0);

        assert_eq!(adts_frame_length(&frame), Some(frame.len()));
        assert_eq!(frame.len(), ADTS_HEADER_WITH_CRC_LENGTH + 2);
    }

    #[test]
    fn rejects_a_truncated_or_desynced_adts_header() {
        let frame = adts_frame(&[0xAA], true, 4, 2, 0);

        // ヘッダー長に満たない
        assert_eq!(adts_frame_length(&frame[..6]), None);
        // sync word が合わない
        assert_eq!(
            adts_frame_length(&[0xFE, 0xF1, 0x50, 0x80, 0x01, 0x1F, 0xFC]),
            None
        );
        // layer が 0 でない
        assert_eq!(
            adts_frame_length(&[0xFF, 0xF3, 0x50, 0x80, 0x01, 0x1F, 0xFC]),
            None
        );
        // frame_length がヘッダー長未満
        assert_eq!(
            adts_frame_length(&[0xFF, 0xF1, 0x50, 0x80, 0x00, 0x1F, 0xFC]),
            None
        );
    }

    #[test]
    fn detects_a_frame_split_across_pushes() {
        let frame = adts_frame(&[1, 2, 3, 4], true, 4, 2, 0);
        let mut parser = AdtsParser::default();

        assert!(parser.push(&frame[..1]).is_none());
        assert!(parser.push(&frame[1..6]).is_none());
        assert_eq!(&parser.push(&frame[6..]).unwrap()[..], &frame);
    }

    #[test]
    fn drains_multiple_buffered_frames() {
        let first = adts_frame(&[1, 2], true, 4, 2, 0);
        let second = adts_frame(&[3, 4, 5], true, 4, 2, 0);
        let mut input = first.clone();
        input.extend_from_slice(&second);
        let mut parser = AdtsParser::default();

        assert_eq!(&parser.push(&input).unwrap()[..], &first);
        assert_eq!(&parser.push(&[]).unwrap()[..], &second);
        assert!(parser.push(&[]).is_none());
    }

    #[test]
    fn skips_garbage_and_invalid_headers() {
        let frame = adts_frame(&[0xAA], true, 4, 2, 0);
        let mut input = vec![0x11, 0x22, 0xFF, 0xF1, 0x7C, 0, 0, 0, 0];
        input.extend_from_slice(&frame);
        let mut parser = AdtsParser::default();

        assert_eq!(&parser.push(&input).unwrap()[..], &frame);
    }

    #[test]
    fn frames_a_header_with_multiple_raw_data_blocks() {
        // 複数 raw data block の拒否は shiguredo_mp4 の parse_adts_frame が行う。
        // フレーミングは frame_length だけを見るので通す
        let frame = adts_frame(&[0xAA], true, 4, 2, 1);
        let mut parser = AdtsParser::default();

        assert_eq!(&parser.push(&frame).unwrap()[..], &frame);
    }
}
