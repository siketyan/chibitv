//! The broadcast schedule, as the store keeps it.
//!
//! The schedule reaches the store one EIT section at a time. A section of the
//! schedule is written as a whole: the events it lists replace the ones it
//! listed before, so a programme the broadcaster cancelled disappears instead
//! of lingering forever. Sections whose version did not change never get this
//! far, which is what keeps the number of statements down — see
//! [`crate::service_information`].

use async_trait::async_trait;
use chrono::NaiveDateTime;

use crate::event::Event;
use crate::service::ServiceKey;

/// Identifies the EIT section that delivered a set of events.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct SectionId {
    pub original_network_id: u16,
    /// The TLV stream id on ISDB-S3, the transport stream id on ISDB-T and
    /// ISDB-S.
    pub stream_id: u16,
    pub service_id: u16,
    pub table_id: u8,
    pub section_number: u8,
}

impl SectionId {
    /// The service the section carries the schedule of.
    pub fn service(&self) -> ServiceKey {
        ServiceKey {
            stream_id: self.stream_id,
            service_id: self.service_id,
        }
    }
}

/// The part of a [`super::Store`] the broadcast schedule is kept in.
#[async_trait]
pub trait EventRepository: Send + Sync {
    /// Every event of the service, in the order they start in.
    async fn find_events(&self, key: ServiceKey) -> anyhow::Result<Vec<Event>>;

    async fn find_event(&self, key: ServiceKey, event_id: u16) -> anyhow::Result<Option<Event>>;

    /// The event of the service on air at the time, which is the one that
    /// started last where two announced overlap.
    async fn find_event_on_air(
        &self,
        key: ServiceKey,
        at: NaiveDateTime,
    ) -> anyhow::Result<Option<Event>>;

    /// Replaces everything a section delivered with the events it lists now.
    async fn replace_section(&self, section: SectionId, events: &[Event]) -> anyhow::Result<()>;

    /// Keeps the events a section lists, leaving whatever else it delivered.
    ///
    /// This is for the present and following events: the section describing
    /// them moves on to the next programme as soon as one ends, and replacing
    /// it would take the one that just ended out of the schedule.
    async fn save_events(&self, section: SectionId, events: &[Event]) -> anyhow::Result<()>;
}
