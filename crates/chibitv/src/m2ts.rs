use std::cmp::min;
use std::collections::BTreeMap;
use std::io::Read;
use std::sync::RwLock;

use bytes::{Buf, Bytes};
use mpeg2ts::es::{StreamId, StreamType};
use mpeg2ts::pes::PesHeader;
use mpeg2ts::time::Timestamp;
use mpeg2ts::ts::payload::{Pat, Pes, Pmt};
use mpeg2ts::ts::{
    ContinuityCounter, Descriptor, EsInfo, Pid, ProgramAssociation, TransportScramblingControl,
    TsHeader, TsPacket, TsPayload, VersionNumber, WriteTsPacket,
};

use crate::remux::{Demux, Mux, Packet, TrackType};

// Multi-program in a stream won't be needed, I believe.
const PROGRAM_NUM: u16 = 0x0001;
const TS_PACKET_SIZE: usize = 188;
const TS_SYNC_BYTE: u8 = 0x47;

#[inline]
fn pat_pid() -> Pid {
    Pid::new(0x0000).unwrap()
}

#[inline]
fn pmt_pid() -> Pid {
    Pid::new(0x1000).unwrap()
}

#[derive(Debug)]
pub enum M2tsPayload {
    Pat {
        transport_stream_id: u16,
        version_number: VersionNumber,
        program_num: u16,
        pmt_pid: u16,
    },
    Pmt {
        program_num: u16,
        pcr_pid: Option<u16>,
        version_number: VersionNumber,
        program_info: Vec<Descriptor>,
        es_info: Vec<EsInfo>,
    },
    Raw(Vec<u8>),
}

#[derive(Debug)]
pub struct M2tsPacket {
    pub pid: u16,
    pub payload: M2tsPayload,
}

#[derive(Debug)]
struct PesBuffer {
    data: Vec<u8>,
    dts: Option<f64>,
    pts: Option<f64>,
}

#[derive(Debug)]
struct PesStart {
    data: Vec<u8>,
    dts: Option<f64>,
    pts: Option<f64>,
}

#[derive(Debug, Default)]
struct TrackState {
    es_info: Option<EsInfo>,
    track_type: Option<TrackType>,
    track_emitted: bool,
    pes: Option<PesBuffer>,
}

#[derive(Debug)]
pub struct M2tsDemuxer<R> {
    reader: R,
    pending: Vec<u8>,
    aligned: bool,
    transport_stream_id: u16,
    pat_version_number: VersionNumber,
    program_num: u16,
    pmt_pid: Option<u16>,
    pcr_pid: Option<u16>,
    pmt_version_number: VersionNumber,
    program_info: Vec<Descriptor>,
    psi_buffers: BTreeMap<u16, Vec<u8>>,
    tracks: BTreeMap<u16, TrackState>,
    eof_flushed: bool,
}

impl<R: Read> M2tsDemuxer<R> {
    pub fn new(reader: R) -> Self {
        Self {
            reader,
            pending: Vec::new(),
            aligned: false,
            transport_stream_id: PROGRAM_NUM,
            pat_version_number: VersionNumber::default(),
            program_num: PROGRAM_NUM,
            pmt_pid: None,
            pcr_pid: None,
            pmt_version_number: VersionNumber::default(),
            program_info: Vec::new(),
            psi_buffers: BTreeMap::new(),
            tracks: BTreeMap::new(),
            eof_flushed: false,
        }
    }

    pub fn read(&mut self) -> anyhow::Result<Option<Vec<M2tsPacket>>> {
        while let Some(packet) = self.read_packet()? {
            let header = TsPacketHeader::parse(&packet)?;
            if header.pid == 0x0000 {
                return Ok(Some(self.read_pat(&packet, &header)?.into_iter().collect()));
            }

            if Some(header.pid) == self.pmt_pid {
                return Ok(Some(self.read_pmt(&packet, &header)?.into_iter().collect()));
            }

            if self.tracks.contains_key(&header.pid) {
                return Ok(Some(vec![M2tsPacket {
                    pid: header.pid,
                    payload: M2tsPayload::Raw(packet),
                }]));
            }

            return Ok(Some(vec![]));
        }

        Ok(None)
    }

