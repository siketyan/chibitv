use std::sync::Arc;

use anyhow::Context;
use chrono::{NaiveDateTime, TimeDelta};
use papaya::HashMap;
use tracing::{debug, info};

use chibitv_b10::descriptor::Descriptor as B10Descriptor;
use chibitv_b10::table::{
    EventInformation as B10EventInformation, ServiceInformation as B10ServiceInformation,
};
use chibitv_b24::decode as decode_b24;
use chibitv_b60::descriptor::Descriptor;
use chibitv_b60::table::{BroadcasterInformation, EventInformation, ServiceInformation};

use crate::store::{EventWriter, SectionId, SectionUpdate, Store, StoredEvent};

#[derive(Clone, Debug)]
#[expect(
    dead_code,
    reason = "collected from the BIT, but not exposed over the API yet"
)]
pub struct Broadcaster {
    pub id: u8,
    pub name: String,
}

/// Identifies one service among the ones on air.
///
/// A service id alone does not tell a service apart from every other: BS 2K
/// and BS 4K number theirs alike, so the stream carrying one goes with it.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ServiceKey {
    /// The TLV stream id on ISDB-S3, the transport stream id on ISDB-T and
    /// ISDB-S.
    pub stream_id: u16,
    pub service_id: u16,
}

#[derive(Clone, Debug)]
pub struct Service {
    pub key: ServiceKey,
    pub name: String,
    pub provider_name: String,
    pub channel_id: usize,

    events: Arc<HashMap<u16, Event>>,
}

#[derive(Clone, Debug)]
pub struct Event {
    pub id: u16,
    pub start_time: Option<NaiveDateTime>,
    pub duration: Option<TimeDelta>,
    pub language_code: Option<String>,
    pub name: Option<String>,
    /// The summary of the event, from the short event descriptor.
    pub text: Option<String>,
    /// The detailed description of the event, one entry per extended event
    /// descriptor: an event is described by up to 16 of them, each numbered so
    /// that they can be collected in order as they arrive.
    pub description: Vec<Vec<(String, String)>>,
}

impl Event {
    /// The detailed description as a flat list of items.
    ///
    /// An item too long for one descriptor continues in the next one, as an
    /// item carrying no description of its own, so those are joined back to the
    /// item they belong to.
    pub fn description_items(&self) -> Vec<(String, String)> {
        let mut items: Vec<(String, String)> = Vec::new();
        for (name, content) in self.description.iter().flatten() {
            match items.last_mut() {
                Some((_, previous)) if name.is_empty() => previous.push_str(content),
                _ => items.push((name.clone(), content.clone())),
            }
        }

        items
    }
}

#[derive(Default)]
pub struct Registry {
    broadcasters: HashMap<u8, Broadcaster>,
    services: HashMap<ServiceKey, Service>,
    events: Option<EventWriter>,
}

impl Registry {
    /// Keeps the schedule this collects between runs.
    pub fn storing_events(mut self, events: EventWriter) -> Self {
        self.events = Some(events);
        self
    }

    /// Fills the registry with the schedule of the previous run.
    pub async fn restore_events(&self, store: &Arc<dyn Store>) -> anyhow::Result<usize> {
        let events = store
            .load_events()
            .await
            .context("Could not read the stored schedule")?;

        // The services come from the configuration while starting up, so the
        // schedule of one that has never been scanned has nowhere to go.
        let restored = events
            .into_iter()
            .filter(|event| {
                let services = self.services.pin();
                let Some(service) = services.get(&event.key) else {
                    return false;
                };

                service
                    .events
                    .pin()
                    .insert(event.event_id, event.clone().into());

                true
            })
            .count();

        info!(restored, "Restored the stored schedule");

        Ok(restored)
    }

    pub fn get_all_services(&self) -> Vec<Service> {
        let services = self.services.pin();
        services.values().cloned().collect()
    }

    pub fn get_service(&self, key: ServiceKey) -> Option<Service> {
        let services = self.services.pin();
        services.get(&key).cloned()
    }

    pub fn get_events(&self, key: ServiceKey) -> Vec<Event> {
        let services = self.services.pin();
        let Some(service) = services.get(&key) else {
            return vec![];
        };

        let events = service.events.pin();

        events.values().cloned().collect()
    }

