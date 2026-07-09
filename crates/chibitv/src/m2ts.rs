use std::cmp::min;
use std::collections::{BTreeMap, BTreeSet};
use std::io::{self, Read};
use std::sync::RwLock;
use std::sync::{Arc, Mutex};

use bytes::{Buf, Bytes, BytesMut};
use mpeg2ts::es::{StreamId, StreamType};
use mpeg2ts::pes::PesHeader;
use mpeg2ts::time::Timestamp;
use mpeg2ts::ts::payload::{Pat, Pes, Pmt};
use mpeg2ts::ts::{
    ContinuityCounter, Descriptor, EsInfo, Pid, ProgramAssociation, ReadTsPacket,
    TransportScramblingControl, TsHeader, TsPacket, TsPacketReader, TsPayload, VersionNumber,
    WriteTsPacket,
};

use chibitv_b10::table::Table as B10Table;
use chibitv_b25::{B25Descrambler, NoDecryptionKeyError};

use crate::remux::{Demux, Mux, Packet, TrackType};

#[derive(Debug, Default)]
struct PesBuffer {
    data: BytesMut,
    dts: Option<f64>,
    pts: Option<f64>,
}

#[derive(Debug)]
struct TrackState {
    pes: PesBuffer,
}

#[derive(Debug)]
pub struct M2tsDemuxer<R> {
    reader: TsPacketReader<AlignedTsReader<R>>,
    descrambler: Arc<Mutex<B25Descrambler>>,
    ecm_pids: BTreeSet<Pid>,
    tracks: BTreeMap<Pid, TrackState>,
    section_buffers: BTreeMap<Pid, Vec<u8>>,
}

impl<R: Read> M2tsDemuxer<R> {
    pub fn new(reader: R, descrambler: B25Descrambler) -> Self {
        let descrambler = Arc::new(Mutex::new(descrambler));
        let mut reader = TsPacketReader::new(AlignedTsReader::new(reader));

        for pid in B10_SECTION_PIDS {
            reader.add_section_pid(Pid::new(*pid).expect("B10 section PID must be valid"));
        }

        Self {
            reader,
            descrambler,
            ecm_pids: BTreeSet::new(),
            tracks: BTreeMap::new(),
            section_buffers: BTreeMap::new(),
        }
    }

    fn read_ecm(&mut self, section: Bytes) -> anyhow::Result<()> {
        if section.first().copied() != Some(0x82) {
            return Ok(());
        }

        let section_syntax_indicator = section[1] & 0x80 != 0;
        let data_offset = if section_syntax_indicator { 8 } else { 3 };
        if section.len() < data_offset + 4 {
            return Ok(());
        }

        let ecm_payload = &section[data_offset..section.len() - 4];
        self.descrambler.lock().unwrap().push_ecm(ecm_payload)
    }

    fn add_ecm_pid(&mut self, pid: Pid) {
        if self.ecm_pids.insert(pid) {
            self.reader.add_section_pid(pid);
        }
    }
}

