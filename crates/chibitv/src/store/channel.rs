//! The channels chibitv serves, as the store keeps them.
//!
//! A channel used to be named by `config.toml`, which meant editing a file and
//! restarting the server to watch what a scan had found. The store keeps them
//! instead, and a scan replaces the channels of the broadcast it walked —
//! [`ChannelScope`] is what says so — leaving the other broadcasts alone.

use async_trait::async_trait;

use crate::channel::{ChannelInner, DeliverySystem};
use crate::config::ChannelConfig;

/// One channel on its way into the store.
///
/// The identifier is the database's to give, so a channel only carries one
/// once it has been read back as a [`StoredChannel`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NewChannel {
    pub name: String,
    /// How the channel is tuned to, and what it delivers once tuned.
    pub inner: ChannelInner,
    /// The stream its services are named under, which the MPEG-2 TS channels
    /// need before anything of them is tuned.
    pub transport_stream_id: Option<u16>,
    pub services: Vec<StoredService>,
}

impl From<&ChannelConfig> for NewChannel {
    fn from(value: &ChannelConfig) -> Self {
        Self {
            name: value.name.clone(),
            inner: (&value.inner).into(),
            transport_stream_id: value.transport_stream_id,
            services: value.services.iter().map(StoredService::from).collect(),
        }
    }
}

/// One channel as the store keeps it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StoredChannel {
    /// The identifier the database gave the channel, which the API, the
    /// registry and the `--channel` option of the commands name it by.
    pub id: usize,
    pub name: String,
    pub inner: ChannelInner,
    pub transport_stream_id: Option<u16>,
    pub services: Vec<StoredService>,
}

impl StoredChannel {
    /// The stream the channel carries, when it is known.
    ///
    /// A satellite channel is picked out of its transponder by its stream, so
    /// the tuning parameters name it; a terrestrial one carries whichever
    /// transport stream a scan found on it, and nothing but a scan can say
    /// which that is.
    pub fn stream_id(&self) -> Option<u16> {
        self.transport_stream_id.or(match self.inner {
            ChannelInner::IsdbS { stream_id, .. } | ChannelInner::IsdbS3 { stream_id, .. } => {
                u16::try_from(stream_id).ok()
            }
            ChannelInner::IsdbT { .. }
            | ChannelInner::BonIsdbT { .. }
            | ChannelInner::BonIsdbS { .. }
            | ChannelInner::BonIsdbS3 { .. } => None,
        })
    }
}

/// One service of a channel, as a scan or the configuration described it.
///
/// This is the catalog the registry is seeded with while starting up, so that
/// the services of a channel are known before it has been tuned.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StoredService {
    pub id: u16,
    pub name: String,
    pub provider_name: String,
}

impl From<&crate::config::ServiceConfig> for StoredService {
    fn from(value: &crate::config::ServiceConfig) -> Self {
        Self {
            id: value.id,
            name: value.name.clone(),
            provider_name: value.provider_name.clone(),
        }
    }
}

/// Which of the channels kept a write replaces.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ChannelScope {
    /// Every channel, which is what importing a whole configuration replaces.
    All,

    /// The channels carried on one broadcast, which is what a scan of that
    /// broadcast replaces while leaving the others alone.
    DeliverySystem(DeliverySystem),
}

/// The part of a [`super::Store`] the channels are kept in.
#[async_trait]
pub trait ChannelStore: Send + Sync {
    /// Every channel kept, in the order they are served in.
    async fn load_channels(&self) -> anyhow::Result<Vec<StoredChannel>>;

    /// Replaces the channels within the scope with the ones given.
    ///
    /// The channels are written as a whole, so one the scope covers and the
    /// write does not list goes: a channel that has left the air stops being
    /// served rather than lingering forever. The identifiers are the
    /// database's to give, so the result is read back with
    /// [`ChannelStore::load_channels`] rather than returned here.
    async fn replace_channels(
        &self,
        scope: ChannelScope,
        channels: &[NewChannel],
    ) -> anyhow::Result<()>;
}