    fn read_packet(&mut self) -> anyhow::Result<Option<Vec<u8>>> {
        if !self.aligned {
            let mut buffer = vec![0; TS_PACKET_SIZE * 8192];
            let read = self.reader.read(&mut buffer)?;
            if read == 0 {
                return Ok(None);
            }
            buffer.truncate(read);

            let Some(offset) = find_sync_offset(&buffer) else {
                anyhow::bail!("Could not find MPEG-TS sync byte");
            };

            self.pending.extend_from_slice(&buffer[offset..]);
            self.aligned = true;
        }

        while self.pending.len() < TS_PACKET_SIZE {
            let mut buffer = [0; TS_PACKET_SIZE * 32];
            let read = self.reader.read(&mut buffer)?;
            if read == 0 {
                return Ok(None);
            }

            self.pending.extend_from_slice(&buffer[..read]);
        }

        let packet = self.pending.drain(..TS_PACKET_SIZE).collect::<Vec<_>>();
        if packet[0] != TS_SYNC_BYTE {
            anyhow::bail!("MPEG-TS sync byte mismatch");
        }

        Ok(Some(packet))
    }

    fn read_pat(
        &mut self,
        packet: &[u8],
        header: &TsPacketHeader,
    ) -> anyhow::Result<Option<M2tsPacket>> {
        let Some(section) = self.read_psi_section(packet, header)? else {
            return Ok(None);
        };
        if section.first().copied() != Some(0x00) {
            return Ok(None);
        }

        let section_length = section_length(&section)?;
        let end = 3 + section_length;
        if section.len() < end || end < 12 {
            return Ok(None);
        }

        self.transport_stream_id = u16::from_be_bytes([section[3], section[4]]);
        self.pat_version_number = VersionNumber::from_u8((section[5] >> 1) & 0x1f)?;

        let mut pmt_pid = None;
        let entries_end = end - 4;
        for entry in section[8..entries_end].chunks_exact(4) {
            let program_number = u16::from_be_bytes([entry[0], entry[1]]);
            if program_number == 0 {
                continue;
            }

            let pid = u16::from_be_bytes([entry[2] & 0x1f, entry[3]]);
            self.program_num = program_number;
            self.pmt_pid = Some(pid);
            self.tracks.entry(pid).or_default();
            pmt_pid = Some(pid);
            break;
        }

        let Some(pmt_pid) = pmt_pid else {
            return Ok(None);
        };

        Ok(Some(M2tsPacket {
            pid: header.pid,
            payload: M2tsPayload::Pat {
                transport_stream_id: self.transport_stream_id,
                version_number: self.pat_version_number,
                program_num: self.program_num,
                pmt_pid,
            },
        }))
    }