impl<R: Read> Demux for M2tsDemuxer<R> {
    fn read(&mut self) -> anyhow::Result<Option<Vec<Packet>>> {
        let mut out = Vec::new();
        loop {
            let mut packet = match self.reader.read_ts_packet() {
                Ok(Some(packet)) => packet,
                Ok(None) => break,
                Err(_) => {
                    if out.is_empty() {
                        continue;
                    }

                    return Ok(Some(out));
                }
            };

            let pid = packet.header.pid;
            if packet.header.transport_scrambling_control
                != TransportScramblingControl::NotScrambled
            {
                let result = self.descrambler.lock().unwrap().descramble(&mut packet);
                if let Err(error) = result {
                    if error.is::<NoDecryptionKeyError>() {
                        continue;
                    }

                    return Err(error);
                }

                if self.tracks.contains_key(&pid) {
                    parse_pes_payload(&mut packet)?;
                }
            }

            let Some(payload) = packet.payload else {
                continue;
            };

            match payload {
                TsPayload::Pmt(pmt) => {
                    let ca_system_id = self.descrambler.lock().unwrap().ca_system_id();

                    for info in pmt.program_info {
                        if let Some(pid) = ca_descriptor_pid(&info, ca_system_id)? {
                            self.add_ecm_pid(pid);
                        }
                    }

                    let mut selected_video = None;
                    let mut selected_audio = None;

                    for info in pmt.es_info {
                        let pid = info.elementary_pid;
                        let Some(track_type) = track_type_from_stream_type(info.stream_type) else {
                            continue;
                        };
                        for descriptor in &info.descriptors {
                            if let Some(pid) = ca_descriptor_pid(descriptor, ca_system_id)? {
                                self.add_ecm_pid(pid);
                            }
                        }
                        if self.tracks.contains_key(&pid) {
                            continue;
                        }

                        if is_video_track_type(track_type) {
                            if matches!(track_type, TrackType::Mpeg2Video) {
                                selected_video = Some((pid, track_type));
                            }
                        } else if is_audio_track_type(track_type) && selected_audio.is_none() {
                            selected_audio = Some((pid, track_type));
                        }
                    }

                    if selected_video.is_none() {
                        continue;
                    }

                    for (pid, track_type) in [selected_video, selected_audio].into_iter().flatten()
                    {
                        self.tracks.insert(
                            pid,
                            TrackState {
                                pes: PesBuffer::default(),
                            },
                        );
                        out.push(Packet::Track {
                            track_id: pid.as_u16(),
                            ty: track_type,
                        });
                    }
                }
                TsPayload::Section(section) => {
                    let sections = read_sections(
                        &mut self.section_buffers,
                        pid,
                        packet.header.payload_unit_start_indicator,
                        section.pointer_field,
                        section.data.as_ref(),
                    );
                    if sections.is_empty() {
                        continue;
                    };

                    for section in sections {
                        if self.ecm_pids.contains(&pid) {
                            self.read_ecm(Bytes::from(section))?;
                            continue;
                        }

                        let Some(table_id) = section.first().copied() else {
                            continue;
                        };

                        let mut bytes = Bytes::from(section);
                        let table = B10Table::read(&mut bytes)?;
                        if !matches!(table, B10Table::Unknown(_, _)) {
                            out.push(Packet::B10Table { table_id, table });
                        }
                    }
                }
                TsPayload::PesStart(pes) => {
                    let Some(state) = self.tracks.get_mut(&pid) else {
                        continue;
                    };

                    let buffer = std::mem::take(&mut state.pes);
                    let finished = (!buffer.data.is_empty()).then_some(Packet::Sample {
                        track_id: pid.as_u16(),
                        data: buffer.data.freeze(),
                        dts: buffer.dts,
                        pts: buffer.pts,
                    });

                    state.pes = PesBuffer {
                        data: BytesMut::from(Bytes::from(pes.data.to_vec())),
                        dts: pes.header.dts.map(timestamp_to_seconds),
                        pts: pes.header.pts.map(timestamp_to_seconds),
                    };

                    if let Some(sample) = finished {
                        out.push(sample);
                    }
                }
                TsPayload::PesContinuation(payload) => {
                    let Some(state) = self.tracks.get_mut(&pid) else {
                        continue;
                    };
                    if state.pes.data.is_empty() {
                        continue;
                    }

                    state.pes.data.extend_from_slice(payload.as_ref());
                }
                _ => {}
            };

            if !out.is_empty() {
                return Ok(Some(out));
            }
        }

        let flushed = self.flush_pes_buffers();
        if !flushed.is_empty() {
            return Ok(Some(flushed));
        }

        Ok(None)
    }
}

impl<R: Read> M2tsDemuxer<R> {
    fn flush_pes_buffers(&mut self) -> Vec<Packet> {
        self.tracks
            .iter_mut()
            .filter_map(|(&track_id, state)| {
                let buffer = std::mem::take(&mut state.pes);
                (!buffer.data.is_empty()).then_some(Packet::Sample {
                    track_id: track_id.as_u16(),
                    data: buffer.data.freeze(),
                    dts: buffer.dts,
                    pts: buffer.pts,
                })
            })
            .collect()
    }
}

