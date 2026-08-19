use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::broadcast::Sender;

use chibitv_b10::table::{Eit, Sdt, Table as B10Table};
use chibitv_b60::message::{M2SectionMessage, Message};
use chibitv_b60::table::{MhBit, MhEit, MhSdt, Table};

use crate::demux::SignalingEvent;
use crate::registry::Registry;

const SDT_ACTUAL_TABLE_ID: u8 = 0x42;
const EIT_ACTUAL_PRESENT_FOLLOWING_TABLE_ID: u8 = 0x4E;
const EIT_ACTUAL_SCHEDULE_TABLE_IDS: std::ops::RangeInclusive<u8> = 0x50..=0x5F;

#[derive(Clone, Debug)]
pub enum Signal {
    EventChanged { event_id: u16 },
}

/// Identifies one EIT section among the ones a stream carries.
///
/// The table id is part of it because the present/following and the schedule
/// tables number their sections independently.
type SectionKey = (u8, u16, u16, u16, u8);

pub struct ServiceInformationProcessor {
    channel_id: usize,
    watched_service_id: Option<u16>,
    registry: Option<Arc<Registry>>,
    signal_tx: Option<Sender<Signal>>,
    current_event_id: Option<u16>,
    stored_sections: HashMap<SectionKey, (u8, u32)>,
}

