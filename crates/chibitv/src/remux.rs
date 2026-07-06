use std::io::BufRead;
use std::sync::Arc;

use bytes::Bytes;
use tokio::sync::broadcast::Sender;
use tokio::sync::oneshot::Receiver;
use tracing::{error, warn};

use chibitv_b60::message::{M2SectionMessage, Message, PaMessage};
use chibitv_b60::table::{MhBit, MhEit, MhSdt, Table};

use crate::mmt::{MmtDemuxer, Packet, Payload};
use crate::registry::Registry;

#[derive(Clone, Debug)]
#[allow(unused)]
pub enum Signal {
    EventChanged { event_id: u16 },
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum TrackType {
    H265,
    AacLatm,
}

pub trait Mux {
    /// Adds a track to the stream.
    fn add_track(&mut self, track_id: u16, ty: TrackType);

    /// Starts the stream.
    fn begin(&mut self) -> anyhow::Result<()> {
        Ok(())
    }

    /// Writes media data to the stream.
    fn write_sample(
        &mut self,
        track_id: u16,
        data: Bytes,
        dts: Option<f64>,
        pts: Option<f64>,
    ) -> anyhow::Result<()>;

    /// Finalises the stream.
    fn finalize(&mut self) -> anyhow::Result<()> {
        Ok(())
    }
}

pub trait Remux {
    fn run(&mut self, kill_rx: Option<Receiver<()>>) -> anyhow::Result<()>;
}

pub struct Remuxer<R: BufRead, M: Mux> {
    demux: MmtDemuxer<R>,
    mux: M,
    signal_tx: Option<Sender<Signal>>,
    registry: Option<Arc<Registry>>,
    current_event_id: Option<u16>,
}

impl<R: BufRead + Send + Sync, M: Mux> Remux for Remuxer<R, M> {
    fn run(&mut self, mut kill_rx: Option<Receiver<()>>) -> anyhow::Result<()> {
        self.mux.begin()?;

        loop {
            if let Some(kill_rx) = kill_rx.as_mut()
                && kill_rx.try_recv().is_ok()
            {
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

        self.mux.finalize()?;

        Ok(())
    }
}

impl<R: BufRead, M: Mux> Remuxer<R, M> {
    pub fn new(
        demux: MmtDemuxer<R>,
        mux: M,
        signal_tx: Option<Sender<Signal>>,
        registry: Option<Arc<Registry>>,
    ) -> Self {
        Self {
            demux,
            mux,
            signal_tx,
            registry,
            current_event_id: None,
        }
    }

    fn read_packet(&mut self, packet: Packet) -> anyhow::Result<()> {
        match packet.payload {
            Payload::Mfu { dts, pts, data } => {
                self.mux
                    .write_sample(packet.packet_id, Bytes::from(data), dts, pts)?;
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

                        self.mux.add_track(packet_id, TrackType::H265);

                        has_video = true;
                    }
                    b"mp4a" => {
                        if has_audio {
                            // warn!("Multiple audio streams are not supported yet.");
                            continue;
                        }

                        self.mux.add_track(packet_id, TrackType::AacLatm);

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
            if let Some(registry) = &self.registry {
                registry.put_event(table.service_id, event);
            }

            let Some((start_time, duration)) = event.start_time.zip(event.duration) else {
                continue;
            };

            let end_time = start_time + duration;
            let now = chrono::Local::now().naive_local();

            if start_time <= now && now < end_time && self.current_event_id != Some(event.event_id)
            {
                if let Some(signal_tx) = &self.signal_tx {
                    signal_tx.send(Signal::EventChanged {
                        event_id: event.event_id,
                    })?;
                }

                self.current_event_id = Some(event.event_id);
            }
        }

        Ok(())
    }

    fn read_mh_bit(&mut self, table: MhBit) -> anyhow::Result<()> {
        for broadcaster in &table.broadcasters {
            if let Some(registry) = &self.registry {
                registry.put_broadcaster(broadcaster);
            }
        }

        Ok(())
    }

    fn read_mh_sdt(&mut self, table: MhSdt) -> anyhow::Result<()> {
        for service in &table.services {
            if let Some(registry) = &self.registry {
                registry.put_service(table.tlv_stream_id, service);
            }
        }

        Ok(())
    }
}
