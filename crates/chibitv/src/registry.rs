use std::sync::Arc;

use chrono::{NaiveDateTime, TimeDelta};
use papaya::HashMap;
use tracing::debug;

use chibitv_b10::descriptor::Descriptor as B10Descriptor;
use chibitv_b10::table::{
    EventInformation as B10EventInformation, ServiceInformation as B10ServiceInformation,
};
use chibitv_b24::decode as decode_b24;
use chibitv_b60::descriptor::Descriptor;
use chibitv_b60::table::{BroadcasterInformation, EventInformation, ServiceInformation};

#[derive(Clone, Debug)]
#[expect(
    dead_code,
    reason = "collected from the BIT, but not exposed over the API yet"
)]
pub struct Broadcaster {
    pub id: u8,
    pub name: String,
}

#[derive(Clone, Debug)]
pub struct Service {
    pub id: u16,
    pub name: String,
    pub provider_name: String,
    pub transport_stream_id: u16,
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
    services: HashMap<u16, Service>,
}

impl Registry {
    pub fn get_all_services(&self) -> Vec<Service> {
        let services = self.services.pin();
        services.values().cloned().collect()
    }

    pub fn get_service_by_id(&self, service_id: u16) -> Option<Service> {
        let services = self.services.pin();
        services.get(&service_id).cloned()
    }

    pub fn get_events_by_service_id(&self, service_id: u16) -> Vec<Event> {
        let services = self.services.pin();
        let Some(service) = services.get(&service_id) else {
            return vec![];
        };

        let events = service.events.pin();

        events.values().cloned().collect()
    }

    pub fn get_event_by_id(&self, service_id: u16, event_id: u16) -> Option<Event> {
        let services = self.services.pin();
        let events = services.get(&service_id)?.events.pin();

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

    pub fn put_service(
        &self,
        channel_id: usize,
        transport_stream_id: u16,
        service: &ServiceInformation,
    ) {
        let service_id = service.service_id;
        let services = self.services.pin();
        if services.contains_key(&service_id) {
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
            id: service_id,
            name: String::from_utf8_lossy(&descriptor.service_name).to_string(),
            provider_name: String::from_utf8_lossy(&descriptor.service_provider_name).to_string(),
            transport_stream_id,
            channel_id,
            events: Arc::new(HashMap::new()),
        };

        debug!(?service, "Added a new service");

        services.insert(service_id, service);
    }

    pub fn put_b10_service(
        &self,
        channel_id: usize,
        transport_stream_id: u16,
        service: &B10ServiceInformation,
    ) {
        let service_id = service.service_id;
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
            .get(&service_id)
            .map(|service| Arc::clone(&service.events))
            .unwrap_or_default();
        let service = Service {
            id: service_id,
            name: decode_b24(&descriptor.service_name),
            provider_name: decode_b24(&descriptor.service_provider_name),
            transport_stream_id,
            channel_id,
            events,
        };

        debug!(?service, "Added a new ISDB-T service");
        services.insert(service_id, service);
    }

    pub fn put_cached_service(
        &self,
        channel_id: usize,
        transport_stream_id: u16,
        service_id: u16,
        name: String,
        provider_name: String,
    ) {
        let services = self.services.pin();
        if services.contains_key(&service_id) {
            return;
        }

        services.insert(
            service_id,
            Service {
                id: service_id,
                name,
                provider_name,
                transport_stream_id,
                channel_id,
                events: Arc::new(HashMap::new()),
            },
        );
    }

    pub fn put_event(&self, service_id: u16, event: &EventInformation) {
        let services = self.services.pin();
        let Some(service) = services.get(&service_id) else {
            return;
        };

        let event_id = event.event_id;
        let events = service.events.pin();
        let previous = events.get(&event_id);

        let mut language_code = previous.and_then(|e| e.language_code.clone());
        let mut name = previous.and_then(|e| e.name.clone());
        let text = previous.and_then(|e| e.text.clone());
        let mut description = previous.map(|e| e.description.clone()).unwrap_or_default();

        for descriptor in &event.descriptors {
            match descriptor {
                Descriptor::MhShortEvent(descriptor) => {
                    language_code = Some(
                        String::from_utf8_lossy(&descriptor.iso_639_language_code[..]).to_string(),
                    );
                    name = Some(String::from_utf8_lossy(&descriptor.event_name).to_string());
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
    }

    pub fn put_b10_event(&self, service_id: u16, event: &B10EventInformation) {
        let services = self.services.pin();
        let Some(service) = services.get(&service_id) else {
            return;
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
    }
}

#[cfg(test)]
mod tests {
    use chrono::{Duration, NaiveDate};

    use chibitv_b10::descriptor::{
        Descriptor as B10Descriptor, ExtendedEventDescriptor, ExtendedEventItem, ServiceDescriptor,
        ShortEventDescriptor,
    };
    use chibitv_b60::descriptor::{Descriptor as B60Descriptor, MhServiceDescriptor};
    use chibitv_b60::table::ServiceInformation as B60ServiceInformation;

    use super::*;

    #[test]
    fn registers_isdb_s_service_with_channel_id() {
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

        let service = registry.get_service_by_id(0x5678).unwrap();
        assert_eq!(service.channel_id, 4);
        assert_eq!(service.transport_stream_id, 0x1234);
    }

    #[test]
    fn registers_isdb_t_service_and_event() {
        let registry = Registry::default();
        registry.put_cached_service(
            3,
            0x1234,
            0x5678,
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

        let service = registry.get_service_by_id(0x5678).unwrap();
        assert_eq!(service.name, "Channel");
        assert_eq!(service.provider_name, "Provider");
        assert_eq!(service.transport_stream_id, 0x1234);
        assert_eq!(service.channel_id, 3);

        let start_time = NaiveDate::from_ymd_opt(2026, 7, 11)
            .unwrap()
            .and_hms_opt(12, 0, 0)
            .unwrap();
        registry.put_b10_event(
            0x5678,
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

        let event = registry.get_event_by_id(0x5678, 0x9ABC).unwrap();
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
        registry.put_cached_service(0, 0x1234, 0x5678, "Channel".to_string(), String::new());

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
        registry.put_b10_event(0x5678, &extended_event(1, vec![(b"", b"\x0e Bob")]));
        registry.put_b10_event(
            0x5678,
            &extended_event(
                0,
                vec![(b"\x0eDetails", b"\x0eA show"), (b"\x0eCast", b"\x0eAlice")],
            ),
        );

        let event = registry.get_event_by_id(0x5678, 0x9ABC).unwrap();
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
        registry.put_cached_service(0, 0x1234, 0x5678, "Channel".to_string(), String::new());
        registry.put_b10_event(
            0x5678,
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
            0x5678,
            &B10EventInformation {
                event_id: 0x9ABC,
                start_time: None,
                duration: None,
                running_status: 4,
                free_ca_mode: false,
                descriptors: vec![],
            },
        );

        let event = registry.get_event_by_id(0x5678, 0x9ABC).unwrap();
        assert_eq!(event.text.as_deref(), Some("Summary"));
    }
}
