use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use clap::Parser;
use tracing::info;

use chibitv_b10::descriptor::Descriptor;
use chibitv_b10::table::{Eit, EventInformation, Nit, Sdt, ServiceInformation, Table};
use chibitv_b24::decode as decode_b24;
use chibitv_b25::B25Descrambler;

use crate::channel::{Channel, ChannelInner};
use crate::config::Config;
use crate::demux::{Demux, Packet, SignalingEvent};
use crate::m2ts::M2tsDemuxer;
use crate::tuner::Tuners;

#[derive(Clone, Debug, Parser)]
pub struct Options {
    #[clap(short, long)]
    channel: usize,

    /// Maximum time in seconds to wait for SI tables before printing.
    #[clap(long, default_value_t = 3)]
    timeout: u64,
}

#[derive(Clone, Debug, Default)]
struct StatusState {
    nit: Option<Nit>,
    services: BTreeMap<u16, Option<ServiceInformation>>,
    current_events: BTreeMap<u16, EventInformation>,
}

pub async fn status(options: &Options, config: &Config) -> anyhow::Result<()> {
    let mut tuners = Tuners::default();
    for (id, tuner) in config.tuners.iter().enumerate() {
        tuners.add_tuner_from_config(id as u32, tuner)?;
    }

    let Some(tuner) = tuners.get_tuner(0) else {
        anyhow::bail!("No tuners are configured");
    };

    let Some(channel) = config.channels.get(options.channel).map(|channel| Channel {
        id: options.channel,
        name: channel.name.to_string(),
        inner: (&channel.inner).into(),
    }) else {
        anyhow::bail!("Could not find the channel in the config");
    };

    let ChannelInner::IsdbT { .. } = channel.inner else {
        anyhow::bail!("ISDB-T channels are only supported");
    };

    info!("Tuning to the channel: {:?}", channel);

    tuner.tune(channel.clone())?;

    let descrambler = B25Descrambler::open()?;
    let mut demux = M2tsDemuxer::new(tuner.open()?, descrambler);
    let mut state = StatusState::default();

    let deadline = Instant::now() + Duration::from_secs(options.timeout);
    while Instant::now() < deadline && !state.is_ready() {
        let packet = match demux.next_packet() {
            Ok(Some(packet)) => packet,
            Ok(None) => break,
            Err(_) => continue,
        };

        let Packet::Signaling(SignalingEvent::B10Table { table_id, table }) = packet else {
            continue;
        };

        state.read_table(Some(table_id), table);

        if state.is_ready() {
            break;
        }
    }

    state.print();

    Ok(())
}

impl StatusState {
    fn is_ready(&self) -> bool {
        if self.nit.is_none() || self.services.is_empty() {
            return false;
        }

        self.has_all_expected_service_descriptors() && self.has_all_current_events()
    }

    fn read_table(&mut self, table_id: Option<u8>, table: Table) {
        match table {
            Table::Nit(nit) => {
                if self.nit.is_none() {
                    self.read_expected_services(&nit);
                    self.nit = Some(nit);
                }
            }
            Table::Sdt(sdt) => self.read_sdt(sdt),
            Table::Eit(eit) if table_id == Some(0x4E) => self.read_eit(eit),
            _ => {}
        }
    }

    fn read_sdt(&mut self, sdt: Sdt) {
        for service in sdt.services {
            self.services.insert(service.service_id, Some(service));
        }
    }

    fn read_expected_services(&mut self, nit: &Nit) {
        for transport_stream in &nit.transport_streams {
            for descriptor in &transport_stream.descriptors {
                if let Descriptor::ServiceList(descriptor) = descriptor {
                    for service in &descriptor.services {
                        self.services.entry(service.service_id).or_insert(None);
                    }
                }
            }
        }
    }

    fn read_eit(&mut self, eit: Eit) {
        for event in eit.events {
            if is_current_event(&event) {
                self.current_events.entry(eit.service_id).or_insert(event);
            }
        }
    }

    fn has_all_expected_service_descriptors(&self) -> bool {
        self.services.values().all(|service| {
            service
                .as_ref()
                .is_some_and(|service| service.descriptors.iter().any(is_service_descriptor))
        })
    }

    fn has_all_current_events(&self) -> bool {
        self.services
            .keys()
            .all(|service_id| self.current_events.contains_key(service_id))
    }