fn read_sections(
    section_buffers: &mut BTreeMap<Pid, Vec<u8>>,
    pid: Pid,
    payload_unit_start_indicator: bool,
    pointer_field: u8,
    payload: &[u8],
) -> Vec<Vec<u8>> {
    let mut sections = Vec::new();
    let buffer = section_buffers.entry(pid).or_default();

    if payload_unit_start_indicator {
        let new_section_offset = usize::from(pointer_field);
        if new_section_offset > payload.len() {
            buffer.clear();
            return sections;
        }

        if new_section_offset > 0 {
            if !buffer.is_empty() {
                buffer.extend_from_slice(&payload[..new_section_offset]);
                drain_complete_sections(buffer, &mut sections);
            }

            buffer.clear();
        } else if !buffer.is_empty() {
            buffer.clear();
        }

        if new_section_offset == payload.len() {
            return sections;
        }

        buffer.extend_from_slice(&payload[new_section_offset..]);
    } else if !buffer.is_empty() {
        buffer.extend_from_slice(payload);
    } else {
        return sections;
    }

    drain_complete_sections(buffer, &mut sections);

    sections
}

fn drain_complete_sections(buffer: &mut Vec<u8>, sections: &mut Vec<Vec<u8>>) {
    loop {
        if buffer.first().copied() == Some(0xFF) {
            buffer.clear();
            break;
        }

        if buffer.len() < 3 {
            break;
        }

        let section_length = usize::from(u16::from_be_bytes([buffer[1] & 0x0F, buffer[2]]));
        let section_end = 3 + section_length;
        if buffer.len() < section_end {
            break;
        }

        sections.push(buffer[..section_end].to_vec());
        buffer.drain(..section_end);
    }
}

#[derive(Debug)]
struct AlignedTsReader<R> {
    inner: R,
    pending: Vec<u8>,
    aligned: bool,
}

impl<R: Read> AlignedTsReader<R> {
    fn new(inner: R) -> Self {
        Self {
            inner,
            pending: Vec::new(),
            aligned: false,
        }
    }

    fn align(&mut self) -> io::Result<()> {
        if self.aligned {
            return Ok(());
        }

        let mut buffer = vec![0; TsPacket::SIZE * 8192];
        loop {
            let read = self.inner.read(&mut buffer)?;
            if read == 0 {
                self.aligned = true;
                return Ok(());
            }

            if let Some(offset) = find_sync_offset(&buffer[..read]) {
                self.pending.extend_from_slice(&buffer[offset..read]);
                self.aligned = true;
                return Ok(());
            }
        }
    }
}

impl<R: Read> Read for AlignedTsReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }

        self.align()?;

        let pending_len = self.pending.len().min(buf.len());
        if pending_len > 0 {
            buf[..pending_len].copy_from_slice(&self.pending[..pending_len]);
            self.pending.drain(..pending_len);

            if pending_len == buf.len() {
                return Ok(pending_len);
            }
        }

        let read = self.inner.read(&mut buf[pending_len..])?;

        Ok(pending_len + read)
    }
}

fn parse_pes_payload(packet: &mut TsPacket) -> anyhow::Result<()> {
    let Some(TsPayload::Raw(payload)) = packet.payload.take() else {
        return Ok(());
    };

    packet.payload = Some(if packet.header.payload_unit_start_indicator {
        TsPayload::PesStart(Pes::read_from(payload.as_ref())?)
    } else {
        TsPayload::PesContinuation(payload)
    });

    Ok(())
}

fn timestamp_to_seconds(timestamp: Timestamp) -> f64 {
    timestamp.as_u64() as f64 / 90_000_f64
}

fn ca_descriptor_pid(descriptor: &Descriptor, ca_system_id: u16) -> anyhow::Result<Option<Pid>> {
    if descriptor.tag != 0x09 || descriptor.data.len() < 4 {
        return Ok(None);
    }

    let descriptor_ca_system_id = u16::from_be_bytes([descriptor.data[0], descriptor.data[1]]);
    if descriptor_ca_system_id != ca_system_id {
        return Ok(None);
    }

    let pid = u16::from_be_bytes([descriptor.data[2] & 0x1f, descriptor.data[3]]);
    if pid == 0 || pid == 0x1fff {
        return Ok(None);
    }

    Ok(Some(Pid::new(pid)?))
}