    pub fn get_event(&self, key: ServiceKey, event_id: u16) -> Option<Event> {
        let services = self.services.pin();
        let events = services.get(&key)?.events.pin();

        events.get(&event_id).cloned()
    }

    pub fn put_broadcaster(&self, broadcaster: &BroadcasterInformation) {
        let broadcaster_id = broadcaster.broadcaster_id;
        let broadcasters = self.broadcasters.pin();
        if broadcasters.contains_key(&broadcaster_id) {
            return;
        }

        let Some(name) = broadcaster.descriptors.iter().find_map(|descriptor| {
            if let Descriptor::MhBroadcasterName(descriptor) = descriptor {
                Some(String::from_utf8_lossy(&descriptor.name).to_string())
            } else {
                None
            }
        }) else {
            return;
        };

        let broadcaster = Broadcaster {
            id: broadcaster_id,
            name,
        };

        debug!(?broadcaster, "Added a new broadcaster");

        broadcasters.insert(broadcaster_id, broadcaster);
    }

    pub fn put_service(&self, channel_id: usize, stream_id: u16, service: &ServiceInformation) {
        let key = ServiceKey {
            stream_id,
            service_id: service.service_id,
        };
        let services = self.services.pin();
        if services.contains_key(&key) {
            return;
        }

        let Some(descriptor) = service.descriptors.iter().find_map(|descriptor| {
            if let Descriptor::MhService(descriptor) = descriptor {
                Some(descriptor)
            } else {
                None
            }
        }) else {
            return;
        };

        // Only TV service is supported for now.
        if descriptor.service_type != 1 {
            return;
        }

        let service = Service {
            key,
            name: String::from_utf8_lossy(&descriptor.service_name).to_string(),
            provider_name: String::from_utf8_lossy(&descriptor.service_provider_name).to_string(),
            channel_id,
            events: Arc::new(HashMap::new()),
        };

        debug!(?service, "Added a new service");

        services.insert(key, service);
    }

    pub fn put_b10_service(
        &self,
        channel_id: usize,
        stream_id: u16,
        service: &B10ServiceInformation,
    ) {
        let key = ServiceKey {
            stream_id,
            service_id: service.service_id,
        };
        let services = self.services.pin();

        let Some(descriptor) = service.descriptors.iter().find_map(|descriptor| {
            if let B10Descriptor::Service(descriptor) = descriptor {
                Some(descriptor)
            } else {
                None
            }
        }) else {
            return;
        };

        // Digital television service.
        if descriptor.service_type != 0x01 {
            return;
        }

        let events = services
            .get(&key)
            .map(|service| Arc::clone(&service.events))
            .unwrap_or_default();
        let service = Service {
            key,
            name: decode_b24(&descriptor.service_name),
            provider_name: decode_b24(&descriptor.service_provider_name),
            channel_id,
            events,
        };

        debug!(?service, "Added a new ISDB-T service");
        services.insert(key, service);
    }

    pub fn put_cached_service(
        &self,
        channel_id: usize,
        key: ServiceKey,
        name: String,
        provider_name: String,
    ) {
        let services = self.services.pin();
        if services.contains_key(&key) {
            return;
        }

        services.insert(
            key,
            Service {
                key,
                name,
                provider_name,
                channel_id,
                events: Arc::new(HashMap::new()),
            },
        );
    }

    /// Stores the events one EIT section describes, reporting whether every
    /// one of them could be stored.
    ///
    /// A section named by `section` is kept between runs under that name, so
    /// that revising it replaces what it described before. One left unnamed —
    /// the present and following events, which the schedule describes as well
    /// — stays in memory only.
    pub fn put_events(
        &self,
        key: ServiceKey,
        section: Option<SectionId>,
        events: &[EventInformation],
    ) -> bool {
        // Every event is offered, so short circuiting on the first one dropped
        // would lose the rest.
        let mut stored = Vec::with_capacity(events.len());
        let mut complete = true;
        for event in events {
            if self.put_event(key, event) {
                stored.push(event.event_id);
            } else {
                complete = false;
            }
        }

        complete && self.keep_events(key, section, &stored)
    }

