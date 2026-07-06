use std::borrow::Cow;
use std::io::Cursor;

use bitstream_io::{BigEndian, BitRead, BitReader};
use bytes::{Bytes, BytesMut};
use tracing::warn;

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
#[repr(u32)]
pub enum SamplingFrequency {
    F96000 = 96_000,
    F88200 = 88_200,
    F64000 = 64_000,
    F48000 = 48_000,
    F44100 = 44_100,
    F32000 = 32_000,
    F24000 = 24_000,
    F22050 = 22_050,
    F16000 = 16_000,
    F12000 = 12_000,
    F11025 = 11_025,
    F8000 = 8_000,
    F7350 = 7350,
}

impl TryFrom<u8> for SamplingFrequency {
    type Error = u8;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        Ok(match value {
            0 => Self::F96000,
            1 => Self::F88200,
            2 => Self::F64000,
            3 => Self::F48000,
            4 => Self::F44100,
            5 => Self::F32000,
            6 => Self::F24000,
            7 => Self::F22050,
            8 => Self::F16000,
            9 => Self::F12000,
            10 => Self::F11025,
            11 => Self::F8000,
            12 => Self::F7350,
            _ => return Err(value),
        })
    }
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
                    sampling_frequency: SamplingFrequency::try_from(sampling_frequency_index)
                        .unwrap(),
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

        let sync_word = u16::from(data[offset + 0]) << 3 | u16::from(data[offset + 1]) >> 5;
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
