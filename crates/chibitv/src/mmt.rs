use std::collections::BTreeMap;
use std::io::{BufRead, Cursor, ErrorKind, Read};
use std::sync::Mutex;

use anyhow::anyhow;
use bytes::{Buf, BufMut, Bytes, BytesMut};
use tracing::{debug, warn};

use chibitv_b60::compressed_ip::HcfbPacket;
use chibitv_b60::deflag::{Defragmenter, State};
use chibitv_b60::descriptor::{Descriptor, MpuExtendedTimestamp};
use chibitv_b60::message::Message;
use chibitv_b60::mfu::MfuPayload;
use chibitv_b60::mmtp::{
    FragmentationIndicator, MmtpPacket, MmtpPayload, MpuFragment, MpuFragmentType,
    SignalingMessage, SignalingMessagePayload,
};
use chibitv_b60::table::Table;
use chibitv_b60::tlv::{TlvPacket, TlvPacketType};

use crate::descrambler::Descrambler;
use crate::hevc::HevcParser;

// TODO: parse the MMTP packet to get the ECM header
const ECM_HEADER: [u8; 6] = [0x00, 0x00, 0x93, 0x2D, 0x1E, 0x01];

const MAX_TIMESTAMP_DESCRIPTOR: usize = 64;

#[derive(Clone, Debug)]
pub enum Payload {
    Mfu {
        pts: Option<f64>,
        dts: Option<f64>,
        data: Vec<u8>,
    },
    Message(Message),
}

#[derive(Clone, Debug)]
pub struct Packet {
    pub packet_id: u16,
    pub payload: Payload,
}

#[derive(Clone, Debug)]
pub struct MmtStream {
    packet_id: u16,
    deflagmenter: Defragmenter,
    last_sequence_number: u32,
    au_count: usize,
    timescale: Option<u32>,
    timestamps: BTreeMap<u32, u64>,
    ext_timestamps: BTreeMap<u32, MpuExtendedTimestamp>,
    dts_pts: Option<(f64, f64)>,
    asset_type: Option<[u8; 4]>,
    hevc_parser: HevcParser,
}

#[derive(Debug)]
pub struct MmtDemuxer<R: BufRead> {
    reader: R,
    descrambler: Descrambler,
    streams: BTreeMap<u16, Mutex<MmtStream>>,
}

impl<R: BufRead> MmtDemuxer<R> {
    pub fn new(reader: R, descrambler: Descrambler) -> Self {
        Self {
            reader,
            descrambler,
            streams: BTreeMap::new(),
        }
    }

