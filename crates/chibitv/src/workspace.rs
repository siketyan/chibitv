use std::sync::Arc;

use bytes::Bytes;
use tokio_stream::wrappers::BroadcastStream;

use crate::channel::{Channel, ChannelInner};
use crate::event_crawler::EventCrawler;
use crate::registry::Registry;
use crate::service_information::Signal;
use crate::stream::{Stream, Streams, SubscribeError};

pub enum WorkspaceError {
    ChannelNotFound,
    ServiceNotFound,
    TunerBusy,
    StreamingUnavailable,
    Internal(anyhow::Error),
}

pub struct StreamSubscription {
    pub stream: Arc<Stream>,
    pub init_segment: Option<Bytes>,
    pub fmp4: BroadcastStream<Bytes>,
    pub signals: BroadcastStream<Signal>,
}

pub struct Workspace {
    registry: Arc<Registry>,
    channels: Vec<Channel>,
    streams: Option<Streams>,
    event_crawler: Option<Arc<EventCrawler>>,
}

impl Workspace {
    pub fn new(registry: Arc<Registry>, channels: Vec<Channel>, streams: Option<Streams>) -> Self {
        Self {
            registry,
            channels,
            streams,
            event_crawler: None,
        }
    }

    pub fn with_event_crawler(mut self, crawler: EventCrawler) -> Self {
        self.event_crawler = Some(Arc::new(crawler));
        self
    }

    pub fn channels(&self) -> impl Iterator<Item = (usize, &Channel)> {
        self.channels.iter().enumerate()
    }

    pub fn registry(&self) -> &Registry {
        &self.registry
    }

    pub fn registry_arc(&self) -> Arc<Registry> {
        Arc::clone(&self.registry)
    }

    pub fn event_crawler(&self) -> Option<Arc<EventCrawler>> {
        self.event_crawler.clone()
    }

    /// Attaches to the shared stream of the service, tuning to it first when
    /// nobody is streaming it yet.
    pub async fn subscribe_stream(
        &self,
        service_id: u16,
    ) -> Result<StreamSubscription, WorkspaceError> {
        let service = self
            .registry
            .get_service_by_id(service_id)
            .ok_or(WorkspaceError::ServiceNotFound)?;

        let channel = self
            .channels
            .iter()
            .find(|channel| match &channel.inner {
                ChannelInner::IsdbS { stream_id, .. } => {
                    *stream_id == u32::from(service.transport_stream_id)
                }
                ChannelInner::IsdbT { .. }
                | ChannelInner::BonIsdbS { .. }
                | ChannelInner::BonIsdbT { .. } => service.channel_id == channel.id,
            })
            .ok_or(WorkspaceError::ChannelNotFound)?;

        let streams = self
            .streams
            .as_ref()
            .ok_or(WorkspaceError::StreamingUnavailable)?;

        let stream = streams
            .subscribe(service_id, channel)
            .await
            .map_err(|error| match error {
                SubscribeError::TunerBusy => WorkspaceError::TunerBusy,
                SubscribeError::Internal(error) => WorkspaceError::Internal(error),
            })?;

        let (init_segment, fmp4) = stream.subscribe_fmp4();
        let signals = stream.subscribe_signal();

        Ok(StreamSubscription {
            stream,
            init_segment,
            fmp4: BroadcastStream::new(fmp4),
            signals: BroadcastStream::new(signals),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn channel() -> Channel {
        Channel {
            id: 0,
            name: "UHF 20".to_string(),
            inner: ChannelInner::IsdbT {
                frequency: 515_142_857,
                bandwidth_hz: 6_000_000,
            },
        }
    }

    #[tokio::test]
    async fn subscribing_an_unknown_service_fails() {
        let workspace = Workspace::new(Arc::new(Registry::default()), vec![channel()], None);

        let result = workspace.subscribe_stream(0x5678).await;

        assert!(matches!(result, Err(WorkspaceError::ServiceNotFound)));
    }

    #[tokio::test]
    async fn subscribing_without_configured_streams_fails() {
        let registry = Arc::new(Registry::default());
        registry.put_cached_service(0, 0x1234, 0x5678, "Channel".to_string(), String::new());
        let workspace = Workspace::new(registry, vec![channel()], None);

        let result = workspace.subscribe_stream(0x5678).await;

        assert!(matches!(result, Err(WorkspaceError::StreamingUnavailable)));
    }
}