    fn read_pmt(
        &mut self,
        packet: &[u8],
        header: &TsPacketHeader,
    ) -> anyhow::Result<Option<M2tsPacket>> {
        let Some(section) = self.read_psi_section(packet, header)? else {
            return Ok(None);
        };
        if section.first().copied() != Some(0x02) {
            return Ok(None);
        }

        let section_length = section_length(&section)?;
        let end = 3 + section_length;
        if section.len() < end || end < 16 {
            return Ok(None);
        }

        self.program_num = u16::from_be_bytes([section[3], section[4]]);
        self.pmt_version_number = VersionNumber::from_u8((section[5] >> 1) & 0x1f)?;

        let pcr_pid = u16::from_be_bytes([section[8] & 0x1f, section[9]]);
        self.pcr_pid = (pcr_pid != 0x1fff).then_some(pcr_pid);
        if let Some(pcr_pid) = self.pcr_pid {
            self.tracks.entry(pcr_pid).or_default();
        }

        let program_info_length = u16::from_be_bytes([section[10] & 0x0f, section[11]]) as usize;
        self.program_info = read_descriptors(&section[12..12 + program_info_length])?;
        let mut selected_es_info = Vec::new();

        let mut offset = 12 + program_info_length;
        let entries_end = end - 4;
        let mut has_video = false;
        let mut has_audio = false;

        while offset + 5 <= entries_end {
            let stream_type = section[offset];
            let pid = u16::from_be_bytes([section[offset + 1] & 0x1f, section[offset + 2]]);
            let es_info_length =
                u16::from_be_bytes([section[offset + 3] & 0x0f, section[offset + 4]]) as usize;

            if !has_video && is_video_stream_type(stream_type) {
                let es_info = EsInfo {
                    stream_type: StreamType::from_u8(stream_type)?,
                    elementary_pid: Pid::new(pid).unwrap(),
                    descriptors: read_descriptors(
                        &section[offset + 5..offset + 5 + es_info_length],
                    )?,
                };
                let state = self.tracks.entry(pid).or_default();
                state.track_type = track_type_from_stream_type(es_info.stream_type);
                state.es_info = Some(es_info.clone());
                selected_es_info.push(es_info);
                has_video = true;
            } else if !has_audio && is_audio_stream_type(stream_type) {
                let es_info = EsInfo {
                    stream_type: StreamType::from_u8(stream_type)?,
                    elementary_pid: Pid::new(pid).unwrap(),
                    descriptors: read_descriptors(
                        &section[offset + 5..offset + 5 + es_info_length],
                    )?,
                };
                let state = self.tracks.entry(pid).or_default();
                state.track_type = track_type_from_stream_type(es_info.stream_type);
                state.es_info = Some(es_info.clone());
                selected_es_info.push(es_info);
                has_audio = true;
            }

            offset += 5 + es_info_length;
        }

        if selected_es_info.is_empty() {
            return Ok(None);
        }

        Ok(Some(M2tsPacket {
            pid: header.pid,
            payload: M2tsPayload::Pmt {
                program_num: self.program_num,
                pcr_pid: self.pcr_pid,
                version_number: self.pmt_version_number,
                program_info: self.program_info.clone(),
                es_info: selected_es_info,
            },
        }))
    }

    fn read_psi_section(
        &mut self,
        packet: &[u8],
        header: &TsPacketHeader,
    ) -> anyhow::Result<Option<Vec<u8>>> {
        let Some(payload_offset) = payload_offset(packet, header) else {
            return Ok(None);
        };

        let payload = &packet[payload_offset..];
        let buffer = self.psi_buffers.entry(header.pid).or_default();

        if header.payload_unit_start_indicator {
            let Some(pointer_field) = payload.first().copied() else {
                return Ok(None);
            };

            let section_offset = 1 + usize::from(pointer_field);
            if section_offset >= payload.len() {
                return Ok(None);
            }

            buffer.clear();
            buffer.extend_from_slice(&payload[section_offset..]);
        } else if !buffer.is_empty() {
            buffer.extend_from_slice(payload);
        }

        if buffer.len() < 3 {
            return Ok(None);
        }

        let section_length = section_length(buffer)?;
        let end = 3 + section_length;
        if buffer.len() < end {
            return Ok(None);
        }

        Ok(Some(buffer[..end].to_vec()))
    }
}

impl<R: Read> Demux for M2tsDemuxer<R> {
    fn read(&mut self) -> anyhow::Result<Option<Vec<Packet>>> {
        loop {
            let Some(packets) = M2tsDemuxer::read(self)? else {
                if self.eof_flushed {
                    return Ok(None);
                }

                self.eof_flushed = true;
                let packets = self.flush_pes_buffers();
                return Ok((!packets.is_empty()).then_some(packets));
            };

            let mut out = Vec::new();
            for packet in packets {
                match packet.payload {
                    M2tsPayload::Pat {
                        transport_stream_id,
                        version_number,
                        program_num,
                        pmt_pid,
                    } => {
                        let _ = (transport_stream_id, version_number, program_num, pmt_pid);
                    }
                    M2tsPayload::Pmt {
                        program_num,
                        pcr_pid,
                        version_number,
                        program_info,
                        es_info,
                    } => {
                        let _ = (program_num, pcr_pid, version_number, program_info);
                        for info in es_info {
                            let track_id = info.elementary_pid.as_u16();
                            let Some(state) = self.tracks.get_mut(&track_id) else {
                                continue;
                            };
                            let Some(ty) = state.track_type else {
                                continue;
                            };
                            if !state.track_emitted {
                                state.track_emitted = true;
                                out.push(Packet::Track { track_id, ty });
                            }
                        }
                    }
                    M2tsPayload::Raw(data) => {
                        if let Some(sample) = self.read_pes_packet(packet.pid, &data)? {
                            out.push(sample);
                        }
                    }
                }
            }

            if !out.is_empty() {
                return Ok(Some(out));
            }
        }
    }
}

