//! The services on air, as the store keeps them.
//!
//! A service is kept under the stream carrying it rather than under a channel:
//! a scan writes the catalog of the channels it found, and the SDT keeps it up
//! to date while a stream is tuned. Which channel a service belongs to is not
//! the store's business, as that follows from the channels being served.

use async_trait::async_trait;

use crate::service::{Service, ServiceKey, StoredService};

/// The part of a [`super::Store`] the services are kept in.
#[async_trait]
pub trait ServiceRepository: Send + Sync {
    /// Every service kept, in the order of their keys.
    async fn find_services(&self) -> anyhow::Result<Vec<Service>>;

    async fn find_service(&self, key: ServiceKey) -> anyhow::Result<Option<Service>>;

    /// Keeps the services of a stream, replacing what was kept of each of them
    /// before.
    async fn save_services(&self, stream_id: u16, services: &[StoredService])
    -> anyhow::Result<()>;
}
