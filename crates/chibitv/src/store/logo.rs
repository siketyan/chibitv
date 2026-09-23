//! Station logos received from broadcast SI.
use std::sync::Arc;

use async_trait::async_trait;
use tokio::sync::mpsc;
use tracing::error;

use super::Store;
use crate::registry::ServiceKey;

#[derive(Clone, Debug, PartialEq)]
pub struct StoredLogo {
    pub key: ServiceKey,
    pub png: Vec<u8>,
}

#[async_trait]
pub trait LogoStore: Send + Sync {
    async fn load_logos(&self) -> anyhow::Result<Vec<StoredLogo>>;
    async fn save_logo(&self, logo: &StoredLogo) -> anyhow::Result<()>;
}

#[derive(Clone)]
pub struct LogoWriter {
    tx: mpsc::Sender<StoredLogo>,
}

impl LogoWriter {
    pub fn spawn(store: Arc<dyn Store>) -> Self {
        let (tx, mut rx) = mpsc::channel::<StoredLogo>(64);
        tokio::spawn(async move {
            while let Some(logo) = rx.recv().await {
                if let Err(error) = store.save_logo(&logo).await {
                    error!(?logo.key, %error, "Could not store a station logo");
                }
            }
        });
        Self { tx }
    }

    pub fn enqueue(&self, logo: StoredLogo) -> bool {
        self.tx.try_send(logo).is_ok()
    }
}