impl<R: Read> M2tsDemuxer<R> {
    fn read_pes_packet(&mut self, pid: u16, packet: &[u8]) -> anyhow::Result<Option<Packet>> {
        let Some(track_type) = self.tracks.get(&pid).and_then(|state| state.track_type) else {
            return Ok(None);
        };

        let header = TsPacketHeader::parse(packet)?;
        let Some(payload_offset) = payload_offset(packet, &header) else {
            return Ok(None);
        };
        let payload = &packet[payload_offset..];

        if header.payload_unit_start_indicator {
            let finished = self
                .tracks
                .get_mut(&pid)
                .and_then(|state| state.pes.take())
                .and_then(|buffer| {
                    (!buffer.data.is_empty()).then_some(Packet::Sample {
                        track_id: pid,
                        data: Bytes::from(buffer.data),
                        dts: buffer.dts,
                        pts: buffer.pts,
                    })
                });

            if let Some(pes) = parse_pes_start(payload)? {
                if let Some(state) = self.tracks.get_mut(&pid) {
                    state.pes = Some(PesBuffer {
                        data: pes.data,
                        dts: pes.dts,
                        pts: pes.pts,
                    });
                    state.track_type = Some(track_type);
                }
            } else {
                if let Some(state) = self.tracks.get_mut(&pid) {
                    state.track_type = None;
                    state.pes = None;
                }
                return Ok(None);
            }

            return Ok(finished);
        }

        if let Some(buffer) = self
            .tracks
            .get_mut(&pid)
            .and_then(|state| state.pes.as_mut())
        {
            buffer.data.extend_from_slice(payload);
        }

        Ok(None)
    }

    fn flush_pes_buffers(&mut self) -> Vec<Packet> {
        self.tracks
            .iter_mut()
            .filter_map(|(&track_id, state)| {
                let buffer = state.pes.take()?;
                (!buffer.data.is_empty()).then_some(Packet::Sample {
                    track_id,
                    data: Bytes::from(buffer.data),
                    dts: buffer.dts,
                    pts: buffer.pts,
                })
            })
            .collect()
    }
}

#[derive(Debug)]
struct TsPacketHeader {
    pid: u16,
    payload_unit_start_indicator: bool,
    adaptation_field_control: u8,
}

impl TsPacketHeader {
    fn parse(packet: &[u8]) -> anyhow::Result<Self> {
        if packet.len() != TS_PACKET_SIZE || packet[0] != TS_SYNC_BYTE {
            anyhow::bail!("Invalid MPEG-TS packet");
        }

        Ok(Self {
            pid: (u16::from(packet[1] & 0x1f) << 8) | u16::from(packet[2]),
            payload_unit_start_indicator: packet[1] & 0x40 != 0,
            adaptation_field_control: (packet[3] >> 4) & 0x03,
        })
    }
}

fn payload_offset(packet: &[u8], header: &TsPacketHeader) -> Option<usize> {
    if header.adaptation_field_control & 0x01 == 0 {
        return None;
    }

    let mut offset = 4;
    if header.adaptation_field_control & 0x02 != 0 {
        let adaptation_field_length = usize::from(*packet.get(offset)?);
        offset += 1 + adaptation_field_length;
    }

    (offset < packet.len()).then_some(offset)
}