    /// The ISDB-T counterpart of [`Registry::put_events`].
    pub fn put_b10_events(
        &self,
        key: ServiceKey,
        section: Option<SectionId>,
        events: &[B10EventInformation],
    ) -> bool {
        let mut stored = Vec::with_capacity(events.len());
        let mut complete = true;
        for event in events {
            if self.put_b10_event(key, event) {
                stored.push(event.event_id);
            } else {
                complete = false;
            }
        }

        complete && self.keep_events(key, section, &stored)
    }

    /// Queues the events for the store, reporting whether it took them.
    ///
    /// The events are read back rather than taken from the section, as the
    /// registry is the one that assembled them out of everything the
    /// descriptors of a service carried.
    fn keep_events(&self, key: ServiceKey, section: Option<SectionId>, event_ids: &[u16]) -> bool {
        let (Some(section), Some(writer)) = (section, &self.events) else {
            return true;
        };

        let events = event_ids
            .iter()
            .filter_map(|event_id| self.get_event(key, *event_id))
            .map(|event| StoredEvent::of_service(key, &event))
            .collect();

        writer.enqueue(SectionUpdate { section, events })
    }

    /// Stores an event of a service, reporting whether it could be stored.
    ///
    /// An event of a service the registry does not know yet — an EIT ahead of
    /// the SDT describes one — is dropped, and `false` tells the caller the
    /// schedule it carried is still missing.
    fn put_event(&self, key: ServiceKey, event: &EventInformation) -> bool {
        let services = self.services.pin();
        let Some(service) = services.get(&key) else {
            return false;
        };

        let event_id = event.event_id;
        let events = service.events.pin();
        let previous = events.get(&event_id);

        let mut language_code = previous.and_then(|e| e.language_code.clone());
        let mut name = previous.and_then(|e| e.name.clone());
        let mut text = previous.and_then(|e| e.text.clone());
        let mut description = previous.map(|e| e.description.clone()).unwrap_or_default();

        for descriptor in &event.descriptors {
            match descriptor {
                Descriptor::MhShortEvent(descriptor) => {
                    language_code = Some(
                        String::from_utf8_lossy(&descriptor.iso_639_language_code[..]).to_string(),
                    );
                    name = Some(String::from_utf8_lossy(&descriptor.event_name).to_string());

                    let summary = String::from_utf8_lossy(&descriptor.text);
                    text = (!summary.is_empty()).then(|| summary.into_owned());
                }
                Descriptor::MhExtendedEvent(descriptor) => {
                    let descriptors_len = (descriptor.last_descriptor_number + 1) as usize;
                    let descriptor_idx = descriptor.descriptor_number as usize;

                    if description.len() != descriptors_len {
                        description = std::iter::repeat_n(vec![], descriptors_len).collect();
                    }

                    if let Some(items) = description.get_mut(descriptor_idx) {
                        *items = descriptor
                            .items
                            .iter()
                            .map(|item| {
                                (
                                    String::from_utf8_lossy(&item.item_description).to_string(),
                                    String::from_utf8_lossy(&item.item).to_string(),
                                )
                            })
                            .collect();
                    }
                }
                _ => {}
            }
        }

        if previous.is_none() {
            debug!(event_id, ?event.start_time, ?event.duration, ?name, "Added a new event");
        }

        let event = Event {
            id: event_id,
            start_time: event.start_time,
            duration: event.duration,
            language_code,
            name,
            text,
            description,
        };

        events.insert(event_id, event);

        true
    }

