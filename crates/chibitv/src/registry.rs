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
    pub channel_id: Option<usize>,

    events: Arc<HashMap<u16, Event>>,
}

#[derive(Clone, Debug)]
pub struct Event {
    pub id: u16,
    pub start_time: Option<NaiveDateTime>,
    pub duration: Option<TimeDelta>,
    pub language_code: Option<String>,
    pub name: Option<String>,
    pub description: Vec<Vec<(String, String)>>,
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

    pub fn put_service(&self, transport_stream_id: u16, service: &ServiceInformation) {
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
            channel_id: None,
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
        if services.contains_key(&service_id) {
            return;
        }

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

        let service = Service {
            id: service_id,
            name: decode_b24(&descriptor.service_name),
            provider_name: decode_b24(&descriptor.service_provider_name),
            transport_stream_id,
            channel_id: Some(channel_id),
            events: Arc::new(HashMap::new()),
        };

        debug!(?service, "Added a new ISDB-T service");
        services.insert(service_id, service);
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

                    description[descriptor_idx] = descriptor
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
        let mut description = previous
            .map(|event| event.description.clone())
            .unwrap_or_default();

        for descriptor in &event.descriptors {
            if let B10Descriptor::ShortEvent(descriptor) = descriptor {
                language_code =
                    Some(String::from_utf8_lossy(&descriptor.iso_639_language_code).into_owned());
                name = Some(decode_b24(&descriptor.event_name));
                let text = decode_b24(&descriptor.text);
                description = if text.is_empty() {
                    vec![]
                } else {
                    vec![vec![(String::new(), text)]]
                };
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
                description,
            },
        );
    }
}

#[cfg(test)]
mod tests {
    use chrono::{Duration, NaiveDate};

    use chibitv_b10::descriptor::{
        Descriptor as B10Descriptor, ServiceDescriptor, ShortEventDescriptor,
    };

    use super::*;

    #[test]
    fn registers_isdb_t_service_and_event() {
        let registry = Registry::default();
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
        assert_eq!(service.channel_id, Some(3));

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
        assert_eq!(
            event.description,
            vec![vec![(String::new(), "Description".to_string())]]
        );
    }
}
