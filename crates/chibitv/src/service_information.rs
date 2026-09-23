use std::collections::HashMap;

use tokio::sync::broadcast::Sender;

use chibitv_b10::table::{Eit, Sdt, Table as B10Table};
use chibitv_b60::message::{M2SectionMessage, Message};
use chibitv_b60::table::{MhEit, MhSdt, Table};

use crate::demux::SignalingEvent;
use crate::guide::{EventEntries, GuideUpdate, GuideWriter};
use crate::service::StoredService;
use crate::store::SectionId;

const SDT_ACTUAL_TABLE_ID: u8 = 0x42;
const MH_SDT_ACTUAL_TABLE_ID: u8 = 0x9F;
const EIT_ACTUAL_PRESENT_FOLLOWING_TABLE_ID: u8 = 0x4E;
const EIT_ACTUAL_SCHEDULE_TABLE_IDS: std::ops::RangeInclusive<u8> = 0x50..=0x5F;
const MH_EIT_ACTUAL_SCHEDULE_TABLE_IDS: std::ops::RangeInclusive<u8> = 0x8C..=0x9B;

#[derive(Clone, Debug)]
pub enum Signal {
    EventChanged { event_id: u16 },
}

/// Identifies one SI section among the ones a stream carries.
///
/// The table id is part of it because the present/following and the schedule
/// tables number their sections independently. A section of the SDT describes
/// every service of its stream, so it names none.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
struct SectionKey {
    table_id: u8,
    original_network_id: u16,
    /// The TLV stream id on ISDB-S3, the transport stream id on ISDB-T and
    /// ISDB-S.
    stream_id: u16,
    service_id: u16,
    section_number: u8,
}

/// Tells one revision of a section apart from the next.
///
/// The CRC comes along because `version_number` is five bits wide and wraps
/// around.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct SectionVersion {
    version_number: u8,
    crc_32: u32,
}

impl From<SectionKey> for SectionId {
    fn from(value: SectionKey) -> Self {
        Self {
            original_network_id: value.original_network_id,
            stream_id: value.stream_id,
            service_id: value.service_id,
            table_id: value.table_id,
            section_number: value.section_number,
        }
    }
}

pub struct ServiceInformationProcessor {
    watched_service_id: Option<u16>,
    writer: Option<GuideWriter>,
    signal_tx: Option<Sender<Signal>>,
    current_event_id: Option<u16>,
    stored_sections: HashMap<SectionKey, SectionVersion>,
}

impl ServiceInformationProcessor {
    /// Processes the SI of a stream, keeping what it says of the services and
    /// their schedule with `writer`.
    pub fn new(writer: Option<GuideWriter>, signal_tx: Option<Sender<Signal>>) -> Self {
        Self {
            watched_service_id: None,
            writer,
            signal_tx,
            current_event_id: None,
            stored_sections: HashMap::new(),
        }
    }

    /// Tracks what is on air on one service only.
    ///
    /// The SI of a transport stream describes every service it carries, so
    /// without this the programme on air is whichever service the EIT happens
    /// to mention first — on a terrestrial channel that is rarely the one
    /// being watched. The tables of the other services are still stored, which
    /// collects the schedule of the whole stream.
    ///
    /// `None` keeps tracking every service, as a capture of a whole transport
    /// stream has no single one.
    pub fn watching_service(mut self, service_id: Option<u16>) -> Self {
        self.watched_service_id = service_id;
        self
    }

    fn is_watched_service(&self, service_id: u16) -> bool {
        self.watched_service_id
            .is_none_or(|watched| watched == service_id)
    }

    pub fn process(&mut self, signaling: SignalingEvent) -> anyhow::Result<()> {
        match signaling {
            SignalingEvent::B10Table { table_id, table } => self.process_b10_table(table_id, table),
            SignalingEvent::B60Message(Message::M2Section(message)) => {
                self.process_m2_section_message(message)
            }
            SignalingEvent::B60Message(_) => Ok(()),
            // The TLV-SI describes where the stream is rather than what is on
            // it, which is the scanner's business rather than the guide's.
            SignalingEvent::TlvTable(_) => Ok(()),
        }
    }

    fn process_b10_table(&mut self, table_id: u8, table: B10Table) -> anyhow::Result<()> {
        match table {
            B10Table::Eit(table)
                if table_id == EIT_ACTUAL_PRESENT_FOLLOWING_TABLE_ID
                    || EIT_ACTUAL_SCHEDULE_TABLE_IDS.contains(&table_id) =>
            {
                self.process_b10_eit(table_id, table)
            }
            B10Table::Sdt(table) if table_id == SDT_ACTUAL_TABLE_ID => {
                self.process_b10_sdt(table);
                Ok(())
            }
            _ => Ok(()),
        }
    }

