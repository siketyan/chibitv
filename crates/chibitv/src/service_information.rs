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
#[allow(dead_code)]
pub enum Signal {
    EventChanged { event_id: u16 },
    ChannelChanged { service_id: u16 },
}

pub struct ServiceInformationProcessor {
    channel_id: usize,
    registry: Option<Arc<Registry>>,
    signal_tx: Option<Sender<Signal>>,
    current_event_id: Option<u16>,
}

impl ServiceInformationProcessor {
    pub fn new(
        channel_id: usize,
        registry: Option<Arc<Registry>>,
        signal_tx: Option<Sender<Signal>>,
    ) -> Self {
        Self {
            channel_id,
            registry,
            signal_tx,
            current_event_id: None,
        }
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
                self.process_b10_eit(table)
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

    fn process_b10_eit(&mut self, table: Eit) -> anyhow::Result<()> {
        for event in &table.events {
            if let Some(registry) = &self.registry {
                registry.put_b10_event(table.service_id, event);
            }

            self.process_event(event.event_id, event.start_time, event.duration)?;
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
        for event in &table.events {
            if let Some(registry) = &self.registry {
                registry.put_event(table.service_id, event);
            }

            self.process_event(event.event_id, event.start_time, event.duration)?;
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

    fn process_event(
        &mut self,
        event_id: u16,
        start_time: Option<chrono::NaiveDateTime>,
        duration: Option<chrono::TimeDelta>,
    ) -> anyhow::Result<()> {
        let Some((start_time, duration)) = start_time.zip(duration) else {
            return Ok(());
        };

        let now = chrono::Local::now().naive_local();
        if start_time <= now
            && now < start_time + duration
            && self.current_event_id != Some(event_id)
        {
            if let Some(signal_tx) = &self.signal_tx {
                signal_tx.send(Signal::EventChanged { event_id })?;
            }
            self.current_event_id = Some(event_id);
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use chrono::TimeDelta;
    use tokio::sync::broadcast::error::TryRecvError;

    use chibitv_b10::table::{Eit, EventInformation};

    use super::*;

    #[test]
    fn emits_the_current_event_only_once() {
        let now = chrono::Local::now().naive_local();
        let event_id = 0x1234;
        let eit = Eit {
            section_syntax_indicator: true,
            section_length: 0,
            service_id: 1,
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
        };
        let (signal_tx, mut signal_rx) = tokio::sync::broadcast::channel(2);
        let mut processor = ServiceInformationProcessor::new(0, None, Some(signal_tx));

        processor
            .process(SignalingEvent::B10Table {
                table_id: EIT_ACTUAL_PRESENT_FOLLOWING_TABLE_ID,
                table: B10Table::Eit(eit.clone()),
            })
            .unwrap();
        processor
            .process(SignalingEvent::B10Table {
                table_id: EIT_ACTUAL_PRESENT_FOLLOWING_TABLE_ID,
                table: B10Table::Eit(eit),
            })
            .unwrap();

        assert!(matches!(
            signal_rx.try_recv(),
            Ok(Signal::EventChanged { event_id: 0x1234 })
        ));
        assert!(matches!(signal_rx.try_recv(), Err(TryRecvError::Empty)));
    }
}