fn section_length(section: &[u8]) -> anyhow::Result<usize> {
    if section.len() < 3 {
        anyhow::bail!("PSI section is too short");
    }

    Ok(usize::from(u16::from_be_bytes([
        section[1] & 0x0f,
        section[2],
    ])))
}

fn find_sync_offset(buffer: &[u8]) -> Option<usize> {
    const MIN_SYNC_PACKETS: usize = 3;

    (0..buffer.len()).find(|&offset| {
        (0..MIN_SYNC_PACKETS).all(|index| {
            buffer
                .get(offset + index * TS_PACKET_SIZE)
                .is_some_and(|byte| *byte == TS_SYNC_BYTE)
        })
    })
}

fn is_video_stream_type(stream_type: u8) -> bool {
    matches!(stream_type, 0x01 | 0x02 | 0x1b | 0x24)
}

fn is_audio_stream_type(stream_type: u8) -> bool {
    matches!(stream_type, 0x03 | 0x04 | 0x0f | 0x11)
}

fn track_type_from_stream_type(stream_type: StreamType) -> Option<TrackType> {
    match stream_type {
        StreamType::Mpeg2Video => Some(TrackType::Mpeg2Video),
        StreamType::AdtsAac => Some(TrackType::AacAdts),
        StreamType::H265 => Some(TrackType::H265),
        StreamType::Mpeg4LoasMultiFormatFramedAudio => Some(TrackType::AacLatm),
        _ => None,
    }
}

fn parse_pes_start(payload: &[u8]) -> anyhow::Result<Option<PesStart>> {
    if payload.len() < 6 || payload[..3] != [0x00, 0x00, 0x01] {
        return Ok(None);
    }

    if payload.len() < 9 {
        anyhow::bail!("PES header is too short");
    }

    let pts_dts_flags = (payload[7] >> 6) & 0x03;
    let header_data_length = usize::from(payload[8]);
    let data_offset = 9 + header_data_length;
    if payload.len() < data_offset {
        anyhow::bail!("PES header data length is invalid");
    }

    let pts = match pts_dts_flags {
        0b10 | 0b11 if payload.len() >= 14 => Some(read_pes_timestamp(&payload[9..14])?),
        _ => None,
    };
    let dts = match pts_dts_flags {
        0b11 if payload.len() >= 19 => Some(read_pes_timestamp(&payload[14..19])?),
        _ => pts,
    };

    Ok(Some(PesStart {
        data: payload[data_offset..].to_vec(),
        dts,
        pts,
    }))
}

fn read_pes_timestamp(data: &[u8]) -> anyhow::Result<f64> {
    if data.len() < 5 {
        anyhow::bail!("PES timestamp is too short");
    }

    let timestamp = ((u64::from(data[0] >> 1) & 0x07) << 30)
        | (u64::from(data[1]) << 22)
        | ((u64::from(data[2] >> 1) & 0x7f) << 15)
        | (u64::from(data[3]) << 7)
        | (u64::from(data[4] >> 1) & 0x7f);

    Ok(timestamp as f64 / 90_000_f64)
}

fn read_descriptors(mut data: &[u8]) -> anyhow::Result<Vec<Descriptor>> {
    let mut descriptors = Vec::new();

    while !data.is_empty() {
        if data.len() < 2 {
            anyhow::bail!("Descriptor is too short");
        }

        let tag = data[0];
        let length = usize::from(data[1]);
        if data.len() < 2 + length {
            anyhow::bail!("Descriptor length is invalid");
        }

        descriptors.push(Descriptor {
            tag,
            data: data[2..2 + length].to_vec(),
        });
        data = &data[2 + length..];
    }

    Ok(descriptors)
}

pub struct M2tsStream {
    cc: ContinuityCounter,
    stream_id: Option<StreamId>,
    es_info: Option<EsInfo>,
}

impl M2tsStream {
    fn new() -> Self {
        Self {
            cc: ContinuityCounter::new(),
            stream_id: None,
            es_info: None,
        }
    }

    fn new_es(stream_id: StreamId, es_info: EsInfo) -> Self {
        Self {
            cc: ContinuityCounter::new(),
            stream_id: Some(stream_id),
            es_info: Some(es_info),
        }
    }
}

