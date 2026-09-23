//! The channels chibitv serves.
//!
//! A channel is kept in the database rather than in the configuration, so that
//! a scan can add to the channels being served without a file being edited and
//! the server restarted. [`find_channel`] is how a command names one of them,
//! by the identifier the database gave it.

use std::fmt::{Display, Formatter, Write as _};

use anyhow::bail;
use serde::Deserialize;

use crate::config::Config;
use crate::store::StoredChannel;

/// The broadcast a channel is carried on.
///
/// This is what decides how the stream is demultiplexed and which descrambler
/// reads it, so a channel a BonDriver tunes still names it. It is also what a
/// tuner is picked for, as not every tuner receives every broadcast.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd)]
pub enum DeliverySystem {
    /// Terrestrial digital broadcasting, which carries MPEG-2 TS.
    #[serde(rename = "ISDB-T")]
    IsdbT,

    /// Satellite 2K (BS/CS) broadcasting, which carries MPEG-2 TS.
    #[serde(rename = "ISDB-S")]
    IsdbS,

    /// Satellite 4K (BS/CS) broadcasting, which carries MMT/TLV.
    #[serde(rename = "ISDB-S3")]
    IsdbS3,
}

impl DeliverySystem {
    /// Every broadcast there is.
    #[cfg(test)]
    pub const ALL: [Self; 3] = [Self::IsdbT, Self::IsdbS, Self::IsdbS3];

    /// The name the configuration and the database call it by.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::IsdbT => "ISDB-T",
            Self::IsdbS => "ISDB-S",
            Self::IsdbS3 => "ISDB-S3",
        }
    }

    /// Reads back what [`DeliverySystem::as_str`] wrote.
    pub fn parse(value: &str) -> anyhow::Result<Self> {
        match value {
            "ISDB-T" => Ok(Self::IsdbT),
            "ISDB-S" => Ok(Self::IsdbS),
            "ISDB-S3" => Ok(Self::IsdbS3),
            _ => bail!("`{value}` is not a delivery system chibitv knows"),
        }
    }
}

impl Display for DeliverySystem {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        // Padded rather than written, so that a listing can align a column of
        // delivery systems with a width.
        f.pad(self.as_str())
    }
}

/// How a channel is tuned to, and what it delivers once tuned.
///
/// The `Bon*` variants name a channel a BonDriver enumerates rather than
/// tuning parameters, because a BonDriver holds those itself. They still say
/// which delivery system it is, since that is what decides how the stream is
/// demultiplexed: ISDB-T and ISDB-S carry MPEG-2 TS while ISDB-S3, the 4K
/// satellite system, carries MMT/TLV.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ChannelInner {
    // Only the DVB and px4_drv tuners, both Linux ones, read the tuning
    // parameters; a build without them keeps them purely to describe the
    // channel.
    #[cfg_attr(
        not(all(any(feature = "dvb", feature = "px4"), target_os = "linux")),
        allow(dead_code)
    )]
    IsdbT { frequency: u32, bandwidth_hz: u32 },
    #[cfg_attr(
        not(all(any(feature = "dvb", feature = "px4"), target_os = "linux")),
        allow(dead_code)
    )]
    IsdbS { frequency: u32, stream_id: u32 },
    #[cfg_attr(
        not(all(any(feature = "dvb", feature = "px4"), target_os = "linux")),
        allow(dead_code)
    )]
    IsdbS3 { frequency: u32, stream_id: u32 },

    #[cfg_attr(not(all(feature = "bon", windows)), allow(dead_code))]
    BonIsdbT { space: u32, channel: u32 },
    #[cfg_attr(not(all(feature = "bon", windows)), allow(dead_code))]
    BonIsdbS { space: u32, channel: u32 },
    #[cfg_attr(not(all(feature = "bon", windows)), allow(dead_code))]
    BonIsdbS3 { space: u32, channel: u32 },
}

impl ChannelInner {
    /// The broadcast the channel is carried on.
    pub fn delivery_system(&self) -> DeliverySystem {
        match self {
            Self::IsdbT { .. } | Self::BonIsdbT { .. } => DeliverySystem::IsdbT,
            Self::IsdbS { .. } | Self::BonIsdbS { .. } => DeliverySystem::IsdbS,
            Self::IsdbS3 { .. } | Self::BonIsdbS3 { .. } => DeliverySystem::IsdbS3,
        }
    }