    pub fn read(&mut self) -> anyhow::Result<Option<Vec<Packet>>> {
        let len = self.reader.skip_until(0x7F)?;
        if len == 0 {
            // EOF.
            return Ok(None);
        } else if len > 1 {
            debug!("Skipped {} octets.", len - 1);
        }

        let mut reader = Read::chain(Cursor::new(&[0x7F]), self.reader.by_ref());

        let tlv_packet = match TlvPacket::try_read(&mut reader) {
            Ok(Some(packet)) if packet.packet_type == TlvPacketType::CompressedIP => packet,
            Ok(_) => return Ok(Some(vec![])),
            Err(e) if e.kind() == ErrorKind::UnexpectedEof => return Ok(None),
            Err(e) => Err(e)?,
        };

        if let Some(ecm_index) = tlv_packet
            .data
            .as_ref()
            .windows(size_of_val(&ECM_HEADER))
            .position(|b| b == ECM_HEADER)
        {
            self.descrambler.push_ecm(
                (&tlv_packet.data[ecm_index + 2..ecm_index + 150])
                    .try_into()
                    .unwrap(),
            )?;

            return Ok(Some(vec![]));
        }

        let mut bytes = tlv_packet.data;
        let _hcfb_packet = HcfbPacket::read(&mut bytes)?;
        let mmtp_packet = MmtpPacket::read(&mut bytes)?;

        #[allow(clippy::map_entry)]
        if !self.streams.contains_key(&mmtp_packet.packet_id) {
            self.streams.insert(
                mmtp_packet.packet_id,
                Mutex::new(MmtStream {
                    packet_id: mmtp_packet.packet_id,
                    deflagmenter: Defragmenter::default(),
                    last_sequence_number: 0,
                    au_count: 0,
                    timescale: None,
                    timestamps: BTreeMap::new(),
                    ext_timestamps: BTreeMap::new(),
                    dts_pts: None,
                    asset_type: None,
                    hevc_parser: HevcParser::default(),
                }),
            );
        }

        let mut stream = self
            .streams
            .get(&mmtp_packet.packet_id)
            .unwrap()
            .lock()
            .unwrap();

        let mmtp_payload = MmtpPayload::try_from(&mmtp_packet)?;

        Ok(Some(match mmtp_payload {
            MmtpPayload::MpuFragment(mut mpu_fragment) => {
                if mpu_fragment.fragment_type != MpuFragmentType::Mfu {
                    return Ok(Some(vec![]));
                }

                assert!(
                    mpu_fragment.fragmentation_indicator == FragmentationIndicator::NotFragmented
                        || !mpu_fragment.aggregation_flag
                );

                match stream.deflagmenter.state() {
                    State::Init if !mmtp_packet.rap_flag => {
                        // Waiting for the first RAP
                        return Ok(Some(vec![]));
                    }
                    State::Init => {
                        stream.last_sequence_number = mpu_fragment.mpu_sequence_number;
                    }
                    _ if mpu_fragment.mpu_sequence_number == stream.last_sequence_number + 1 => {
                        stream.last_sequence_number = mpu_fragment.mpu_sequence_number;
                        stream.au_count = 0;
                    }
                    _ if mpu_fragment.mpu_sequence_number != stream.last_sequence_number => {
                        warn!(
                            "MPU sequence number jump: {} != {} + 1",
                            mpu_fragment.mpu_sequence_number, stream.last_sequence_number,
                        );

                        stream.last_sequence_number = mpu_fragment.mpu_sequence_number;
                        stream.au_count = 0;
                    }
                    _ => {}
                }

                stream.deflagmenter.sync(mmtp_packet.packet_sequence_number);

                self.descrambler
                    .descramble(&mmtp_packet, mpu_fragment.payload.as_mut_slice())
                    .map_err(|e| anyhow!("Could not descramble the payload: {}", e))?;

                Self::read_mfu(&mut stream, mpu_fragment)?
            }
            MmtpPayload::SignalingMessage(message) => {
                stream.deflagmenter.sync(mmtp_packet.packet_sequence_number);

                self.read_message(&mut stream, message)
            }
        }))
    }

    fn read_mfu(stream: &mut MmtStream, mpu_fragment: MpuFragment) -> anyhow::Result<Vec<Packet>> {
        let mfu_payload = MfuPayload::try_from(&mpu_fragment)?;
        let packet_id = stream.packet_id;
        let mpu_sequence_number = mpu_fragment.mpu_sequence_number;

        // TODO: This is O(n^2), will be a bottleneck
        let timestamp = stream.timestamps.get(&mpu_sequence_number).copied();
        let ext_timestamp = stream.ext_timestamps.get(&mpu_sequence_number).cloned();

        let data: Vec<_> = match mfu_payload {
            MfuPayload::TimedAggregated(aggregated_data) => aggregated_data
                .into_iter()
                .map(|timed_data| timed_data.data)
                .collect(),
            MfuPayload::Timed(timed_data) => stream
                .deflagmenter
                .push(mpu_fragment.fragmentation_indicator, &timed_data.data)
                .into_iter()
                .collect(),
            MfuPayload::Aggregated(aggregated_data) => aggregated_data
                .into_iter()
                .map(|non_timed_data| non_timed_data.data)
                .collect(),
            MfuPayload::Default(non_timed_data) => stream
                .deflagmenter
                .push(mpu_fragment.fragmentation_indicator, &non_timed_data.data)
                .into_iter()
                .collect(),
        };

        Ok(data
            .into_iter()
            .filter_map(|data| {
                let mut bytes = Bytes::from(data);

                if stream.dts_pts.is_none() {
                    if let (Some(presentation_time), Some(ext_timestamp), Some(timescale)) =
                        (&timestamp, &ext_timestamp, stream.timescale)
                    {
                        // See page 208 of the STD-B60 for this calculation.

                        let timescale = timescale as f64;

                        // presentation_time is a NTP timestamp, so let's convert to a normal float number.
                        let presentation_time = ((presentation_time >> 32) as f64)
                            + ((presentation_time & 0xFFFFFFFF) as f64) / (2u64.pow(32) as f64);

                        // DTS(m) = mpu_presentation_time
                        //            - mpu_decoding_time_offset / timescale
                        //            + \sum_{l=1}^{m-1} pts_offset(l) / timescale
                        let mut dts_sec = presentation_time
                            - (ext_timestamp.mpu_decoding_time_offset as f64) / timescale;

                        assert!(stream.au_count < ext_timestamp.num_of_au as usize);

                        for i in 0..stream.au_count {
                            dts_sec += (ext_timestamp.offsets[i].pts_offset as f64) / timescale;
                        }

                        // PTS(m) = DTS(m) + dts_pts_offset(m) / timescale
                        let pts_sec = dts_sec
                            + (ext_timestamp.offsets[stream.au_count].pts_dts_offset as f64)
                                / timescale;

                        stream.dts_pts = Some((dts_sec, pts_sec));
                        stream.au_count += 1;
                    }
                }

                let data = match &stream.asset_type? {
                    b"hev1" => {
                        // HEVC
                        let size = bytes.get_u32();
                        assert_eq!(size as usize, bytes.remaining(), "insufficient buffer size");

                        let mut data = BytesMut::new();
                        data.put_slice(&[0x00, 0x00, 0x01][..]);
                        data.put_slice(&bytes);

                        stream.hevc_parser.push(&data)?
                    }
                    b"mp4a" => {
                        // AAC-LATM
                        let size = bytes.remaining();

                        let mut data = BytesMut::new();
                        data.put(Bytes::copy_from_slice(
                            &[0x56, 0xe0 | (size >> 8) as u8, (size & 0xff) as u8][..],
                        ));
                        data.put(&mut bytes);

                        data.freeze()
                    }
                    _ => return None,
                };

                let (dts, pts) = std::mem::take(&mut stream.dts_pts).unzip();

                Some(Packet {
                    packet_id,
                    payload: Payload::Mfu {
                        dts,
                        pts,
                        data: data.to_vec(),
                    },
                })
            })
            .collect())
    }