impl M2tsStream {
    fn next_cc(&mut self) -> ContinuityCounter {
        self.cc.increment();
        self.cc
    }
}

fn default_streams() -> BTreeMap<Pid, RwLock<M2tsStream>> {
    BTreeMap::from_iter([
        (pat_pid(), RwLock::new(M2tsStream::new())),
        (pmt_pid(), RwLock::new(M2tsStream::new())),
    ])
}

pub struct M2tsMuxer<W> {
    writer: W,
    track_map: BTreeMap<u16, Pid>,
    streams: BTreeMap<Pid, RwLock<M2tsStream>>,
    last_pat_pmt_ts: Option<f64>,
}

impl<W: WriteTsPacket + Send + Sync> M2tsMuxer<W> {
    pub fn new(writer: W) -> Self {
        Self {
            writer,
            track_map: BTreeMap::new(),
            streams: default_streams(),
            last_pat_pmt_ts: None,
        }
    }

    pub fn add_stream(&mut self, pid: Pid, stream_id: StreamId, es_info: EsInfo) {
        self.streams
            .insert(pid, RwLock::new(M2tsStream::new_es(stream_id, es_info)));
    }

    pub fn write_pes(
        &mut self,
        pid: Pid,
        mut data: Bytes,
        dts: Option<f64>,
        pts: Option<f64>,
    ) -> anyhow::Result<()> {
        // Emit PAT + PMT for each 100 ms
        if let Some(dts) = dts
            && self.last_pat_pmt_ts.is_none_or(|ts| dts - ts >= 0.1_f64)
        {
            self.emit_pat_pmt()?;
            self.last_pat_pmt_ts = Some(dts);
        }

        let mut stream = self.streams.get(&pid).unwrap().write().unwrap();

        let mut header_len = 9;
        let dts = dts.map(|dts| {
            header_len += 5;
            Timestamp::new(((dts * 90_000_f64) as u64) % Timestamp::MAX).unwrap()
        });
        let pts = pts.map(|pts| {
            header_len += 5;
            Timestamp::new(((pts * 90_000_f64) as u64) % Timestamp::MAX).unwrap()
        });
        // TODO: Add PCR packets to sync clock correctly.

        let payload = data.split_to(min(data.remaining(), 188 - 4 - header_len));

        // Emit the first packet.
        self.writer.write_ts_packet(&TsPacket {
            header: TsHeader {
                pid,
                continuity_counter: stream.next_cc(),
                transport_error_indicator: false,
                transport_priority: false,
                transport_scrambling_control: TransportScramblingControl::NotScrambled,
            },
            payload: Some(TsPayload::PesStart(Pes {
                header: PesHeader {
                    stream_id: stream.stream_id.unwrap(),
                    priority: false,
                    data_alignment_indicator: false,
                    copyright: false,
                    original_or_copy: false,
                    dts,
                    pts,
                    escr: None,
                },
                pes_packet_len: 0,
                data: mpeg2ts::ts::payload::Bytes::new(&payload).unwrap(),
            })),
            adaptation_field: None,
        })?;

        // Emit extra packets until the data were consumed fully.
        while data.has_remaining() {
            let payload = data.split_to(min(data.remaining(), 188 - 4));

            self.writer.write_ts_packet(&TsPacket {
                header: TsHeader {
                    pid,
                    continuity_counter: stream.next_cc(),
                    transport_error_indicator: false,
                    transport_priority: false,
                    transport_scrambling_control: TransportScramblingControl::NotScrambled,
                },
                payload: Some(TsPayload::Raw(
                    mpeg2ts::ts::payload::Bytes::new(&payload).unwrap(),
                )),
                adaptation_field: None,
            })?;
        }

        Ok(())
    }

