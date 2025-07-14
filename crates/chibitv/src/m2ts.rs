use std::cmp::min;
use std::collections::BTreeMap;
use std::sync::RwLock;

use bytes::{Buf, Bytes};
use mpeg2ts::es::StreamId;
use mpeg2ts::pes::PesHeader;
use mpeg2ts::time::Timestamp;
use mpeg2ts::ts::payload::{Pat, Pes, Pmt};
use mpeg2ts::ts::{
    ContinuityCounter, EsInfo, Pid, ProgramAssociation, TransportScramblingControl, TsHeader,
    TsPacket, TsPayload, VersionNumber, WriteTsPacket,
};

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
    streams: BTreeMap<Pid, RwLock<M2tsStream>>,
    last_pat_pmt_ts: Option<f64>,
}

impl<W: WriteTsPacket> M2tsMuxer<W> {
    pub fn new(writer: W) -> Self {
        Self {
            writer,
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
        if let Some(dts) = dts {
            if self.last_pat_pmt_ts.is_none_or(|ts| dts - ts < 0.1_f64) {
                self.emit_pat_pmt()?;
                self.last_pat_pmt_ts = Some(dts);
            }
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
            payload: Some(TsPayload::Pes(Pes {
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
            .iter()
            .filter_map(|(_, stream)| stream.read().ok()?.es_info.as_ref().cloned())
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

    pub fn clear(&mut self) {
        self.streams = default_streams();
        self.last_pat_pmt_ts = None;
    }
}