    /// Stores an ISDB-T event of a service, reporting whether it could be
    /// stored. See [`Registry::put_event`].
    fn put_b10_event(&self, key: ServiceKey, event: &B10EventInformation) -> bool {
        let services = self.services.pin();
        let Some(service) = services.get(&key) else {
            return false;
        };

        let event_id = event.event_id;
        let events = service.events.pin();
        let previous = events.get(&event_id);

        let mut language_code = previous.and_then(|event| event.language_code.clone());
        let mut name = previous.and_then(|event| event.name.clone());
        let mut text = previous.and_then(|event| event.text.clone());
        let mut description = previous
            .map(|event| event.description.clone())
            .unwrap_or_default();

        for descriptor in &event.descriptors {
            match descriptor {
                B10Descriptor::ShortEvent(descriptor) => {
                    language_code = Some(
                        String::from_utf8_lossy(&descriptor.iso_639_language_code).into_owned(),
                    );
                    name = Some(decode_b24(&descriptor.event_name));

                    let decoded = decode_b24(&descriptor.text);
                    text = (!decoded.is_empty()).then_some(decoded);
                }
                // The detailed description of a terrestrial programme is
                // carried here, split over as many descriptors as it needs.
                B10Descriptor::ExtendedEvent(descriptor) => {
                    let descriptors_len = (descriptor.last_descriptor_number + 1) as usize;
                    let descriptor_idx = descriptor.descriptor_number as usize;

                    if description.len() != descriptors_len {
                        description = std::iter::repeat_n(vec![], descriptors_len).collect();
                    }

                    if let Some(items) = description.get_mut(descriptor_idx) {
                        *items = descriptor
                            .items
                            .iter()
                            .map(|item| {
                                (decode_b24(&item.item_description), decode_b24(&item.item))
                            })
                            .collect();
                    }
                }
                _ => {}
            }
        }

        if previous.is_none() {
            debug!(event_id, ?event.start_time, ?event.duration, ?name, "Added a new ISDB-T event");
        }

        events.insert(
            event_id,
            Event {
                id: event_id,
                start_time: event.start_time,
                duration: event.duration,
                language_code,
                name,
                text,
                description,
            },
        );

        true
    }
}

#[cfg(test)]
mod tests {
    use chrono::{Duration, NaiveDate};

    use chibitv_b10::descriptor::{
        Descriptor as B10Descriptor, ExtendedEventDescriptor, ExtendedEventItem, ServiceDescriptor,
        ShortEventDescriptor,
    };
    use chibitv_b60::descriptor::{
        Descriptor as B60Descriptor, ExtendedEventItem as MhExtendedEventItem,
        MhExtendedEventDescriptor, MhServiceDescriptor, MhShortEventDescriptor,
    };
    use chibitv_b60::table::{
        EventInformation as B60EventInformation, EventRunningStatus,
        ServiceInformation as B60ServiceInformation,
    };

    use super::*;

    const SERVICE: ServiceKey = ServiceKey {
        stream_id: 0x1234,
        service_id: 0x5678,
    };

    #[tokio::test]
    async fn restores_the_schedule_of_the_services_it_knows() {
        let store = crate::store::open("sqlite::memory:").await.unwrap();
        let section = SectionId {
            original_network_id: 4,
            stream_id: 0x1234,
            service_id: 0x5678,
            table_id: 0x50,
            section_number: 0,
        };
        store
            .replace_section(
                section,
                &[
                    stored_event(SERVICE, 0x0001, "Programme"),
                    // The configuration knows nothing of this service, so its
                    // schedule has nowhere to go.
                    stored_event(
                        ServiceKey {
                            service_id: 0x9ABC,
                            ..SERVICE
                        },
                        0x0002,
                        "Elsewhere",
                    ),
                ],
            )
            .await
            .unwrap();

        let registry = Registry::default();
        registry.put_cached_service(0, SERVICE, "Channel".to_string(), String::new());

        assert_eq!(registry.restore_events(&store).await.unwrap(), 1);
        assert_eq!(
            registry
                .get_event(SERVICE, 0x0001)
                .and_then(|event| event.name)
                .as_deref(),
            Some("Programme")
        );
    }

    fn stored_event(key: ServiceKey, event_id: u16, name: &str) -> StoredEvent {
        StoredEvent {
            key,
            event_id,
            start_time: None,
            duration: None,
            language_code: None,
            name: Some(name.to_string()),
            text: None,
            description: vec![],
        }
    }

    #[test]
    fn registers_isdb_s3_service_with_channel_id() {
        let registry = Registry::default();
        registry.put_service(
            4,
            0x1234,
            &B60ServiceInformation {
                service_id: 0x5678,
                eit_user_defined_flags: 0,
                eit_schedule_flag: true,
                eit_present_following_flag: true,
                running_status: 4,
                free_ca_mode: false,
                descriptors: vec![B60Descriptor::MhService(MhServiceDescriptor {
                    service_type: 0x01,
                    service_provider_name: b"Provider".to_vec(),
                    service_name: b"Channel".to_vec(),
                })],
            },
        );

        let service = registry.get_service(SERVICE).unwrap();
        assert_eq!(service.channel_id, 4);
        assert_eq!(service.key.stream_id, 0x1234);
    }

