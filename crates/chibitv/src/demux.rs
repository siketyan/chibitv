use std::collections::VecDeque;

use bytes::Bytes;

use chibitv_b10::table::Table as B10Table;
use chibitv_b60::message::Message;

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum TrackType {
    Mpeg2Video,
    AacAdts,
    H265,
    AacLatm,
}

#[derive(Clone, Debug)]
pub enum MediaPacket {
    Track {
        track_id: u16,
        ty: TrackType,
    },
    Sample {
        track_id: u16,
        data: Bytes,
        dts: Option<f64>,
        pts: Option<f64>,
    },
}

#[derive(Clone, Debug)]
pub enum SignalingEvent {
    B10Table { table_id: u8, table: B10Table },
    B60Message(Message),
}

#[derive(Clone, Debug)]
pub enum Packet {
    Media(MediaPacket),
    Signaling(SignalingEvent),
}

pub trait Demux {
    fn next_packet(&mut self) -> anyhow::Result<Option<Packet>>;
}

#[derive(Debug, Default)]
pub struct PacketQueue {
    packets: VecDeque<Packet>,
}

impl PacketQueue {
    pub fn pop(&mut self) -> Option<Packet> {
        self.packets.pop_front()
    }

    pub fn extend(&mut self, packets: impl IntoIterator<Item = Packet>) {
        self.packets.extend(packets);
    }
}
