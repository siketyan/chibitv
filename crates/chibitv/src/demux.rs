use std::collections::VecDeque;

use bytes::Bytes;

use chibitv_b10::table::Table as B10Table;
use chibitv_b60::message::Message;
use chibitv_b60::tlv_si::Table as TlvTable;

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
    B10Table {
        table_id: u8,
        table: B10Table,
    },
    B60Message(Message),
    /// A table of the TLV-SI, which describes the TLV stream itself rather
    /// than what it carries. It names itself, unlike the SI tables, so there is
    /// no table id beside it.
    TlvTable(TlvTable),
}

#[derive(Clone, Debug)]
pub enum Packet {
    Media(MediaPacket),
    Signaling(SignalingEvent),
}

pub trait Demux {
    fn next_packet(&mut self) -> anyhow::Result<Option<Packet>>;
}

/// Whether the card protecting the stream hands over no key to descramble it
/// with, which is the one error reading on does not get past.
///
/// Both conditional access systems answer that way, and what reads a [`Demux`]
/// does not know which of them is in the way, so the two are asked about
/// together here. A programme with no contract behind it is the ordinary
/// reason, and the card will answer the next ECM the same way: nothing is
/// coming, so whatever wants the picture stops, while whatever wants the
/// tables — a scan, the programme guide — carries on reading them unscrambled.
pub fn is_descrambling_refused(error: &anyhow::Error) -> bool {
    error.is::<chibitv_b25::EcmRefusedError>() || error.is::<chibitv_b61::EcmRefusedError>()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tells_a_card_that_hands_over_no_key_from_any_other_error() {
        assert!(is_descrambling_refused(
            &chibitv_b25::EcmRefusedError {
                return_code: 0x0801
            }
            .into()
        ));
        assert!(is_descrambling_refused(
            &chibitv_b61::EcmRefusedError {
                return_code: 0x0801
            }
            .into()
        ));
        assert!(!is_descrambling_refused(&anyhow::anyhow!("a torn packet")));
    }
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