    #[test]
    fn tells_the_services_of_two_streams_apart() {
        // BS 2K and BS 4K both carry a service numbered 101, on streams of
        // their own.
        let registry = Registry::default();
        registry.put_cached_service(
            0,
            ServiceKey {
                stream_id: 0x40F1,
                service_id: 101,
            },
            "BS Channel".to_string(),
            String::new(),
        );
        registry.put_cached_service(
            1,
            ServiceKey {
                stream_id: 0xB071,
                service_id: 101,
            },
            "BS 4K Channel".to_string(),
            String::new(),
        );

        assert_eq!(registry.get_all_services().len(), 2);
        assert_eq!(
            registry
                .get_service(ServiceKey {
                    stream_id: 0xB071,
                    service_id: 101,
                })
                .map(|service| service.name)
                .as_deref(),
            Some("BS 4K Channel")
        );
    }

    #[test]
    fn collects_isdb_s3_event_summary_and_details() {
        let registry = Registry::default();
        registry.put_service(
            0,
            0x1234,
            &B60ServiceInformation {
                service_id: 0x5678,
                eit_user_defined_flags: 0,
                eit_schedule_flag: true,
                eit_present_following_flag: true,
                running_status: 4,
                free_ca_mode: false,
                descriptors: vec![B60Descriptor::MhService(MhServiceDescriptor {
                    service_type: 0x01,
                    service_provider_name: b"Provider".to_vec(),
                    service_name: b"Channel".to_vec(),
                })],
            },
        );

        let event = |descriptors| B60EventInformation {
            event_id: 0x9ABC,
            start_time: None,
            duration: None,
            running_status: EventRunningStatus::InOperation,
            free_ca_mode: false,
            descriptors,
        };

        registry.put_event(
            SERVICE,
            &event(vec![B60Descriptor::MhExtendedEvent(
                MhExtendedEventDescriptor {
                    descriptor_number: 1,
                    last_descriptor_number: 1,
                    iso_639_language_code: *b"jpn",
                    items: vec![MhExtendedEventItem {
                        item_description: vec![],
                        item: b" Bob".to_vec(),
                    }],
                    text: vec![],
                },
            )]),
        );
        registry.put_event(
            SERVICE,
            &event(vec![
                B60Descriptor::MhShortEvent(MhShortEventDescriptor {
                    iso_639_language_code: *b"jpn",
                    event_name: b"Program".to_vec(),
                    text: b"Summary".to_vec(),
                }),
                B60Descriptor::MhExtendedEvent(MhExtendedEventDescriptor {
                    descriptor_number: 0,
                    last_descriptor_number: 1,
                    iso_639_language_code: *b"jpn",
                    items: vec![MhExtendedEventItem {
                        item_description: b"Cast".to_vec(),
                        item: b"Alice".to_vec(),
                    }],
                    text: vec![],
                }),
            ]),
        );

        let event = registry.get_event(SERVICE, 0x9ABC).unwrap();
        assert_eq!(event.name.as_deref(), Some("Program"));
        assert_eq!(event.text.as_deref(), Some("Summary"));
        assert_eq!(
            event.description_items(),
            vec![("Cast".to_string(), "Alice Bob".to_string())]
        );
    }

