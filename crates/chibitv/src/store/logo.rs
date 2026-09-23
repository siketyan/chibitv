//! Station logos received from broadcast SI.

use async_trait::async_trait;

use crate::service::ServiceKey;

#[derive(Clone, Debug, PartialEq)]
pub struct StoredLogo {
    pub key: ServiceKey,
    pub png: Vec<u8>,
}

/// The part of a [`super::Store`] the logos of the services are kept in.
#[async_trait]
pub trait LogoRepository: Send + Sync {
    /// Every logo kept, in the order of the keys of their services.
    async fn find_logos(&self) -> anyhow::Result<Vec<StoredLogo>>;

    async fn find_logo(&self, key: ServiceKey) -> anyhow::Result<Option<StoredLogo>>;

    async fn save_logo(&self, logo: &StoredLogo) -> anyhow::Result<()>;
}
