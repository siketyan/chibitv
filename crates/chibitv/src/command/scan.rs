use std::collections::{BTreeMap, BTreeSet};
use std::time::{Duration, Instant};

use clap::Parser;
use toml_edit::{Array, ArrayOfTables, DocumentMut, InlineTable, Item, Table as TomlTable, Value};
use tracing::{info, warn};

use chibitv_b10::descriptor::Descriptor;
use chibitv_b10::table::{Nit, Sdt, ServiceInformation, Table};
use chibitv_b24::decode as decode_b24;
use chibitv_b25::B25Descrambler;

use crate::channel::{Channel, ChannelInner};
use crate::config::{ChannelConfig, ChannelConfigInner, Config, ServiceConfig};
use crate::m2ts::M2tsDemuxer;
use crate::remux::{Demux, Packet};
use crate::tuner::Tuners;

const FIRST_UHF_CHANNEL: u8 = 13;
const LAST_UHF_CHANNEL: u8 = 52;
const FIRST_UHF_FREQUENCY_HZ: u32 = 473_142_857;
const UHF_CHANNEL_BANDWIDTH_HZ: u32 = 6_000_000;

#[derive(Clone, Debug, Parser)]
pub struct Options {
    /// First UHF physical channel to scan.
    #[clap(long, default_value_t = FIRST_UHF_CHANNEL)]
    start_channel: u8,

    /// Last UHF physical channel to scan.
    #[clap(long, default_value_t = LAST_UHF_CHANNEL)]
    end_channel: u8,

    /// Maximum time in seconds to wait on each UHF channel.
    #[clap(long, default_value_t = 12)]
    timeout: u64,
}

#[derive(Clone, Debug, Default)]
struct ScanState {
    nit: Option<Nit>,
    transport_stream_id: Option<u16>,
    services: BTreeMap<u16, ServiceInformation>,
    sdt_sections: BTreeSet<u8>,
    sdt_last_section_number: Option<u8>,
    logged_networks: BTreeSet<u16>,
    logged_services: BTreeSet<u16>,
}

pub async fn scan(options: &Options, config: &Config) -> anyhow::Result<()> {
    if options.start_channel < FIRST_UHF_CHANNEL
        || options.end_channel > LAST_UHF_CHANNEL
        || options.start_channel > options.end_channel
    {
        anyhow::bail!(
            "UHF channel range must be within {}..={}",
            FIRST_UHF_CHANNEL,
            LAST_UHF_CHANNEL
        );
    }

    let mut tuners = Tuners::default();
    for (id, tuner) in config.tuners.iter().enumerate() {
        tuners.add_tuner_from_config(id as u32, tuner)?;
    }

    let Some(tuner) = tuners.get_tuner(0) else {
        anyhow::bail!("No tuners are configured");
    };

    let mut channels = Vec::new();
    for physical_channel in options.start_channel..=options.end_channel {
        let frequency = uhf_frequency(physical_channel);
        let channel = Channel {
            id: usize::from(physical_channel),
            name: format!("UHF {}", physical_channel),
            inner: ChannelInner::IsdbT {
                frequency,
                bandwidth_hz: UHF_CHANNEL_BANDWIDTH_HZ,
            },
        };

        info!(physical_channel, frequency, "Scanning UHF channel");

        if let Err(error) = tuner.tune(channel) {
            warn!(
                physical_channel,
                frequency, error = %error, "Could not tune to UHF channel"
            );
            continue;
        }

        let descrambler = B25Descrambler::open()?;
        let mut demux = M2tsDemuxer::new(tuner.open()?, descrambler);
        let mut state = ScanState::default();
        let deadline = Instant::now() + Duration::from_secs(options.timeout);

        while Instant::now() < deadline && !state.is_ready() {
            let packets = match demux.read() {
                Ok(Some(packets)) => packets,
                Ok(None) => break,
                Err(error) => {
                    warn!(
                        physical_channel,
                        frequency, error = %error, "Could not read transport stream"
                    );
                    continue;
                }
            };

            for packet in packets {
                let Packet::B10Table { table_id, table } = packet else {
                    continue;
                };

                state.read_table(physical_channel, table_id, table);

                if state.is_ready() {
                    break;
                }
            }
        }

        let Some(name) = state.channel_name() else {
            continue;
        };

        channels.push(ChannelConfig {
            name,
            transport_stream_id: state.transport_stream_id,
            services: state.service_configs(),
            inner: ChannelConfigInner::IsdbT {
                frequency,
                bandwidth_hz: UHF_CHANNEL_BANDWIDTH_HZ,
            },
        });
    }

    print!("{}", format_scan_output(&channels));

    Ok(())
}

impl ScanState {
    fn is_ready(&self) -> bool {
        self.nit.is_some()
            && self.sdt_last_section_number.is_some_and(|last_section| {
                self.sdt_sections.len() == usize::from(last_section) + 1
            })
            && !self.services.is_empty()
    }

    fn read_table(&mut self, physical_channel: u8, table_id: u8, table: Table) {
        match table {
            Table::Nit(nit) if table_id == 0x40 => self.read_nit(physical_channel, nit),
            Table::Sdt(sdt) if table_id == 0x42 => self.read_sdt(physical_channel, sdt),
            _ => {}
        }
    }

    fn read_nit(&mut self, physical_channel: u8, nit: Nit) {
        if self.logged_networks.insert(nit.network_id) {
            info!(
                physical_channel,
                network_id = nit.network_id,
                network_name = network_name(&nit).unwrap_or_default(),
                "Network found"
            );
        }

        if self.nit.is_none() {
            self.nit = Some(nit);
        }
    }

