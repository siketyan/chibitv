use crate::config::ChannelConfigInner;

/// How a channel is tuned to, and what it delivers once tuned.
///
/// The `Bon*` variants name a channel a BonDriver enumerates rather than
/// tuning parameters, because a BonDriver holds those itself. They still say
/// which delivery system it is, since that is what decides how the stream is
/// demultiplexed: ISDB-T carries MPEG-2 TS and ISDB-S carries MMT/TLV.
#[derive(Clone, Debug)]
pub enum ChannelInner {
    // Only the DVB tuner reads the tuning parameters; a build without it keeps
    // them purely to describe the channel.
    #[cfg_attr(not(feature = "dvb"), allow(dead_code))]
    IsdbS { frequency: u32, stream_id: u32 },
    #[cfg_attr(not(feature = "dvb"), allow(dead_code))]
    IsdbT { frequency: u32, bandwidth_hz: u32 },

    #[cfg_attr(not(feature = "bon"), allow(dead_code))]
    BonIsdbS { space: u32, channel: u32 },
    #[cfg_attr(not(feature = "bon"), allow(dead_code))]
    BonIsdbT { space: u32, channel: u32 },
}

impl From<&ChannelConfigInner> for ChannelInner {
    fn from(value: &ChannelConfigInner) -> Self {
        match value {
            ChannelConfigInner::IsdbS {
                frequency,
                stream_id,
            } => Self::IsdbS {
                frequency: *frequency,
                stream_id: *stream_id,
            },
            ChannelConfigInner::IsdbT {
                frequency,
                bandwidth_hz,
            } => Self::IsdbT {
                frequency: *frequency,
                bandwidth_hz: *bandwidth_hz,
            },
            ChannelConfigInner::BonIsdbS { space, channel } => Self::BonIsdbS {
                space: *space,
                channel: *channel,
            },
            ChannelConfigInner::BonIsdbT { space, channel } => Self::BonIsdbT {
                space: *space,
                channel: *channel,
            },
        }
    }
}

#[derive(Clone, Debug)]
pub struct Channel {
    pub id: usize,
    pub name: String,
    pub inner: ChannelInner,
}
