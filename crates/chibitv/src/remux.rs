use std::collections::BTreeMap;
use std::io::BufRead;
use std::sync::Arc;

use bytes::Bytes;
use mpeg2ts::es::{StreamId, StreamType};
use mpeg2ts::ts::{Descriptor as TsDescriptor, EsInfo, Pid, WriteTsPacket};
use tokio::sync::broadcast::Sender;
use tokio::sync::oneshot::Receiver;
use tracing::{error, info, warn};

use chibitv_b60::message::{M2SectionMessage, Message, PaMessage};
use chibitv_b60::table::{MhBit, MhEit, MhSdt, Table};

use crate::m2ts::M2tsMuxer;
use crate::mmt::{MmtDemuxer, Packet, Payload};
use crate::registry::Registry;

#[derive(Clone, Debug)]
#[allow(unused)]
pub enum Signal {
    EventChanged { event_id: u16 },
}

pub trait Remux: Send + Sync {
    fn run(&mut self, kill_rx: Receiver<()>) -> anyhow::Result<()>;

    fn clear(&mut self);
}

pub struct Remuxer<R: BufRead, W: WriteTsPacket> {
    demux: MmtDemuxer<R>,
    mux: M2tsMuxer<W>,
    signal_tx: Sender<Signal>,
    registry: Arc<Registry>,
    map: BTreeMap<u16, Pid>,
    current_event_id: Option<u16>,
}

impl<R: BufRead + Send + Sync, W: WriteTsPacket + Send + Sync> Remux for Remuxer<R, W> {
    fn run(&mut self, mut kill_rx: Receiver<()>) -> anyhow::Result<()> {
        loop {
            if kill_rx.try_recv().is_ok() {
                break;
            }

            let packets = match self.demux.read() {
                Ok(Some(packets)) => packets,
                Ok(None) => {
                    // No more data.
                    break;
                }
                Err(e) => {
                    error!("{}", e);
                    continue;
                }
            };

            for packet in packets {
                self.read_packet(packet)?;
            }
        }

        Ok(())
    }

    fn clear(&mut self) {
        self.demux.clear();
        self.mux.clear();
        self.map.clear();
        self.current_event_id = None;
    }
}

impl<R: BufRead, W: WriteTsPacket> Remuxer<R, W> {
    pub fn new(
        demux: MmtDemuxer<R>,
        mux: M2tsMuxer<W>,
        signal_tx: Sender<Signal>,
        registry: Arc<Registry>,
    ) -> Self {
        Self {
            demux,
            mux,
            signal_tx,
            registry,
            map: BTreeMap::new(),
            current_event_id: None,
        }
    }

    fn read_packet(&mut self, packet: Packet) -> anyhow::Result<()> {
        match packet.payload {
            Payload::Mfu { dts, pts, data } => {
                let Some(pid) = self.map.get(&packet.packet_id).copied() else {
                    // The stream is not yet added, or unrecognisable.
                    return Ok(());
                };

                self.mux.write_pes(pid, Bytes::from(data), dts, pts)?;
            }
            Payload::Message(message) => match message {
                Message::Pa(message) => self.read_pa_message(message),
                Message::M2Section(message) => self.read_m2_section_message(message)?,
                _ => {}
            },
        }

        Ok(())
    }

    fn read_pa_message(&mut self, message: PaMessage) {
        for table in &message.tables {
            let Table::Mpt(table) = table else {
                continue;
            };

            // Already added streams.
            // TODO: Compare the streams and handle changes?
            if !self.map.is_empty() {
                return;
            }

            let mut has_video = false;
            let mut has_audio = false;

            for asset in &table.assets {
                let packet_id = asset.locations.last().unwrap().packet_id().unwrap();

                match &asset.asset_type {
                    b"hev1" => {
                        if has_video {
                            warn!("Multiple video streams are not supported yet.");
                            continue;
                        }

                        let pid = Pid::new(0x1011).unwrap();

                        self.map.insert(packet_id, pid);
                        self.mux.add_stream(
                            pid,
                            StreamId::new_video(0xe0).unwrap(),
                            EsInfo {
                                elementary_pid: pid,
                                stream_type: StreamType::H265,
                                descriptors: vec![TsDescriptor {
                                    tag: 0x05,
                                    data: b"HEVC".to_vec(),
                                }],
                            },
                        );

                        info!(packet_id, pid = pid.as_u16(), "Added a HEVC video stream");

                        has_video = true;
                    }
                    b"mp4a" => {
                        if has_audio {
                            warn!("Multiple audio streams are not supported yet.");
                            continue;
                        }

                        let pid = Pid::new(0x1100).unwrap();

                        self.map.insert(packet_id, pid);
                        self.mux.add_stream(
                            pid,
                            StreamId::new_audio(0xc0).unwrap(),
                            EsInfo {
                                elementary_pid: pid,
                                stream_type: StreamType::Mpeg4LoasMultiFormatFramedAudio, // AAC-LATM
                                descriptors: vec![],
                            },
                        );

                        info!(packet_id, pid = pid.as_u16(), "Added an AAC video stream");

                        has_audio = true;
                    }
                    _ => {}
                }
            }
        }
    }

    fn read_m2_section_message(&mut self, message: M2SectionMessage) -> anyhow::Result<()> {
        match message.table {
            Table::MhEit(table) => self.read_mh_eit(table),
            Table::MhBit(table) => self.read_mh_bit(table),
            Table::MhSdt(table) => self.read_mh_sdt(table),
            _ => Ok(()),
        }
    }

    fn read_mh_eit(&mut self, table: MhEit) -> anyhow::Result<()> {
        for event in &table.events {
            self.registry.put_event(table.service_id, event);

            let Some((start_time, duration)) = event.start_time.zip(event.duration) else {
                continue;
            };

            let end_time = start_time + duration;
            let now = chrono::Local::now().naive_local();

            if start_time <= now && now < end_time && self.current_event_id != Some(event.event_id)
            {
                self.signal_tx.send(Signal::EventChanged {
                    event_id: event.event_id,
                })?;

                self.current_event_id = Some(event.event_id);
            }
        }

        Ok(())
    }

    fn read_mh_bit(&mut self, table: MhBit) -> anyhow::Result<()> {
        for broadcaster in &table.broadcasters {
            self.registry.put_broadcaster(broadcaster);
        }

        Ok(())
    }

    fn read_mh_sdt(&mut self, table: MhSdt) -> anyhow::Result<()> {
        for service in &table.services {
            self.registry.put_service(table.tlv_stream_id, service);
        }

        Ok(())
    }
}
