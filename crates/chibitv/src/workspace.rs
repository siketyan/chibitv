use std::sync::{Arc, RwLock};

use bytes::Bytes;
use tokio_stream::wrappers::BroadcastStream;

use crate::channel::{Channel, ChannelInner};
use crate::registry::{Event, Registry, Service};
use crate::service_information::Signal;
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
        self.get_current_event_with_id(stream_id, None)
    }

    pub fn get_current_event_with_id(
        &self,
        stream_id: u32,
        event_id: Option<u16>,
    ) -> Option<(Option<Service>, Option<Event>)> {
        let streams = self.streams.read().unwrap();
        let stream = streams.get_stream(stream_id)?;

        let service_id = stream.get_service_id();
        let service = service_id.and_then(|service_id| self.registry.get_service_by_id(service_id));

        let event_id = event_id.or_else(|| stream.get_event_id());
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

        let channel = self
            .channels
            .iter()
            .find(|channel| match &channel.inner {
                ChannelInner::IsdbS { stream_id, .. } => {
                    *stream_id == u32::from(service.transport_stream_id)
                }
                ChannelInner::IsdbT { .. } => service.channel_id == channel.id,
            })
            .ok_or_else(|| WorkspaceError::ChannelNotFound)?;

        stream
            .set_channel(service_id, channel)
            .map_err(WorkspaceError::Internal)
    }

    pub fn subscribe_stream(
        &self,
        stream_id: u32,
    ) -> Option<(
        Option<Bytes>,
        BroadcastStream<Bytes>,
        BroadcastStream<Signal>,
    )> {
        let streams = self.streams.read().unwrap();
        let stream = streams.get_stream(stream_id)?;
        let (init_segment, rx) = stream.subscribe_fmp4();
        let signal_rx = stream.subscribe_signal();

        Some((
            init_segment,
            BroadcastStream::new(rx),
            BroadcastStream::new(signal_rx),
        ))
    }
}
