//! The broadcast schedule, as the store keeps it.
//!
//! The schedule reaches the store one EIT section at a time, and a section is
//! written as a whole: the events it lists replace the ones it listed before,
//! so a programme the broadcaster cancelled disappears instead of lingering
//! forever. Sections whose version did not change never get this far, which is
//! what keeps the number of statements down — see
//! [`crate::service_information`].

use std::sync::Arc;

use anyhow::Context;
use async_trait::async_trait;
use chrono::{NaiveDateTime, TimeDelta};
use tokio::sync::mpsc;
use tracing::{debug, error, info};

use crate::registry;

use super::Store;

/// How many sections wait for the store before one is refused.
const QUEUE_CAPACITY: usize = 256;

/// Identifies the EIT section that delivered a set of events.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct SectionId {
    pub original_network_id: u16,
    /// The TLV stream id on ISDB-S, the transport stream id on ISDB-T.
    pub stream_id: u16,
    pub service_id: u16,
    pub table_id: u8,
    pub section_number: u8,
}

/// One programme of the schedule, as it is kept between runs.
///
/// Events are identified the way the registry identifies them, by the service
/// carrying them and their event id.
#[derive(Clone, Debug, PartialEq)]
pub struct StoredEvent {
    pub service_id: u16,
    pub event_id: u16,
    pub start_time: Option<NaiveDateTime>,
    pub duration: Option<TimeDelta>,
    pub language_code: Option<String>,
    pub name: Option<String>,
    pub text: Option<String>,
    pub description: Vec<Vec<(String, String)>>,
}

impl StoredEvent {
    pub fn of_service(service_id: u16, event: &registry::Event) -> Self {
        Self {
            service_id,
            event_id: event.id,
            start_time: event.start_time,
            duration: event.duration,
            language_code: event.language_code.clone(),
            name: event.name.clone(),
            text: event.text.clone(),
            description: event.description.clone(),
        }
    }
}

impl From<StoredEvent> for registry::Event {
    fn from(value: StoredEvent) -> Self {
        Self {
            id: value.event_id,
            start_time: value.start_time,
            duration: value.duration,
            language_code: value.language_code,
            name: value.name,
            text: value.text,
            description: value.description,
        }
    }
}

/// One section of the schedule on its way to the store.
#[derive(Clone, Debug)]
pub struct SectionUpdate {
    pub section: SectionId,
    pub events: Vec<StoredEvent>,
}

/// The part of a [`Store`] the broadcast schedule is kept in.
#[async_trait]
pub trait EventStore: Send + Sync {
    /// Every event kept, to seed the registry with while starting up.
    async fn load_events(&self) -> anyhow::Result<Vec<StoredEvent>>;

    /// Replaces everything a section delivered with the events it lists now.
    async fn replace_section(
        &self,
        section: SectionId,
        events: &[StoredEvent],
    ) -> anyhow::Result<()>;
}

/// Hands sections to the store from wherever they are demultiplexed.
///
/// The demultiplexers run on their own threads and have a stream to keep up
/// with, so they queue their sections here instead of waiting for a database.
#[derive(Clone)]
pub struct EventWriter {
    tx: mpsc::Sender<SectionUpdate>,
}

impl EventWriter {
    /// Starts writing to the store in the background.
    pub fn spawn(store: Arc<dyn Store>) -> Self {
        let (tx, mut rx) = mpsc::channel::<SectionUpdate>(QUEUE_CAPACITY);

        tokio::spawn(async move {
            while let Some(update) = rx.recv().await {
                let SectionUpdate { section, events } = update;
                match store.replace_section(section, &events).await {
                    Ok(()) => debug!(?section, events = events.len(), "Stored a section"),
                    Err(error) => error!(?section, %error, "Could not store a section"),
                }
            }
        });

        Self { tx }
    }

    /// A writer whose sections the caller receives itself.
    #[cfg(test)]
    pub fn for_test() -> (Self, mpsc::Receiver<SectionUpdate>) {
        let (tx, rx) = mpsc::channel(QUEUE_CAPACITY);

        (Self { tx }, rx)
    }

    /// Queues a section, reporting whether the store took it.
    ///
    /// A queue that is full refuses the section rather than holding up the
    /// demultiplexer behind it. The caller leaves such a section unremembered,
    /// so the next repetition of it — a few seconds away — tries again.
    pub fn enqueue(&self, update: SectionUpdate) -> bool {
        self.tx.try_send(update).is_ok()
    }
}

/// Fills the registry with the schedule of the previous run.
pub async fn restore_events(
    store: &Arc<dyn Store>,
    registry: &registry::Registry,
) -> anyhow::Result<usize> {
    let events = store
        .load_events()
        .await
        .context("Could not read the stored schedule")?;

    let restored = events
        .into_iter()
        .filter(|event| registry.put_loaded_event(event.service_id, event.clone().into()))
        .count();

    info!(restored, "Restored the stored schedule");

    Ok(restored)
}

#[cfg(test)]
mod tests {
    use chrono::NaiveDate;

    use super::*;

    fn stored_event() -> StoredEvent {
        StoredEvent {
            service_id: 0x0400,
            event_id: 0x0001,
            start_time: NaiveDate::from_ymd_opt(2026, 7, 11)
                .unwrap()
                .and_hms_opt(12, 0, 0),
            duration: Some(TimeDelta::minutes(30)),
            language_code: Some("jpn".to_string()),
            name: Some("Programme".to_string()),
            text: Some("Summary".to_string()),
            description: vec![vec![("Cast".to_string(), "Someone".to_string())]],
        }
    }

    #[test]
    fn converts_an_event_to_the_registry_and_back() {
        let event = stored_event();

        let restored =
            StoredEvent::of_service(event.service_id, &registry::Event::from(event.clone()));

        assert_eq!(restored, event);
    }
}