impl ServiceInformationProcessor {
    pub fn new(
        channel_id: usize,
        registry: Option<Arc<Registry>>,
        signal_tx: Option<Sender<Signal>>,
    ) -> Self {
        Self {
            channel_id,
            watched_service_id: None,
            registry,
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
    /// being watched. The tables of the other services still reach the
    /// registry, which collects the schedule of the whole stream.
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

    fn process_b10_sdt(&self, table: Sdt) {
        if let Some(registry) = &self.registry {
            for service in &table.services {
                registry.put_b10_service(self.channel_id, table.transport_stream_id, service);
            }
        }
    }

    fn process_b10_eit(&mut self, table_id: u8, table: Eit) -> anyhow::Result<()> {
        self.store_section(
            (
                table_id,
                table.original_network_id,
                table.transport_stream_id,
                table.service_id,
                table.section_number,
            ),
            (table.version_number, table.crc_32),
            |registry| {
                table.events.iter().fold(true, |stored, event| {
                    registry.put_b10_event(table.service_id, event) && stored
                })
            },
        );

        if !self.is_watched_service(table.service_id) {
            return Ok(());
        }

        for event in &table.events {
            self.process_event(
                table.service_id,
                event.event_id,
                event.start_time,
                event.duration,
            )?;
        }

        Ok(())
    }

    fn process_m2_section_message(&mut self, message: M2SectionMessage) -> anyhow::Result<()> {
        match message.table {
            Table::MhEit(table) => self.process_mh_eit(table),
            Table::MhBit(table) => {
                self.process_mh_bit(table);
                Ok(())
            }
            Table::MhSdt(table) => {
                self.process_mh_sdt(table);
                Ok(())
            }
            _ => Ok(()),
        }
    }

    fn process_mh_eit(&mut self, table: MhEit) -> anyhow::Result<()> {
        self.store_section(
            (
                table.table_id,
                table.original_network_id,
                table.tlv_stream_id,
                table.service_id,
                table.section_number,
            ),
            (table.version_number, table.crc_32),
            |registry| {
                table.events.iter().fold(true, |stored, event| {
                    registry.put_event(table.service_id, event) && stored
                })
            },
        );

        if !self.is_watched_service(table.service_id) {
            return Ok(());
        }

        for event in &table.events {
            self.process_event(
                table.service_id,
                event.event_id,
                event.start_time,
                event.duration,
            )?;
        }

        Ok(())
    }

    fn process_mh_bit(&self, table: MhBit) {
        if let Some(registry) = &self.registry {
            for broadcaster in &table.broadcasters {
                registry.put_broadcaster(broadcaster);
            }
        }
    }

    fn process_mh_sdt(&self, table: MhSdt) {
        if let Some(registry) = &self.registry {
            for service in &table.services {
                registry.put_service(self.channel_id, table.tlv_stream_id, service);
            }
        }
    }

    /// Hands the events of an EIT section to the registry, unless it already
    /// holds them.
    ///
    /// A stream repeats every section every few seconds and only bumps
    /// `version_number` when its content changes, so remembering the version
    /// keeps the registry from rebuilding a schedule that did not move. The
    /// CRC guards against the version wrapping around its five bits.
    ///
    /// A section is only remembered once every event of it made it into the
    /// registry: one describing a service the registry does not know yet is
    /// dropped, and the next repetition has to retry it.
    fn store_section(
        &mut self,
        key: SectionKey,
        version: (u8, u32),
        store: impl FnOnce(&Registry) -> bool,
    ) {
        let Some(registry) = self.registry.clone() else {
            return;
        };
        if self.stored_sections.get(&key) == Some(&version) {
            return;
        }

        if store(&registry) {
            self.stored_sections.insert(key, version);
        }
    }

    fn process_event(
        &mut self,
        service_id: u16,
        event_id: u16,
        start_time: Option<chrono::NaiveDateTime>,
        duration: Option<chrono::TimeDelta>,
    ) -> anyhow::Result<()> {
        let Some((start_time, duration)) = start_time.zip(duration) else {
            return Ok(());
        };

        // The SI carries JST wall-clock time and the server runs on that zone,
        // so the local clock is the one the broadcast schedules against.
        let now = chrono::Local::now().naive_local();
        if now < start_time || start_time + duration <= now {
            return Ok(());
        }
        if self.current_event_id == Some(event_id) {
            return Ok(());
        }

        // The registry keeps events under a service it already knows, so an
        // EIT that arrives before the SDT is dropped. Waiting for the next
        // section keeps the announced event resolvable by whoever receives
        // the signal, instead of latching onto one nobody can look up.
        if let Some(registry) = &self.registry
            && registry.get_event_by_id(service_id, event_id).is_none()
        {
            return Ok(());
        }

        if let Some(signal_tx) = &self.signal_tx {
            // Nobody may be listening right now; that is fine.
            let _ = signal_tx.send(Signal::EventChanged { event_id });
        }
        self.current_event_id = Some(event_id);

        Ok(())
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
            transport_stream_id: 1,
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

    fn event_name_of(registry: &Registry, event_id: u16) -> Option<String> {
        registry.get_event_by_id(SERVICE_ID, event_id)?.name
    }

    fn sdt_of(service_id: u16) -> Sdt {
        Sdt {
            section_syntax_indicator: true,
            section_length: 0,
            transport_stream_id: 1,
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

    #[test]
    fn emits_the_current_event_only_once() {
        let (signal_tx, mut signal_rx) = tokio::sync::broadcast::channel(2);
        let mut processor = ServiceInformationProcessor::new(0, None, Some(signal_tx));
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
        let registry = Arc::new(Registry::default());
        let mut processor =
            ServiceInformationProcessor::new(0, Some(Arc::clone(&registry)), Some(signal_tx))
                .watching_service(Some(SERVICE_ID));

        processor
            .process(SignalingEvent::B10Table {
                table_id: SDT_ACTUAL_TABLE_ID,
                table: B10Table::Sdt(sdt_of(SERVICE_ID)),
            })
            .unwrap();
        processor
            .process(SignalingEvent::B10Table {
                table_id: SDT_ACTUAL_TABLE_ID,
                table: B10Table::Sdt(sdt_of(OTHER_SERVICE_ID)),
            })
            .unwrap();

        // The transport stream carries the EIT of every service it multiplexes.
        processor
            .process(signaling(B10Table::Eit(eit_on_air(
                OTHER_SERVICE_ID,
                0x0002,
            ))))
            .unwrap();
        processor
            .process(signaling(B10Table::Eit(eit_on_air(SERVICE_ID, 0x0001))))
            .unwrap();

        assert_eq!(processor.current_event_id(), Some(0x0001));
        assert!(matches!(
            signal_rx.try_recv(),
            Ok(Signal::EventChanged { event_id: 0x0001 })
        ));
        assert!(matches!(signal_rx.try_recv(), Err(TryRecvError::Empty)));

        // The schedule of the other service is still collected.
        assert!(registry.get_event_by_id(OTHER_SERVICE_ID, 0x0002).is_some());
    }

    #[test]
    fn stores_a_section_once_per_version() {
        let registry = Arc::new(Registry::default());
        let mut processor = ServiceInformationProcessor::new(0, Some(Arc::clone(&registry)), None);

        processor
            .process(SignalingEvent::B10Table {
                table_id: SDT_ACTUAL_TABLE_ID,
                table: B10Table::Sdt(sdt_of(SERVICE_ID)),
            })
            .unwrap();
        processor
            .process(signaling(B10Table::Eit(eit_named(
                SERVICE_ID,
                0x0001,
                "Programme",
            ))))
            .unwrap();

        // A section of a version already stored is dropped without reaching
        // the registry, which the rewritten name it carries here shows.
        processor
            .process(signaling(B10Table::Eit(eit_named(
                SERVICE_ID,
                0x0001,
                "Rewritten",
            ))))
            .unwrap();

        assert_eq!(
            event_name_of(&registry, 0x0001).as_deref(),
            Some("Programme")
        );

        // A new version of it is stored again.
        let mut updated = eit_named(SERVICE_ID, 0x0001, "Updated");
        updated.version_number = 1;
        processor
            .process(signaling(B10Table::Eit(updated)))
            .unwrap();

        assert_eq!(event_name_of(&registry, 0x0001).as_deref(), Some("Updated"));
    }

    #[test]
    fn waits_for_the_service_the_event_belongs_to() {
        let (signal_tx, mut signal_rx) = tokio::sync::broadcast::channel(2);
        let registry = Arc::new(Registry::default());
        let mut processor = ServiceInformationProcessor::new(0, Some(registry), Some(signal_tx))
            .watching_service(Some(SERVICE_ID));

        // An EIT ahead of the SDT describes a service the registry does not
        // know yet, so its event cannot be looked up.
        processor
            .process(signaling(B10Table::Eit(eit_on_air(SERVICE_ID, 0x0001))))
            .unwrap();

        assert_eq!(processor.current_event_id(), None);
        assert!(matches!(signal_rx.try_recv(), Err(TryRecvError::Empty)));

        processor
            .process(SignalingEvent::B10Table {
                table_id: SDT_ACTUAL_TABLE_ID,
                table: B10Table::Sdt(sdt_of(SERVICE_ID)),
            })
            .unwrap();
        processor
            .process(signaling(B10Table::Eit(eit_on_air(SERVICE_ID, 0x0001))))
            .unwrap();

        assert_eq!(processor.current_event_id(), Some(0x0001));
        assert!(matches!(
            signal_rx.try_recv(),
            Ok(Signal::EventChanged { event_id: 0x0001 })
        ));
    }
}
