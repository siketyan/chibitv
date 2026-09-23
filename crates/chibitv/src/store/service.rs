//! The services of the channels chibitv serves, as the store keeps them.
//!
//! A service is kept under the stream carrying it rather than under a channel:
//! a scan writes the catalog of the channels it found, the SDT keeps it up to
//! date while a stream is tuned, and the channel a service belongs to is the
//! one carrying its stream when it is read back. A service of a stream no
//! channel carries — one the signalling of the network describes beside the
//! stream being tuned, or one of a channel no longer served — is not read back
//! at all, as it cannot be watched or recorded.

use async_trait::async_trait;

use crate::service::{Service, ServiceKey, StoredService};

/// The part of a [`super::Store`] the services are kept in.
#[async_trait]
pub trait ServiceRepository: Send + Sync {
    /// Every service of the channels being served, in the order of their keys.
    async fn find_services(&self) -> anyhow::Result<Vec<Service>>;

    /// The service of the key, when a channel being served carries it.
    async fn find_service(&self, key: ServiceKey) -> anyhow::Result<Option<Service>>;

    /// Keeps the services of a stream, replacing what was kept of each of them
    /// before.
    async fn save_services(&self, stream_id: u16, services: &[StoredService])
    -> anyhow::Result<()>;
}
