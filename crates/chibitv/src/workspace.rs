use std::sync::{Arc, RwLock};

use bytes::Bytes;
use tokio_stream::wrappers::BroadcastStream;

use crate::channel::{Channel, ChannelInner};
use crate::registry::{Event, Registry, Service};
use crate::stream::Streams;

pub enum WorkspaceError {
    ChannelNotFound,
    ServiceNotFound,
    StreamNotFound,
    Internal(anyhow::Error),
}

pub struct Workspace {
    registry: Arc<Registry>,
    channels: Vec<Channel>,
    streams: RwLock<Streams>,
}

impl Workspace {
    pub fn new(registry: Arc<Registry>, channels: Vec<Channel>, streams: RwLock<Streams>) -> Self {
        Self {
            registry,
            channels,
            streams,
        }
    }

    pub fn channels(&self) -> impl Iterator<Item = (usize, &Channel)> {
        self.channels.iter().enumerate()
    }

    pub fn registry(&self) -> &Registry {
        &self.registry
    }

    pub fn get_current_event(&self, stream_id: u32) -> Option<(Option<Service>, Option<Event>)> {
        let streams = self.streams.read().unwrap();
        let stream = streams.get_stream(stream_id)?;

        let service_id = stream.get_service_id();
        let service = service_id.and_then(|service_id| self.registry.get_service_by_id(service_id));

        let event_id = stream.get_event_id();
        let event = service_id
            .zip(event_id)
            .and_then(|(service_id, event_id)| self.registry.get_event_by_id(service_id, event_id));

        Some((service, event))
    }

    pub fn set_channel(&self, stream_id: u32, service_id: u16) -> Result<(), WorkspaceError> {
        let streams = self.streams.read().unwrap();
        let Some(stream) = streams.get_stream(stream_id) else {
            return Err(WorkspaceError::StreamNotFound);
        };

        let service = self
            .registry
            .get_service_by_id(service_id)
            .ok_or_else(|| WorkspaceError::ServiceNotFound)?;

        let channel = self.channels
                .iter()
                .find(|channel| matches!(&channel.inner, ChannelInner::IsdbS {stream_id, ..} if *stream_id == (service.tlv_stream_id as u32)))
            .ok_or_else(|| WorkspaceError::ChannelNotFound)?;

        stream
            .set_channel(service_id, channel)
            .map_err(WorkspaceError::Internal)
    }

    pub fn get_m2ts_stream(&self, stream_id: u32) -> Option<BroadcastStream<Bytes>> {
        let streams = self.streams.read().unwrap();
        let stream = streams.get_stream(stream_id)?;
        let rx = stream.subscribe();

        Some(BroadcastStream::new(rx))
    }
}