    fn process_b10_sdt(&mut self, table: Sdt) {
        let stream_id = table.transport_stream_id;
        self.store_section(
            SectionKey {
                table_id: SDT_ACTUAL_TABLE_ID,
                original_network_id: table.original_network_id,
                stream_id,
                service_id: 0,
                section_number: table.section_number,
            },
            SectionVersion {
                version_number: table.version_number,
                crc_32: table.crc_32,
            },
            || GuideUpdate::Services {
                stream_id,
                services: table
                    .services
                    .iter()
                    .filter_map(StoredService::from_b10)
                    .collect(),
            },
        );
    }

    fn process_b10_eit(&mut self, table_id: u8, table: Eit) -> anyhow::Result<()> {
        let key = SectionKey {
            table_id,
            original_network_id: table.original_network_id,
            stream_id: table.transport_stream_id,
            service_id: table.service_id,
            section_number: table.section_number,
        };

        self.store_section(
            key,
            SectionVersion {
                version_number: table.version_number,
                crc_32: table.crc_32,
            },
            || GuideUpdate::Events {
                section: key.into(),
                replaces: EIT_ACTUAL_SCHEDULE_TABLE_IDS.contains(&table_id),
                entries: EventEntries::B10(table.events.clone()),
            },
        );

        if !self.is_watched_service(table.service_id) {
            return Ok(());
        }

        for event in &table.events {
            self.process_event(event.event_id, event.start_time, event.duration);
        }

        Ok(())
    }

    fn process_m2_section_message(&mut self, message: M2SectionMessage) -> anyhow::Result<()> {
        match message.table {
            Table::MhEit(table) => self.process_mh_eit(table),
            Table::MhSdt(table) => {
                self.process_mh_sdt(table);
                Ok(())
            }
            _ => Ok(()),
        }
    }

    fn process_mh_eit(&mut self, table: MhEit) -> anyhow::Result<()> {
        let key = SectionKey {
            table_id: table.table_id,
            original_network_id: table.original_network_id,
            stream_id: table.tlv_stream_id,
            service_id: table.service_id,
            section_number: table.section_number,
        };

        self.store_section(
            key,
            SectionVersion {
                version_number: table.version_number,
                crc_32: table.crc_32,
            },
            || GuideUpdate::Events {
                section: key.into(),
                replaces: MH_EIT_ACTUAL_SCHEDULE_TABLE_IDS.contains(&table.table_id),
                entries: EventEntries::B60(table.events.clone()),
            },
        );

        if !self.is_watched_service(table.service_id) {
            return Ok(());
        }

        for event in &table.events {
            self.process_event(event.event_id, event.start_time, event.duration);
        }

        Ok(())
    }

    fn process_mh_sdt(&mut self, table: MhSdt) {
        let stream_id = table.tlv_stream_id;
        self.store_section(
            SectionKey {
                table_id: MH_SDT_ACTUAL_TABLE_ID,
                original_network_id: table.original_network_id,
                stream_id,
                service_id: 0,
                section_number: table.section_number,
            },
            SectionVersion {
                version_number: table.version_number,
                crc_32: table.crc_32,
            },
            || GuideUpdate::Services {
                stream_id,
                services: table
                    .services
                    .iter()
                    .filter_map(StoredService::from_b60)
                    .collect(),
            },
        );
    }

    /// Queues what a section says for the store, unless it already has it.
    ///
    /// A stream repeats every section every few seconds and only bumps its
    /// version when the content changes, so remembering the version keeps the
    /// store from rewriting what did not move.
    ///
    /// A section is only remembered once the store took it: one refused by a
    /// queue that is full has to be retried by the next repetition.
    fn store_section(
        &mut self,
        key: SectionKey,
        version: SectionVersion,
        update: impl FnOnce() -> GuideUpdate,
    ) {
        let Some(writer) = &self.writer else {
            return;
        };
        if self.stored_sections.get(&key) == Some(&version) {
            return;
        }

        if writer.enqueue(update()) {
            self.stored_sections.insert(key, version);
        }
    }

    fn process_event(
        &mut self,
        event_id: u16,
        start_time: Option<chrono::NaiveDateTime>,
        duration: Option<chrono::TimeDelta>,
    ) {
        let Some((start_time, duration)) = start_time.zip(duration) else {
            return;
        };

        // The SI carries JST wall-clock time and the server runs on that zone,
        // so the local clock is the one the broadcast schedules against.
        let now = chrono::Local::now().naive_local();
        if now < start_time || start_time + duration <= now {
            return;
        }
        if self.current_event_id == Some(event_id) {
            return;
        }
        self.current_event_id = Some(event_id);

        let Some(signal_tx) = self.signal_tx.clone() else {
            return;
        };
        // Nobody may be listening right now; that is fine.
        let signal = move || {
            let _ = signal_tx.send(Signal::EventChanged { event_id });
        };

        // Whoever receives the signal looks the event up, so it is sent once
        // the section describing the event is in the store.
        match &self.writer {
            Some(writer) => writer.notify(signal),
            None => signal(),
        }
    }