    fn emit_pat_pmt(&mut self) -> mpeg2ts::Result<()> {
        let es_info = self
            .streams
            .values()
            .filter_map(|stream| stream.read().ok()?.es_info.as_ref().cloned())
            .collect::<Vec<_>>();

        let mut pat_stream = self.streams.get(&pat_pid()).unwrap().write().unwrap();
        let mut pmt_stream = self.streams.get(&pmt_pid()).unwrap().write().unwrap();

        // Emit a PAT.
        self.writer.write_ts_packet(&TsPacket {
            header: TsHeader {
                pid: pat_pid(),
                continuity_counter: pat_stream.next_cc(),
                transport_error_indicator: false,
                transport_priority: false,
                transport_scrambling_control: TransportScramblingControl::NotScrambled,
            },
            payload: Some(TsPayload::Pat(Pat {
                transport_stream_id: 0x0001, // TODO: What's this?
                version_number: VersionNumber::default(),
                table: vec![ProgramAssociation {
                    program_num: PROGRAM_NUM,
                    program_map_pid: pmt_pid(),
                }],
            })),
            adaptation_field: None,
        })?;

        // Emit a PMT.
        self.writer.write_ts_packet(&TsPacket {
            header: TsHeader {
                pid: pmt_pid(),
                continuity_counter: pmt_stream.next_cc(),
                transport_error_indicator: false,
                transport_priority: false,
                transport_scrambling_control: TransportScramblingControl::NotScrambled,
            },
            payload: Some(TsPayload::Pmt(Pmt {
                program_num: PROGRAM_NUM,
                version_number: VersionNumber::default(),
                pcr_pid: None,
                es_info,
                program_info: vec![],
            })),
            adaptation_field: None,
        })?;

        Ok(())
    }
}

impl<W: WriteTsPacket + Send + Sync> Mux for M2tsMuxer<W> {
    fn add_track(&mut self, track_id: u16, ty: TrackType) {
        // Already added streams.
        // TODO: Support video-only or audio-only mux
        // TODO: Compare the streams and handle changes?
        if self.track_map.len() >= 2 {
            return;
        }

        match ty {
            TrackType::Mpeg2Video => {
                let pid = Pid::new(0x0100).unwrap();

                self.track_map.insert(track_id, pid);
                self.add_stream(
                    pid,
                    StreamId::new_video(0xe0).unwrap(),
                    EsInfo {
                        elementary_pid: pid,
                        stream_type: StreamType::Mpeg2Video,
                        descriptors: vec![],
                    },
                );
            }
            TrackType::AacAdts => {
                let pid = Pid::new(0x0110).unwrap();

                self.track_map.insert(track_id, pid);
                self.add_stream(
                    pid,
                    StreamId::new_audio(0xc0).unwrap(),
                    EsInfo {
                        elementary_pid: pid,
                        stream_type: StreamType::AdtsAac,
                        descriptors: vec![],
                    },
                );
            }
            TrackType::H265 => {
                let pid = Pid::new(0x1011).unwrap();

                self.track_map.insert(track_id, pid);
                self.add_stream(
                    pid,
                    StreamId::new_video(0xe0).unwrap(),
                    EsInfo {
                        elementary_pid: pid,
                        stream_type: StreamType::H265,
                        descriptors: vec![Descriptor {
                            tag: 0x05,
                            data: b"HEVC".to_vec(),
                        }],
                    },
                );
            }
            TrackType::AacLatm => {
                let pid = Pid::new(0x1100).unwrap();

                self.track_map.insert(track_id, pid);
                self.add_stream(
                    pid,
                    StreamId::new_audio(0xc0).unwrap(),
                    EsInfo {
                        elementary_pid: pid,
                        stream_type: StreamType::Mpeg4LoasMultiFormatFramedAudio, // AAC-LATM
                        descriptors: vec![],
                    },
                );
            }
        }
    }

    fn write_sample(
        &mut self,
        track_id: u16,
        data: Bytes,
        dts: Option<f64>,
        pts: Option<f64>,
    ) -> anyhow::Result<()> {
        let Some(pid) = self.track_map.get(&track_id).copied() else {
            // The stream is not yet added, or unrecognisable.
            return Ok(());
        };

        self.write_pes(pid, data, dts, pts)
    }
}