const B10_SECTION_PIDS: &[u16] = &[
    0x0001, // CAT
    0x0010, // NIT
    0x0011, // SDT, BAT
    0x0012, // EIT
    0x0013, // RST
    0x0014, // TDT, TOT
    0x0020, // LIT
    0x0021, // ERT
    0x0022, // PCAT
    0x0024, // BIT
    0x0025, // NBIT, LDT
    0x0026, // EIT for terrestrial digital TV and multimedia broadcasting
    0x0027, // EIT for terrestrial digital TV and multimedia broadcasting
    0x002E, // AMT
];

fn find_sync_offset(buffer: &[u8]) -> Option<usize> {
    const MIN_SYNC_PACKETS: usize = 3;

    (0..buffer.len()).find(|&offset| {
        (0..MIN_SYNC_PACKETS).all(|index| {
            buffer
                .get(offset + index * TsPacket::SIZE)
                .is_some_and(|byte| *byte == TsPacket::SYNC_BYTE)
        })
    })
}

fn track_type_from_stream_type(stream_type: StreamType) -> Option<TrackType> {
    match stream_type {
        StreamType::Mpeg2Video => Some(TrackType::Mpeg2Video),
        StreamType::H265 => Some(TrackType::H265),
        StreamType::AdtsAac => Some(TrackType::AacAdts),
        StreamType::Mpeg4LoasMultiFormatFramedAudio => Some(TrackType::AacLatm),
        _ => None,
    }
}

fn is_video_track_type(track_type: TrackType) -> bool {
    matches!(track_type, TrackType::Mpeg2Video | TrackType::H265)
}

fn is_audio_track_type(track_type: TrackType) -> bool {
    matches!(track_type, TrackType::AacAdts | TrackType::AacLatm)
}

// Multi-program in a stream won't be needed, I believe.
const PROGRAM_NUM: u16 = 0x0001;

#[inline]
fn pat_pid() -> Pid {
    Pid::new(0x0000).unwrap()
}

#[inline]
fn pmt_pid() -> Pid {
    Pid::new(0x1000).unwrap()
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
                payload_unit_start_indicator: true,
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
                    payload_unit_start_indicator: false,
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
                payload_unit_start_indicator: true,
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
                payload_unit_start_indicator: true,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_sections_keeps_previous_section_tail_before_pointer_field() {
        let pid = Pid::new(0x0012).unwrap();
        let mut buffers = BTreeMap::new();

        assert!(
            read_sections(&mut buffers, pid, true, 0, &[0x4E, 0xB0, 0x05, 0x01, 0x02],).is_empty()
        );

        let sections = read_sections(
            &mut buffers,
            pid,
            true,
            3,
            &[
                0x03, 0x04, 0x05, // End of the previous section.
                0x4F, 0xB0, 0x03, 0x06, 0x07, 0x08,
            ],
        );

        assert_eq!(
            sections,
            vec![
                vec![0x4E, 0xB0, 0x05, 0x01, 0x02, 0x03, 0x04, 0x05],
                vec![0x4F, 0xB0, 0x03, 0x06, 0x07, 0x08],
            ]
        );
    }

    #[test]
    fn read_sections_drains_multiple_sections_from_one_payload() {
        let pid = Pid::new(0x0012).unwrap();
        let mut buffers = BTreeMap::new();

        let sections = read_sections(
            &mut buffers,
            pid,
            true,
            0,
            &[
                0x4E, 0xB0, 0x03, 0x01, 0x02, 0x03, 0x4F, 0xB0, 0x03, 0x04, 0x05, 0x06, 0xFF,
            ],
        );

        assert_eq!(
            sections,
            vec![
                vec![0x4E, 0xB0, 0x03, 0x01, 0x02, 0x03],
                vec![0x4F, 0xB0, 0x03, 0x04, 0x05, 0x06],
            ]
        );
        assert!(buffers.get(&pid).map_or(true, Vec::is_empty));
    }
}