    fn read_message(&self, stream: &mut MmtStream, message: SignalingMessage) -> Vec<Packet> {
        let packet_id = stream.packet_id;
        let messages: Vec<_> = match message.payload {
            SignalingMessagePayload::Aggregated(payloads) => payloads
                .into_iter()
                .filter_map(|payload| {
                    stream
                        .deflagmenter
                        .push(message.fragmentation_indicator, &payload)
                })
                .filter_map(|data| Message::read(Cursor::new(data)).ok())
                .collect(),
            SignalingMessagePayload::Default(payload) => stream
                .deflagmenter
                .push(message.fragmentation_indicator, payload.as_slice())
                .into_iter()
                .filter_map(|data| Message::read(Cursor::new(data)).ok())
                .collect(),
        };

        messages
            .into_iter()
            .map(|message| {
                if let Message::Pa(message) = &message {
                    for table in &message.tables {
                        let Table::Mpt(mpt) = table else {
                            continue;
                        };

                        for asset in &mpt.assets {
                            let packet_id = asset.locations.last().unwrap().packet_id().unwrap();

                            let Some(stream) = self.streams.get(&packet_id) else {
                                continue;
                            };

                            let mut stream = stream.lock().unwrap();

                            stream.asset_type = Some(asset.asset_type);

                            for descriptor in &asset.asset_descriptors {
                                match descriptor {
                                    Descriptor::MpuTimestamp(descriptor) => {
                                        for ts in &descriptor.timestamps {
                                            stream.timestamps.insert(
                                                ts.mpu_sequence_number,
                                                ts.mpu_presentation_time,
                                            );
                                        }
                                    }
                                    Descriptor::MpuExtendedTimestamp(descriptor) => {
                                        if let Some(scale) = descriptor.timescale {
                                            stream.timescale = Some(scale);
                                        }

                                        for ts in &descriptor.timestamps {
                                            stream
                                                .ext_timestamps
                                                .insert(ts.mpu_sequence_number, ts.clone());
                                        }
                                    }
                                    _ => {}
                                }
                            }

                            // Evict the oldest descriptors from the buffer.
                            while stream.timestamps.len() > MAX_TIMESTAMP_DESCRIPTOR {
                                _ = stream.timestamps.pop_first();
                            }

                            while stream.ext_timestamps.len() > MAX_TIMESTAMP_DESCRIPTOR {
                                _ = stream.ext_timestamps.pop_first();
                            }
                        }
                    }
                }

                Packet {
                    packet_id,
                    payload: Payload::Message(message),
                }
            })
            .collect()
    }

    pub fn clear(&mut self) {
        self.streams.clear();
        self.descrambler.clear();
    }
}
