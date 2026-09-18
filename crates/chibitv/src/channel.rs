//! The channels chibitv serves.
//!
//! A channel is kept in the database rather than in `config.toml`, so that a
//! scan can add to the channels being served without the file being edited and
//! the server restarted. The `[[channels]]` entries a configuration from
//! before that still names are imported once, by [`load_channels`], into a
//! database that has none of its own yet.

use std::fmt::{Display, Formatter};
use std::sync::Arc;

use anyhow::bail;
use tracing::{info, warn};

use crate::config::{ChannelConfig, ChannelConfigInner};
use crate::store::{ChannelScope, NewChannel, Store, StoredChannel};

/// The broadcast a channel is carried on.
///
/// This is what decides how the stream is demultiplexed and which descrambler
/// reads it, so a channel a BonDriver tunes still names it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DeliverySystem {
    /// Terrestrial digital broadcasting, which carries MPEG-2 TS.
    IsdbT,

    /// Satellite 2K (BS/CS) broadcasting, which carries MPEG-2 TS.
    IsdbS,

    /// Satellite 4K (BS/CS) broadcasting, which carries MMT/TLV.
    IsdbS3,
}

impl DeliverySystem {
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
        f.write_str(self.as_str())
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
    // Only the DVB tuner reads the tuning parameters; a build without it keeps
    // them purely to describe the channel.
    #[cfg_attr(not(all(feature = "dvb", unix)), allow(dead_code))]
    IsdbT { frequency: u32, bandwidth_hz: u32 },
    #[cfg_attr(not(all(feature = "dvb", unix)), allow(dead_code))]
    IsdbS { frequency: u32, stream_id: u32 },
    #[cfg_attr(not(all(feature = "dvb", unix)), allow(dead_code))]
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
}

impl From<&ChannelConfigInner> for ChannelInner {
    fn from(value: &ChannelConfigInner) -> Self {
        match value {
            ChannelConfigInner::IsdbT {
                frequency,
                bandwidth_hz,
            } => Self::IsdbT {
                frequency: *frequency,
                bandwidth_hz: *bandwidth_hz,
            },
            ChannelConfigInner::IsdbS {
                frequency,
                stream_id,
            } => Self::IsdbS {
                frequency: *frequency,
                stream_id: *stream_id,
            },
            ChannelConfigInner::IsdbS3 {
                frequency,
                stream_id,
            } => Self::IsdbS3 {
                frequency: *frequency,
                stream_id: *stream_id,
            },
            ChannelConfigInner::BonIsdbT { space, channel } => Self::BonIsdbT {
                space: *space,
                channel: *channel,
            },
            ChannelConfigInner::BonIsdbS { space, channel } => Self::BonIsdbS {
                space: *space,
                channel: *channel,
            },
            ChannelConfigInner::BonIsdbS3 { space, channel } => Self::BonIsdbS3 {
                space: *space,
                channel: *channel,
            },
        }
    }
}

#[derive(Clone, Debug)]
pub struct Channel {
    /// The identifier the database gave the channel, which the API, the
    /// registry and the `--channel` option of the commands name it by.
    pub id: usize,
    pub name: String,
    pub inner: ChannelInner,
}

impl From<&StoredChannel> for Channel {
    fn from(value: &StoredChannel) -> Self {
        Self {
            id: value.id,
            name: value.name.clone(),
            inner: value.inner.clone(),
        }
    }
}

/// The channels to serve, as the database keeps them.
///
/// A database holding no channel yet takes the ones `config.toml` still names,
/// which is how a setup from before the channels moved into the database
/// carries over. Once they are there the file is not read for them again, so a
/// scan is what the channels change with from then on.
pub async fn load_channels(
    store: &Arc<dyn Store>,
    configured: &[ChannelConfig],
) -> anyhow::Result<Vec<StoredChannel>> {
    let stored = store.load_channels().await?;
    if !stored.is_empty() {
        if !configured.is_empty() {
            warn!(
                "The database keeps the channels now, so the `[[channels]]` entries of config.toml are ignored and can be removed"
            );
        }

        return Ok(stored);
    }

    if configured.is_empty() {
        return Ok(stored);
    }

    let imported = configured.iter().map(NewChannel::from).collect::<Vec<_>>();
    store.replace_channels(ChannelScope::All, &imported).await?;

    info!(
        channels = imported.len(),
        "Imported the channels of config.toml into the database"
    );

    store.load_channels().await
}

#[cfg(test)]
mod tests {
    use crate::config::ServiceConfig;
    use crate::store::{SqliteStore, StoredService};

    use super::*;

    fn config_channel(name: &str, frequency: u32) -> ChannelConfig {
        ChannelConfig {
            name: name.to_string(),
            transport_stream_id: Some(0x1234),
            services: vec![ServiceConfig {
                id: 0x0400,
                name: "Service".to_string(),
                provider_name: "Provider".to_string(),
            }],
            inner: ChannelConfigInner::IsdbT {
                frequency,
                bandwidth_hz: 6_000_000,
            },
        }
    }

    async fn store() -> Arc<dyn Store> {
        Arc::new(SqliteStore::open("sqlite::memory:").await.unwrap())
    }

    #[tokio::test]
    async fn imports_the_configured_channels_into_an_empty_database() {
        let store = store().await;
        let configured = [
            config_channel("UHF 20", 515_142_857),
            config_channel("UHF 21", 521_142_857),
        ];

        let channels = load_channels(&store, &configured).await.unwrap();

        assert_eq!(channels.len(), 2);
        assert_eq!(channels[0].name, "UHF 20");
        assert_eq!(
            channels[0].services,
            vec![StoredService {
                id: 0x0400,
                name: "Service".to_string(),
                provider_name: "Provider".to_string(),
            }]
        );
        assert!(channels[0].id < channels[1].id);
    }

    #[tokio::test]
    async fn leaves_the_stored_channels_alone_once_they_are_there() {
        let store = store().await;
        load_channels(&store, &[config_channel("UHF 20", 515_142_857)])
            .await
            .unwrap();

        let channels = load_channels(&store, &[config_channel("UHF 21", 521_142_857)])
            .await
            .unwrap();

        assert_eq!(channels.len(), 1);
        assert_eq!(channels[0].name, "UHF 20");
    }

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
}