    fn print(&self) {
        if let Some(nit) = &self.nit {
            let network_name = nit
                .descriptors
                .iter()
                .find_map(network_name_descriptor)
                .unwrap_or_default();

            println!(
                "Network: {:#06X} {}",
                nit.network_id,
                empty_as_unknown(&network_name),
            );
            println!("Services:");
        }

        for (&service_id, service) in &self.services {
            let Some(service) = service else {
                println!("  {:#06X}: (not found)", service_id);
                continue;
            };

            let service_descriptor = service
                .descriptors
                .iter()
                .find_map(service_descriptor)
                .unwrap_or_default();

            println!(
                "  {:#06X}: {}",
                service.service_id,
                empty_as_unknown(&service_descriptor.service_name),
            );
            println!(
                "    Provider: {}",
                empty_as_unknown(&service_descriptor.provider_name),
            );

            if let Some(service_type) = service_descriptor.service_type {
                println!("    Type: {:#04X}", service_type);
            }

            if let Some(event) = self.current_events.get(&service.service_id) {
                print_event(event, 4);
            } else {
                println!("    Event: (not found)");
            }
        }

        for (service_id, event) in &self.current_events {
            if self
                .services
                .get(service_id)
                .is_some_and(|service| service.is_some())
            {
                continue;
            }

            println!("  {:#06X}: (unknown service)", service_id);
            print_event(event, 4);
        }
    }
}

#[derive(Clone, Debug, Default)]
struct ServiceDescriptor {
    service_type: Option<u8>,
    provider_name: String,
    service_name: String,
}

#[derive(Clone, Debug, Default)]
struct ShortEventDescriptor {
    event_name: String,
    text: String,
}

fn network_name_descriptor(descriptor: &Descriptor) -> Option<String> {
    if let Descriptor::NetworkName(descriptor) = descriptor {
        Some(text_bytes(&descriptor.network_name))
    } else {
        None
    }
}

fn service_descriptor(descriptor: &Descriptor) -> Option<ServiceDescriptor> {
    let Descriptor::Service(descriptor) = descriptor else {
        return None;
    };

    Some(ServiceDescriptor {
        service_type: Some(descriptor.service_type),
        provider_name: text_bytes(&descriptor.service_provider_name),
        service_name: text_bytes(&descriptor.service_name),
    })
}

fn is_service_descriptor(descriptor: &Descriptor) -> bool {
    matches!(descriptor, Descriptor::Service(_))
}

fn short_event_descriptor(descriptor: &Descriptor) -> Option<ShortEventDescriptor> {
    let Descriptor::ShortEvent(descriptor) = descriptor else {
        return None;
    };

    Some(ShortEventDescriptor {
        event_name: text_bytes(&descriptor.event_name),
        text: text_bytes(&descriptor.text),
    })
}

fn print_event(event: &EventInformation, indent: usize) {
    let pad = " ".repeat(indent);
    let event_descriptor = event
        .descriptors
        .iter()
        .find_map(short_event_descriptor)
        .unwrap_or_default();

    println!(
        "{}Event: {:#06X} {}",
        pad,
        event.event_id,
        empty_as_unknown(&event_descriptor.event_name)
    );

    if !event_descriptor.text.is_empty() {
        println!("{}Description: {}", pad, event_descriptor.text);
    }

    if let Some(start_time) = event.start_time {
        if let Some(duration) = event.duration {
            let end_time = start_time + duration;
            println!(
                "{}Time: {} - {}  ({} min)",
                pad,
                start_time.format("%Y-%m-%d %H:%M:%S"),
                end_time.format("%H:%M:%S"),
                duration.num_minutes()
            );
        } else {
            println!("{}Time: {}", pad, start_time.format("%Y-%m-%d %H:%M:%S"));
        }
    }
}

fn is_current_event(event: &EventInformation) -> bool {
    let Some((start_time, duration)) = event.start_time.zip(event.duration) else {
        return false;
    };

    let now = chrono::Local::now().naive_local();
    start_time <= now && now < start_time + duration
}

fn text_bytes(bytes: &[u8]) -> String {
    decode_b24(bytes)
}

fn empty_as_unknown(value: &str) -> &str {
    if value.is_empty() { "(unknown)" } else { value }
}