    pub fn current_event_id(&self) -> Option<u16> {
        self.current_event_id
    }
}

#[cfg(test)]
mod tests {
    use chrono::TimeDelta;
    use tokio::sync::broadcast::error::TryRecvError;

    use chibitv_b10::descriptor::{
        Descriptor as B10Descriptor, ServiceDescriptor, ShortEventDescriptor,
    };
    use chibitv_b10::table::{Eit, EventInformation, ServiceInformation as B10ServiceInformation};

    use super::*;

    const SERVICE_ID: u16 = 0x0400;
    const OTHER_SERVICE_ID: u16 = 0x0401;

    /// The stream the tables below belong to.
    const STREAM_ID: u16 = 1;

    /// An EIT[p/f] announcing an event that started a minute ago.
    fn eit_on_air(service_id: u16, event_id: u16) -> Eit {
        let now = chrono::Local::now().naive_local();

        Eit {
            section_syntax_indicator: true,
            section_length: 0,
            service_id,
            version_number: 0,
            current_next_indicator: true,
            section_number: 0,
            last_section_number: 0,
            transport_stream_id: STREAM_ID,
            original_network_id: 1,
            segment_last_section_number: 0,
            last_table_id: EIT_ACTUAL_PRESENT_FOLLOWING_TABLE_ID,
            events: vec![EventInformation {
                event_id,
                start_time: Some(now - TimeDelta::minutes(1)),
                duration: Some(TimeDelta::minutes(2)),
                running_status: 4,
                free_ca_mode: false,
                descriptors: vec![],
            }],
            crc_32: 0,
        }
    }

    /// The same EIT[p/f] carrying the name of the event it announces.
    fn eit_named(service_id: u16, event_id: u16, name: &str) -> Eit {
        let mut eit = eit_on_air(service_id, event_id);
        eit.events[0].descriptors = vec![B10Descriptor::ShortEvent(ShortEventDescriptor {
            iso_639_language_code: *b"jpn",
            event_name: [b"\x0e", name.as_bytes()].concat(),
            text: vec![],
        })];

        eit
    }

    /// The name of the events an update carries, for the service it is for.
    fn event_names(update: GuideUpdate) -> (u16, Vec<Option<String>>) {
        let GuideUpdate::Events {
            section,
            entries: EventEntries::B10(entries),
            ..
        } = update
        else {
            panic!("expected the events of a section");
        };

        let names = entries
            .iter()
            .map(|entry| {
                let mut event = crate::event::Event::new(section.service(), entry.event_id);
                event.apply_b10(entry);
                event.name
            })
            .collect();

        (section.service_id, names)
    }

    fn sdt_of(service_id: u16) -> Sdt {
        Sdt {
            section_syntax_indicator: true,
            section_length: 0,
            transport_stream_id: STREAM_ID,
            version_number: 0,
            current_next_indicator: true,
            section_number: 0,
            last_section_number: 0,
            original_network_id: 1,
            services: vec![B10ServiceInformation {
                service_id,
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
            }],
            crc_32: 0,
        }
    }

    fn signaling(table: B10Table) -> SignalingEvent {
        SignalingEvent::B10Table {
            table_id: EIT_ACTUAL_PRESENT_FOLLOWING_TABLE_ID,
            table,
        }
    }

    fn schedule_signaling(table: B10Table) -> SignalingEvent {
        SignalingEvent::B10Table {
            table_id: *EIT_ACTUAL_SCHEDULE_TABLE_IDS.start(),
            table,
        }
    }

    #[test]
    fn emits_the_current_event_only_once() {
        let (signal_tx, mut signal_rx) = tokio::sync::broadcast::channel(2);
        let mut processor = ServiceInformationProcessor::new(None, Some(signal_tx));
        let eit = eit_on_air(SERVICE_ID, 0x1234);

        processor
            .process(signaling(B10Table::Eit(eit.clone())))
            .unwrap();
        processor.process(signaling(B10Table::Eit(eit))).unwrap();

        assert!(matches!(
            signal_rx.try_recv(),
            Ok(Signal::EventChanged { event_id: 0x1234 })
        ));
        assert!(matches!(signal_rx.try_recv(), Err(TryRecvError::Empty)));
    }

