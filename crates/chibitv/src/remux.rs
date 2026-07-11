use bytes::Bytes;
use tracing::error;

use crate::demux::{Demux, MediaPacket, Packet, SignalingEvent, TrackType};

pub trait Mux {
    /// Adds a track to the stream.
    fn add_track(&mut self, track_id: u16, ty: TrackType);

    /// Writes any container data that must precede media samples.
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

pub struct Remuxer<D: Demux, M: Mux> {
    demux: D,
    mux: M,
}

impl<D: Demux, M: Mux> Remuxer<D, M> {
    pub fn new(demux: D, mut mux: M) -> anyhow::Result<Self> {
        mux.begin()?;
        Ok(Self { demux, mux })
    }

    pub fn next(&mut self) -> anyhow::Result<Option<SignalingEvent>> {
        loop {
            let packet = match self.demux.next_packet() {
                Ok(Some(packet)) => packet,
                Ok(None) => return Ok(None),
                Err(error) => {
                    error!(%error, "Failed to read demuxed packet");
                    continue;
                }
            };

            match packet {
                Packet::Media(packet) => self.write_media(packet)?,
                Packet::Signaling(signaling) => return Ok(Some(signaling)),
            }
        }
    }

    pub fn finish(mut self) -> anyhow::Result<()> {
        self.mux.finalize()
    }

    fn write_media(&mut self, packet: MediaPacket) -> anyhow::Result<()> {
        match packet {
            MediaPacket::Track { track_id, ty } => self.mux.add_track(track_id, ty),
            MediaPacket::Sample {
                track_id,
                data,
                dts,
                pts,
            } => self.mux.write_sample(track_id, data, dts, pts)?,
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;

    use chibitv_b10::table::Table as B10Table;

    use super::*;

    struct FakeDemux {
        packets: VecDeque<Packet>,
    }

    impl Demux for FakeDemux {
        fn next_packet(&mut self) -> anyhow::Result<Option<Packet>> {
            Ok(self.packets.pop_front())
        }
    }

    #[derive(Default)]
    struct RecordingMux {
        began: bool,
        finalized: bool,
        tracks: Vec<(u16, TrackType)>,
        samples: Vec<(u16, Bytes)>,
    }

    impl Mux for RecordingMux {
        fn add_track(&mut self, track_id: u16, ty: TrackType) {
            self.tracks.push((track_id, ty));
        }

        fn begin(&mut self) -> anyhow::Result<()> {
            self.began = true;
            Ok(())
        }

        fn write_sample(
            &mut self,
            track_id: u16,
            data: Bytes,
            _dts: Option<f64>,
            _pts: Option<f64>,
        ) -> anyhow::Result<()> {
            self.samples.push((track_id, data));
            Ok(())
        }

        fn finalize(&mut self) -> anyhow::Result<()> {
            self.finalized = true;
            Ok(())
        }
    }

    #[test]
    fn muxes_media_and_returns_signaling_to_the_caller() {
        let signaling = SignalingEvent::B10Table {
            table_id: 0x42,
            table: B10Table::Unknown(0x42, vec![1, 2, 3]),
        };
        let demux = FakeDemux {
            packets: VecDeque::from([
                Packet::Media(MediaPacket::Track {
                    track_id: 100,
                    ty: TrackType::Mpeg2Video,
                }),
                Packet::Media(MediaPacket::Sample {
                    track_id: 100,
                    data: Bytes::from_static(b"sample"),
                    dts: Some(1.0),
                    pts: Some(1.5),
                }),
                Packet::Signaling(signaling),
            ]),
        };
        let mut remuxer = Remuxer::new(demux, RecordingMux::default()).unwrap();

        let returned = remuxer.next().unwrap().unwrap();

        assert!(matches!(
            returned,
            SignalingEvent::B10Table { table_id: 0x42, .. }
        ));
        assert!(remuxer.mux.began);
        assert_eq!(remuxer.mux.tracks, vec![(100, TrackType::Mpeg2Video)]);
        assert_eq!(
            remuxer.mux.samples,
            vec![(100, Bytes::from_static(b"sample"))]
        );
        assert!(!remuxer.mux.finalized);

        assert!(remuxer.next().unwrap().is_none());
        remuxer.finish().unwrap();
    }
}
