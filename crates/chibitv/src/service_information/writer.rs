//! Writes what the SI says of the services and their schedule to the store.
//!
//! The demultiplexers run on their own threads and have a stream to keep up
//! with, so they queue the tables here instead of waiting for a database, and
//! one task writes them in the order they were queued in.

use std::sync::Arc;

use tokio::sync::mpsc;
use tracing::{debug, error};

use chibitv_b10::table::EventInformation as B10EventInformation;
use chibitv_b60::table::EventInformation as B60EventInformation;

use crate::event::Event;
use crate::service::StoredService;
use crate::store::{SectionId, Store};

/// How many updates wait for the store before one is refused.
const QUEUE_CAPACITY: usize = 256;

/// The events of one EIT section, as the table carrying them lists them.
pub enum EventEntries {
    B10(Vec<B10EventInformation>),
    B60(Vec<B60EventInformation>),
}

/// One update on its way to the store.
pub enum ServiceInformationUpdate {
    /// The services an SDT describes, under the stream carrying them.
    Services {
        stream_id: u16,
        services: Vec<StoredService>,
    },
    /// The events an EIT section describes.
    Events {
        section: SectionId,
        /// Whether the section replaces everything it delivered before, which
        /// a section of the schedule does.
        replaces: bool,
        entries: EventEntries,
    },
    /// Runs once every update queued before it is written.
    Notify(Box<dyn FnOnce() + Send>),
}

#[derive(Clone)]
pub struct ServiceInformationWriter {
    tx: mpsc::Sender<ServiceInformationUpdate>,
}

impl ServiceInformationWriter {
    /// Starts writing to the store in the background.
    pub fn spawn(store: Arc<dyn Store>) -> Self {
        let (tx, mut rx) = mpsc::channel(QUEUE_CAPACITY);

        tokio::spawn(async move {
            while let Some(update) = rx.recv().await {
                if let Err(error) = write(store.as_ref(), update).await {
                    error!(%error, "Could not store the service information");
                }
            }
        });

        Self { tx }
    }

    /// A writer whose updates the caller receives itself.
    #[cfg(test)]
    pub fn for_test() -> (Self, mpsc::Receiver<ServiceInformationUpdate>) {
        let (tx, rx) = mpsc::channel(QUEUE_CAPACITY);

        (Self { tx }, rx)
    }

    /// Queues an update, reporting whether the store took it.
    ///
    /// A queue that is full refuses the update rather than holding up the
    /// demultiplexer behind it. The caller leaves the section it came from
    /// unremembered, so the next repetition of it — a few seconds away —
    /// tries again.
    pub fn enqueue(&self, update: ServiceInformationUpdate) -> bool {
        self.tx.try_send(update).is_ok()
    }

    /// Runs `f` once everything queued so far is written, so that whoever it
    /// tells finds it in the store.
    ///
    /// A queue that is full runs it at once instead, as a notice arriving a
    /// little early is better than none at all.
    pub fn notify(&self, f: impl FnOnce() + Send + 'static) {
        if let Err(error) = self
            .tx
            .try_send(ServiceInformationUpdate::Notify(Box::new(f)))
            && let ServiceInformationUpdate::Notify(f) = error.into_inner()
        {
            f();
        }
    }
}

async fn write(store: &dyn Store, update: ServiceInformationUpdate) -> anyhow::Result<()> {
    match update {
        ServiceInformationUpdate::Services {
            stream_id,
            services,
        } => {
            store.save_services(stream_id, &services).await?;
            debug!(stream_id, services = services.len(), "Stored the services");
        }
        ServiceInformationUpdate::Events {
            section,
            replaces,
            entries,
        } => {
            let events = match entries {
                EventEntries::B10(entries) => {
                    merge(
                        store,
                        section,
                        &entries,
                        |entry| entry.event_id,
                        Event::apply_b10,
                    )
                    .await?
                }
                EventEntries::B60(entries) => {
                    merge(
                        store,
                        section,
                        &entries,
                        |entry| entry.event_id,
                        Event::apply_b60,
                    )
                    .await?
                }
            };

            if replaces {
                store.replace_section(section, &events).await?;
            } else {
                store.save_events(section, &events).await?;
            }
            debug!(?section, events = events.len(), "Stored a section");
        }
        ServiceInformationUpdate::Notify(f) => f(),
    }

    Ok(())
}

/// The events of a section, each assembled out of what was kept of it before
/// and what the section says of it now.
///
/// An event is described across several sections — the schedule names it in
/// one table and details it in another — so one section alone does not
/// describe all of it.
async fn merge<T>(
    store: &dyn Store,
    section: SectionId,
    entries: &[T],
    event_id: impl Fn(&T) -> u16,
    apply: impl Fn(&mut Event, &T),
) -> anyhow::Result<Vec<Event>> {
    let key = section.service();
    let mut events = Vec::with_capacity(entries.len());
    for entry in entries {
        let id = event_id(entry);
        let mut event = store
            .find_event(key, id)
            .await?
            .unwrap_or_else(|| Event::new(key, id));
        apply(&mut event, entry);
        events.push(event);
    }

    Ok(events)
}

#[cfg(test)]
mod tests {
    use chibitv_b10::descriptor::{
        Descriptor as B10Descriptor, ExtendedEventDescriptor, ExtendedEventItem,
        ShortEventDescriptor,
    };

    use super::*;

    const SECTION: SectionId = SectionId {
        original_network_id: 1,
        stream_id: 0x1234,
        service_id: 0x5678,
        table_id: 0x50,
        section_number: 0,
    };

    fn entry(descriptor: B10Descriptor) -> EventEntries {
        EventEntries::B10(vec![B10EventInformation {
            event_id: 0x0001,
            start_time: None,
            duration: None,
            running_status: 4,
            free_ca_mode: false,
            descriptors: vec![descriptor],
        }])
    }

    #[tokio::test]
    async fn assembles_an_event_described_across_sections() {
        let store = crate::store::open("sqlite::memory:").await.unwrap();
        let writer = ServiceInformationWriter::spawn(Arc::clone(&store));

        // The basic schedule names the event, and the extended one details it
        // in a section of its own.
        writer.enqueue(ServiceInformationUpdate::Events {
            section: SECTION,
            replaces: true,
            entries: entry(B10Descriptor::ShortEvent(ShortEventDescriptor {
                iso_639_language_code: *b"jpn",
                event_name: b"\x0eProgramme".to_vec(),
                text: vec![],
            })),
        });
        writer.enqueue(ServiceInformationUpdate::Events {
            section: SectionId {
                table_id: 0x58,
                ..SECTION
            },
            replaces: true,
            entries: entry(B10Descriptor::ExtendedEvent(ExtendedEventDescriptor {
                descriptor_number: 0,
                last_descriptor_number: 0,
                iso_639_language_code: *b"jpn",
                items: vec![ExtendedEventItem {
                    item_description: b"\x0eCast".to_vec(),
                    item: b"\x0eAlice".to_vec(),
                }],
                text: vec![],
            })),
        });

        let (tx, rx) = tokio::sync::oneshot::channel();
        writer.notify(move || {
            let _ = tx.send(());
        });
        rx.await.unwrap();

        let event = store
            .find_event(SECTION.service(), 0x0001)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(event.name.as_deref(), Some("Programme"));
        assert_eq!(
            event.description_items(),
            [("Cast".to_string(), "Alice".to_string())]
        );
    }
}