    /// The stream the tuning picks out of a transponder, which only a
    /// satellite channel tuned by its parameters names.
    pub fn stream_id(&self) -> Option<u16> {
        match *self {
            Self::IsdbS { stream_id, .. } | Self::IsdbS3 { stream_id, .. } => {
                u16::try_from(stream_id).ok()
            }
            Self::IsdbT { .. }
            | Self::BonIsdbT { .. }
            | Self::BonIsdbS { .. }
            | Self::BonIsdbS3 { .. } => None,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Channel {
    /// The identifier the database gave the channel, which the API, the
    /// services and the `--channel` option of the commands name it by.
    pub id: usize,
    pub name: String,
    pub inner: ChannelInner,
    /// The stream the channel carries, when it is known, which is what says
    /// the services of that stream are this channel's.
    pub stream_id: Option<u16>,
}

impl From<&StoredChannel> for Channel {
    fn from(value: &StoredChannel) -> Self {
        Self {
            id: value.id,
            name: value.name.clone(),
            inner: value.inner.clone(),
            stream_id: value.stream_id(),
        }
    }
}

/// The channel of the identifier, out of the channels the database keeps.
///
/// This is what the `--channel` option of the commands names, and the
/// `channels` command is what lists the identifiers to choose from.
pub async fn find_channel(config: &Config, id: usize) -> anyhow::Result<Channel> {
    let store = crate::store::open(&config.database.url).await?;
    let channels = store.load_channels().await?;

    let Some(channel) = channels.iter().find(|channel| channel.id == id) else {
        bail!(
            "Could not find the channel {id} in the database; `chibitv channels` lists the ones it keeps"
        );
    };

    Ok(channel.into())
}

/// The channels as the `channels` command and a saved scan list them, one line
/// per channel and one per service of it.
pub fn format_channel_list(channels: &[StoredChannel]) -> String {
    let mut output = String::new();
    for channel in channels {
        let _ = writeln!(
            output,
            "{:>4}  {:<7}  {}",
            channel.id,
            channel.inner.delivery_system(),
            channel.name,
        );

        for service in &channel.services {
            let _ = writeln!(output, "      {:>5}  {}", service.id, service.name);
        }
    }

    output
}

#[cfg(test)]
mod tests {
    use crate::store::StoredService;

    use super::*;

    #[test]
    fn names_the_delivery_system_of_a_bondriver_channel() {
        assert_eq!(
            ChannelInner::BonIsdbS3 {
                space: 2,
                channel: 0,
            }
            .delivery_system(),
            DeliverySystem::IsdbS3,
        );
        assert_eq!(DeliverySystem::IsdbS3.to_string(), "ISDB-S3");
        assert_eq!(
            DeliverySystem::parse("ISDB-S").unwrap(),
            DeliverySystem::IsdbS,
        );
        assert!(DeliverySystem::parse("ISDB-C").is_err());
    }

    #[test]
    fn reads_a_delivery_system_from_the_configuration_by_the_same_name() {
        for system in DeliverySystem::ALL {
            let configured: DeliverySystem =
                toml::from_str::<toml::Value>(&format!("system = \"{system}\""))
                    .unwrap()
                    .get("system")
                    .cloned()
                    .unwrap()
                    .try_into()
                    .unwrap();
            assert_eq!(configured, system);
        }
    }

    #[test]
    fn lists_a_channel_with_the_services_of_it() {
        let listed = format_channel_list(&[StoredChannel {
            id: 3,
            name: "TOKYO MX".to_string(),
            inner: ChannelInner::IsdbT {
                frequency: 515_142_857,
                bandwidth_hz: 6_000_000,
            },
            transport_stream_id: Some(0x1234),
            services: vec![StoredService {
                id: 23608,
                name: "TOKYO MX1".to_string(),
                provider_name: "TOKYO MX".to_string(),
            }],
        }]);

        assert_eq!(listed, "   3  ISDB-T   TOKYO MX\n      23608  TOKYO MX1\n",);
    }
}