    #[test]
    fn tracks_the_watched_service_only() {
        let (signal_tx, mut signal_rx) = tokio::sync::broadcast::channel(2);
        let (writer, mut updates) = GuideWriter::for_test();
        let mut processor = ServiceInformationProcessor::new(Some(writer), Some(signal_tx))
            .watching_service(Some(SERVICE_ID));

        // The transport stream carries the EIT of every service it multiplexes.
        processor
            .process(signaling(B10Table::Eit(eit_named(
                OTHER_SERVICE_ID,
                0x0002,
                "Elsewhere",
            ))))
            .unwrap();
        processor
            .process(signaling(B10Table::Eit(eit_on_air(SERVICE_ID, 0x0001))))
            .unwrap();

        assert_eq!(processor.current_event_id(), Some(0x0001));

        // The schedule of the other service is still collected.
        assert_eq!(
            event_names(updates.try_recv().unwrap()),
            (OTHER_SERVICE_ID, vec![Some("Elsewhere".to_string())])
        );
        assert_eq!(
            event_names(updates.try_recv().unwrap()),
            (SERVICE_ID, vec![None])
        );

        // The event on air is announced once the section describing it is
        // stored, and not before.
        assert!(matches!(signal_rx.try_recv(), Err(TryRecvError::Empty)));
        let Ok(GuideUpdate::Notify(notify)) = updates.try_recv() else {
            panic!("expected the signal to wait for the store");
        };
        notify();

        assert!(matches!(
            signal_rx.try_recv(),
            Ok(Signal::EventChanged { event_id: 0x0001 })
        ));
        assert!(matches!(signal_rx.try_recv(), Err(TryRecvError::Empty)));
        assert!(updates.try_recv().is_err());
    }

    #[test]
    fn stores_a_section_once_per_version() {
        let (writer, mut updates) = GuideWriter::for_test();
        let mut processor = ServiceInformationProcessor::new(Some(writer), None);

        processor
            .process(signaling(B10Table::Eit(eit_named(
                SERVICE_ID,
                0x0001,
                "Programme",
            ))))
            .unwrap();
        assert_eq!(
            event_names(updates.try_recv().unwrap()),
            (SERVICE_ID, vec![Some("Programme".to_string())])
        );

        // A section of a version already stored is dropped without reaching
        // the store, whatever it carries.
        processor
            .process(signaling(B10Table::Eit(eit_named(
                SERVICE_ID,
                0x0001,
                "Rewritten",
            ))))
            .unwrap();
        assert!(updates.try_recv().is_err());

        // A new version of it is stored again.
        let mut updated = eit_named(SERVICE_ID, 0x0001, "Updated");
        updated.version_number = 1;
        processor
            .process(signaling(B10Table::Eit(updated)))
            .unwrap();

        assert_eq!(
            event_names(updates.try_recv().unwrap()),
            (SERVICE_ID, vec![Some("Updated".to_string())])
        );
    }

    #[test]
    fn replaces_the_schedule_but_only_adds_what_is_on_air() {
        let (writer, mut updates) = GuideWriter::for_test();
        let mut processor = ServiceInformationProcessor::new(Some(writer), None);

        // The present/following table moves on to the next programme as soon
        // as one ends, which is no reason to forget the one that did.
        processor
            .process(signaling(B10Table::Eit(eit_named(
                SERVICE_ID, 0x0001, "On air",
            ))))
            .unwrap();
        processor
            .process(schedule_signaling(B10Table::Eit(eit_named(
                SERVICE_ID,
                0x0002,
                "Scheduled",
            ))))
            .unwrap();

        let replaces = |update| match update {
            GuideUpdate::Events {
                section, replaces, ..
            } => (section.table_id, replaces),
            _ => panic!("expected the events of a section"),
        };
        assert_eq!(
            replaces(updates.try_recv().unwrap()),
            (EIT_ACTUAL_PRESENT_FOLLOWING_TABLE_ID, false)
        );
        assert_eq!(
            replaces(updates.try_recv().unwrap()),
            (*EIT_ACTUAL_SCHEDULE_TABLE_IDS.start(), true)
        );
    }

    #[test]
    fn stores_the_television_services_of_the_stream_once_per_version() {
        let (writer, mut updates) = GuideWriter::for_test();
        let mut processor = ServiceInformationProcessor::new(Some(writer), None);
        let sdt = || SignalingEvent::B10Table {
            table_id: SDT_ACTUAL_TABLE_ID,
            table: B10Table::Sdt(sdt_of(SERVICE_ID)),
        };

        processor.process(sdt()).unwrap();
        processor.process(sdt()).unwrap();

        let Ok(GuideUpdate::Services {
            stream_id,
            services,
        }) = updates.try_recv()
        else {
            panic!("expected the services of the stream");
        };
        assert_eq!(stream_id, STREAM_ID);
        assert_eq!(
            services
                .iter()
                .map(|service| (service.id, service.name.as_str()))
                .collect::<Vec<_>>(),
            [(SERVICE_ID, "Channel")]
        );
        assert!(updates.try_recv().is_err());
    }
}