    #[test]
    fn registers_isdb_t_service_and_event() {
        let registry = Registry::default();
        registry.put_cached_service(
            3,
            ServiceKey {
                stream_id: 0x1234,
                service_id: 0x5678,
            },
            "Cached Channel".to_string(),
            "Cached Provider".to_string(),
        );
        registry.put_b10_service(
            3,
            0x1234,
            &B10ServiceInformation {
                service_id: 0x5678,
                eit_user_defined_flags: 0,
                eit_schedule_flag: true,
                eit_present_following_flag: true,
                running_status: 4,
                free_ca_mode: false,
                descriptors: vec![B10Descriptor::Service(ServiceDescriptor {
                    service_type: 0x01,
                    service_provider_name: b"\x0eProvider".to_vec(),
                    service_name: b"\x0eChannel".to_vec(),
                })],
            },
        );

        let service = registry.get_service(SERVICE).unwrap();
        assert_eq!(service.name, "Channel");
        assert_eq!(service.provider_name, "Provider");
        assert_eq!(service.key.stream_id, 0x1234);
        assert_eq!(service.channel_id, 3);

        let start_time = NaiveDate::from_ymd_opt(2026, 7, 11)
            .unwrap()
            .and_hms_opt(12, 0, 0)
            .unwrap();
        registry.put_b10_event(
            SERVICE,
            &B10EventInformation {
                event_id: 0x9ABC,
                start_time: Some(start_time),
                duration: Some(Duration::minutes(30)),
                running_status: 4,
                free_ca_mode: false,
                descriptors: vec![B10Descriptor::ShortEvent(ShortEventDescriptor {
                    iso_639_language_code: *b"jpn",
                    event_name: b"\x0eProgram".to_vec(),
                    text: b"\x0eDescription".to_vec(),
                })],
            },
        );

        let event = registry.get_event(SERVICE, 0x9ABC).unwrap();
        assert_eq!(event.name.as_deref(), Some("Program"));
        assert_eq!(event.language_code.as_deref(), Some("jpn"));
        assert_eq!(event.start_time, Some(start_time));
        assert_eq!(event.duration, Some(Duration::minutes(30)));
        assert_eq!(event.text.as_deref(), Some("Description"));
        assert!(event.description.is_empty());
    }

    #[test]
    fn collects_isdb_t_event_details_from_every_extended_event_descriptor() {
        let registry = Registry::default();
        registry.put_cached_service(0, SERVICE, "Channel".to_string(), String::new());

        let extended_event = |descriptor_number, items: Vec<(&[u8], &[u8])>| B10EventInformation {
            event_id: 0x9ABC,
            start_time: None,
            duration: None,
            running_status: 4,
            free_ca_mode: false,
            descriptors: vec![B10Descriptor::ExtendedEvent(ExtendedEventDescriptor {
                descriptor_number,
                last_descriptor_number: 1,
                iso_639_language_code: *b"jpn",
                items: items
                    .into_iter()
                    .map(|(item_description, item)| ExtendedEventItem {
                        item_description: item_description.to_vec(),
                        item: item.to_vec(),
                    })
                    .collect(),
                text: vec![],
            })],
        };

        // The second descriptor may well arrive first, and its leading item
        // continues the last item of the first one.
        registry.put_b10_event(SERVICE, &extended_event(1, vec![(b"", b"\x0e Bob")]));
        registry.put_b10_event(
            SERVICE,
            &extended_event(
                0,
                vec![(b"\x0eDetails", b"\x0eA show"), (b"\x0eCast", b"\x0eAlice")],
            ),
        );

        let event = registry.get_event(SERVICE, 0x9ABC).unwrap();
        assert_eq!(
            event.description_items(),
            vec![
                ("Details".to_string(), "A show".to_string()),
                ("Cast".to_string(), "Alice Bob".to_string()),
            ]
        );
    }

    #[test]
    fn keeps_the_isdb_t_summary_when_only_the_schedule_is_known() {
        let registry = Registry::default();
        registry.put_cached_service(0, SERVICE, "Channel".to_string(), String::new());
        registry.put_b10_event(
            SERVICE,
            &B10EventInformation {
                event_id: 0x9ABC,
                start_time: None,
                duration: None,
                running_status: 4,
                free_ca_mode: false,
                descriptors: vec![B10Descriptor::ShortEvent(ShortEventDescriptor {
                    iso_639_language_code: *b"jpn",
                    event_name: b"\x0eProgram".to_vec(),
                    text: b"\x0eSummary".to_vec(),
                })],
            },
        );
        registry.put_b10_event(
            SERVICE,
            &B10EventInformation {
                event_id: 0x9ABC,
                start_time: None,
                duration: None,
                running_status: 4,
                free_ca_mode: false,
                descriptors: vec![],
            },
        );

        let event = registry.get_event(SERVICE, 0x9ABC).unwrap();
        assert_eq!(event.text.as_deref(), Some("Summary"));
    }
}
