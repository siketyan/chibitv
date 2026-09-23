//! The programmes of the schedule, as the EIT describes them.

use chrono::{NaiveDateTime, TimeDelta};

use chibitv_b10::descriptor::Descriptor as B10Descriptor;
use chibitv_b10::table::EventInformation as B10EventInformation;
use chibitv_b24::decode as decode_b24;
use chibitv_b60::descriptor::Descriptor as B60Descriptor;
use chibitv_b60::table::EventInformation as B60EventInformation;

use crate::service::ServiceKey;

/// One programme of a service, identified by the service carrying it and its
/// event id.
#[derive(Clone, Debug, PartialEq)]
pub struct Event {
    pub key: ServiceKey,
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
    /// An event nothing is known of yet but its identity.
    pub fn new(key: ServiceKey, id: u16) -> Self {
        Self {
            key,
            id,
            start_time: None,
            duration: None,
            language_code: None,
            name: None,
            text: None,
            description: vec![],
        }
    }

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

    /// Takes in what an MH-EIT entry says of the event.
    ///
    /// An event is described across several sections — the schedule names it
    /// in one table and details it in another — so what the entry carries no
    /// descriptor for is left as it was.
    pub fn apply_b60(&mut self, event: &B60EventInformation) {
        self.start_time = event.start_time;
        self.duration = event.duration;

        for descriptor in &event.descriptors {
            match descriptor {
                B60Descriptor::MhShortEvent(descriptor) => {
                    self.language_code = Some(
                        String::from_utf8_lossy(&descriptor.iso_639_language_code[..]).to_string(),
                    );
                    self.name = Some(String::from_utf8_lossy(&descriptor.event_name).to_string());

                    let summary = String::from_utf8_lossy(&descriptor.text);
                    self.text = (!summary.is_empty()).then(|| summary.into_owned());
                }
                B60Descriptor::MhExtendedEvent(descriptor) => self.put_description(
                    descriptor.descriptor_number,
                    descriptor.last_descriptor_number,
                    descriptor
                        .items
                        .iter()
                        .map(|item| {
                            (
                                String::from_utf8_lossy(&item.item_description).to_string(),
                                String::from_utf8_lossy(&item.item).to_string(),
                            )
                        })
                        .collect(),
                ),
                _ => {}
            }
        }
    }

    /// The ISDB-T counterpart of [`Event::apply_b60`].
    pub fn apply_b10(&mut self, event: &B10EventInformation) {
        self.start_time = event.start_time;
        self.duration = event.duration;

        for descriptor in &event.descriptors {
            match descriptor {
                B10Descriptor::ShortEvent(descriptor) => {
                    self.language_code = Some(
                        String::from_utf8_lossy(&descriptor.iso_639_language_code).into_owned(),
                    );
                    self.name = Some(decode_b24(&descriptor.event_name));

                    let decoded = decode_b24(&descriptor.text);
                    self.text = (!decoded.is_empty()).then_some(decoded);
                }
                // The detailed description of a terrestrial programme is
                // carried here, split over as many descriptors as it needs.
                B10Descriptor::ExtendedEvent(descriptor) => self.put_description(
                    descriptor.descriptor_number,
                    descriptor.last_descriptor_number,
                    descriptor
                        .items
                        .iter()
                        .map(|item| (decode_b24(&item.item_description), decode_b24(&item.item)))
                        .collect(),
                ),
                _ => {}
            }
        }
    }

    /// Puts the items of one extended event descriptor in its place.
    fn put_description(
        &mut self,
        descriptor_number: u8,
        last_descriptor_number: u8,
        items: Vec<(String, String)>,
    ) {
        let descriptors_len = last_descriptor_number as usize + 1;
        if self.description.len() != descriptors_len {
            self.description = std::iter::repeat_n(vec![], descriptors_len).collect();
        }

        if let Some(slot) = self.description.get_mut(descriptor_number as usize) {
            *slot = items;
        }
    }
}

#[cfg(test)]
mod tests {
    use chrono::{Duration, NaiveDate};

    use chibitv_b10::descriptor::{
        ExtendedEventDescriptor, ExtendedEventItem, ShortEventDescriptor,
    };
    use chibitv_b60::descriptor::{
        ExtendedEventItem as MhExtendedEventItem, MhExtendedEventDescriptor, MhShortEventDescriptor,
    };
    use chibitv_b60::table::EventRunningStatus;

    use super::*;

    const SERVICE: ServiceKey = ServiceKey {
        stream_id: 0x1234,
        service_id: 0x5678,
    };

    #[test]
    fn collects_isdb_s3_event_summary_and_details() {
        let entry = |descriptors| B60EventInformation {
            event_id: 0x9ABC,
            start_time: None,
            duration: None,
            running_status: EventRunningStatus::InOperation,
            free_ca_mode: false,
            descriptors,
        };

        let mut event = Event::new(SERVICE, 0x9ABC);
        event.apply_b60(&entry(vec![B60Descriptor::MhExtendedEvent(
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
        )]));
        event.apply_b60(&entry(vec![
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
        ]));

        assert_eq!(event.name.as_deref(), Some("Program"));
        assert_eq!(event.text.as_deref(), Some("Summary"));
        assert_eq!(
            event.description_items(),
            vec![("Cast".to_string(), "Alice Bob".to_string())]
        );
    }

    #[test]
    fn reads_an_isdb_t_event() {
        let start_time = NaiveDate::from_ymd_opt(2026, 7, 11)
            .unwrap()
            .and_hms_opt(12, 0, 0)
            .unwrap();

        let mut event = Event::new(SERVICE, 0x9ABC);
        event.apply_b10(&B10EventInformation {
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
        });

        assert_eq!(event.name.as_deref(), Some("Program"));
        assert_eq!(event.language_code.as_deref(), Some("jpn"));
        assert_eq!(event.start_time, Some(start_time));
        assert_eq!(event.duration, Some(Duration::minutes(30)));
        assert_eq!(event.text.as_deref(), Some("Description"));
        assert!(event.description.is_empty());
    }

    #[test]
    fn collects_isdb_t_event_details_from_every_extended_event_descriptor() {
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
        let mut event = Event::new(SERVICE, 0x9ABC);
        event.apply_b10(&extended_event(1, vec![(b"", b"\x0e Bob")]));
        event.apply_b10(&extended_event(
            0,
            vec![(b"\x0eDetails", b"\x0eA show"), (b"\x0eCast", b"\x0eAlice")],
        ));

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
        let entry = |descriptors| B10EventInformation {
            event_id: 0x9ABC,
            start_time: None,
            duration: None,
            running_status: 4,
            free_ca_mode: false,
            descriptors,
        };

        let mut event = Event::new(SERVICE, 0x9ABC);
        event.apply_b10(&entry(vec![B10Descriptor::ShortEvent(
            ShortEventDescriptor {
                iso_639_language_code: *b"jpn",
                event_name: b"\x0eProgram".to_vec(),
                text: b"\x0eSummary".to_vec(),
            },
        )]));
        event.apply_b10(&entry(vec![]));

        assert_eq!(event.text.as_deref(), Some("Summary"));
    }
}