    fn read_sdt(&mut self, physical_channel: u8, sdt: Sdt) {
        self.transport_stream_id = Some(sdt.transport_stream_id);
        self.sdt_sections.insert(sdt.section_number);
        self.sdt_last_section_number = Some(sdt.last_section_number);

        for service in sdt.services {
            if self.logged_services.insert(service.service_id) {
                let descriptor = service_descriptor(&service).unwrap_or_default();

                info!(
                    physical_channel,
                    transport_stream_id = sdt.transport_stream_id,
                    service_id = service.service_id,
                    service_type = descriptor.service_type.unwrap_or_default(),
                    service_name = descriptor.service_name,
                    "Service found"
                );
            }

            self.services.insert(service.service_id, service);
        }
    }

    fn channel_name(&self) -> Option<String> {
        self.services
            .values()
            .find_map(service_name)
            .or_else(|| self.nit.as_ref().and_then(network_name))
    }

    fn service_configs(&self) -> Vec<ServiceConfig> {
        self.services
            .values()
            .filter_map(|service| {
                let descriptor = service_descriptor(service)?;
                (descriptor.service_type == Some(0x01)).then_some(ServiceConfig {
                    id: service.service_id,
                    name: descriptor.service_name,
                    provider_name: descriptor.provider_name,
                })
            })
            .collect()
    }
}

#[derive(Clone, Debug, Default)]
struct ServiceDescriptor {
    service_type: Option<u8>,
    service_name: String,
    provider_name: String,
}

fn uhf_frequency(channel: u8) -> u32 {
    FIRST_UHF_FREQUENCY_HZ + u32::from(channel - FIRST_UHF_CHANNEL) * UHF_CHANNEL_BANDWIDTH_HZ
}

fn network_name(nit: &Nit) -> Option<String> {
    nit.descriptors.iter().find_map(|descriptor| {
        let Descriptor::NetworkName(descriptor) = descriptor else {
            return None;
        };

        non_empty_text(&descriptor.network_name)
    })
}

fn service_descriptor(service: &ServiceInformation) -> Option<ServiceDescriptor> {
    service.descriptors.iter().find_map(|descriptor| {
        let Descriptor::Service(descriptor) = descriptor else {
            return None;
        };

        Some(ServiceDescriptor {
            service_type: Some(descriptor.service_type),
            service_name: text_bytes(&descriptor.service_name),
            provider_name: text_bytes(&descriptor.service_provider_name),
        })
    })
}

fn service_name(service: &ServiceInformation) -> Option<String> {
    let descriptor = service_descriptor(service)?;
    (!descriptor.service_name.is_empty()).then_some(descriptor.service_name)
}

fn non_empty_text(bytes: &[u8]) -> Option<String> {
    let text = text_bytes(bytes);
    (!text.is_empty()).then_some(text)
}

fn text_bytes(bytes: &[u8]) -> String {
    decode_b24(bytes)
}

fn format_scan_output(channels: &[ChannelConfig]) -> String {
    let mut channel_tables = ArrayOfTables::new();

    for channel in channels {
        let mut table = TomlTable::new();
        table["name"] = toml_edit::value(&channel.name);
        if let Some(transport_stream_id) = channel.transport_stream_id {
            table["transport_stream_id"] = toml_edit::value(i64::from(transport_stream_id));
        }

        match channel.inner {
            ChannelConfigInner::IsdbS {
                frequency,
                stream_id,
            } => {
                table["delivery_system"] = toml_edit::value("ISDB-S");
                table["frequency"] = toml_edit::value(i64::from(frequency));
                table["stream_id"] = toml_edit::value(i64::from(stream_id));
            }
            ChannelConfigInner::IsdbT {
                frequency,
                bandwidth_hz,
            } => {
                table["delivery_system"] = toml_edit::value("ISDB-T");
                table["frequency"] = toml_edit::value(i64::from(frequency));
                if bandwidth_hz != UHF_CHANNEL_BANDWIDTH_HZ {
                    table["bandwidth_hz"] = toml_edit::value(i64::from(bandwidth_hz));
                }
            }
        }

        if !channel.services.is_empty() {
            let mut services = Array::new();
            for service in &channel.services {
                let mut inline = InlineTable::new();
                inline.insert("id", Value::from(i64::from(service.id)));
                inline.insert("name", Value::from(service.name.clone()));
                if !service.provider_name.is_empty() {
                    inline.insert("provider_name", Value::from(service.provider_name.clone()));
                }
                services.push(inline);
            }
            table["services"] = Item::Value(Value::Array(services));
        }

        channel_tables.push(table);
    }

    let mut document = DocumentMut::new();
    document["channels"] = Item::ArrayOfTables(channel_tables);
    document.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serializes_service_catalog_under_physical_channel() {
        let channels = vec![ChannelConfig {
            name: "TOKYO MX".to_string(),
            transport_stream_id: Some(0x1234),
            services: vec![ServiceConfig {
                id: 0x5678,
                name: "TOKYO MX1".to_string(),
                provider_name: "TOKYO MX".to_string(),
            }],
            inner: ChannelConfigInner::IsdbT {
                frequency: 515_142_857,
                bandwidth_hz: 6_000_000,
            },
        }];

        let toml = format_scan_output(&channels);

        assert!(toml.contains("[[channels]]"));
        assert!(toml.contains("transport_stream_id = 4660"));
        assert!(!toml.contains("bandwidth_hz"));
        assert!(!toml.contains("[[channels.services]]"));
        assert!(toml.contains(
            "services = [{ id = 22136, name = \"TOKYO MX1\", provider_name = \"TOKYO MX\" }]"
        ));
    }
}
