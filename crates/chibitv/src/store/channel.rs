//! The channels chibitv serves, as the store keeps them.
//!
//! A channel used to be named by `config.toml`, which meant editing a file and
//! restarting the server to watch what a scan had found. The store keeps them
//! instead, one broadcast at a time: a scan replaces the channels of the
//! broadcast it walked and leaves the other broadcasts alone.

use async_trait::async_trait;

use crate::channel::{ChannelInner, DeliverySystem};

/// One channel on its way into the store, which is what a scan finds.
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

/// One service of a channel, as a scan described it.
///
/// This is the catalog the registry is seeded with while starting up, so that
/// the services of a channel are known before it has been tuned.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StoredService {
    pub id: u16,
    pub name: String,
    pub provider_name: String,
}

/// The part of a [`super::Store`] the channels are kept in.
#[async_trait]
pub trait ChannelStore: Send + Sync {
    /// Every channel kept, in the order they are served in.
    async fn load_channels(&self) -> anyhow::Result<Vec<StoredChannel>>;

    /// Replaces the channels of one broadcast with the ones given, which are
    /// the channels of that broadcast a scan of it found.
    ///
    /// They are written as a whole, so one carried on that broadcast which the
    /// write does not list goes: a channel that has left the air stops being
    /// served rather than lingering forever. The identifiers are the
    /// database's to give, so the result is read back with
    /// [`ChannelStore::load_channels`] rather than returned here.
    async fn replace_channels(
        &self,
        delivery_system: DeliverySystem,
        channels: &[NewChannel],
    ) -> anyhow::Result<()>;
}
