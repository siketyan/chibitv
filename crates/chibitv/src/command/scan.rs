use std::collections::{BTreeMap, BTreeSet};
use std::time::{Duration, Instant};

use clap::Parser;
use serde::Serialize;
use tracing::{info, warn};

use chibitv_b10::descriptor::Descriptor;
use chibitv_b10::table::{Nit, Sdt, ServiceInformation, Table};
use chibitv_b24::decode as decode_b24;
use chibitv_b25::B25Descrambler;

use crate::channel::{Channel, ChannelInner};
use crate::config::{ChannelConfig, ChannelConfigInner, Config};
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
    services: BTreeMap<u16, ServiceInformation>,
    logged_networks: BTreeSet<u16>,
    logged_services: BTreeSet<u16>,
}

#[derive(Clone, Debug, Serialize)]
struct ScanOutput {
    channels: Vec<ChannelConfig>,
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
                let Packet::B10Table { table, .. } = packet else {
                    continue;
                };

                state.read_table(physical_channel, frequency, table);

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
            inner: ChannelConfigInner::IsdbT {
                frequency,
                bandwidth_hz: UHF_CHANNEL_BANDWIDTH_HZ,
            },
        });
    }

    print!("{}", toml::to_string_pretty(&ScanOutput { channels })?);

    Ok(())
}

impl ScanState {
    fn is_ready(&self) -> bool {
        self.nit.is_some() && !self.services.is_empty()
    }

    fn read_table(&mut self, physical_channel: u8, frequency: u32, table: Table) {
        match table {
            Table::Nit(nit) => self.read_nit(physical_channel, frequency, nit),
            Table::Sdt(sdt) => self.read_sdt(physical_channel, frequency, sdt),
            _ => {}
        }
    }

    fn read_nit(&mut self, physical_channel: u8, frequency: u32, nit: Nit) {
        if self.logged_networks.insert(nit.network_id) {
            info!(
                physical_channel,
                frequency,
                network_id = nit.network_id,
                network_name = network_name(&nit).unwrap_or_default(),
                "Network found"
            );
        }

        if self.nit.is_none() {
            self.nit = Some(nit);
        }
    }

    fn read_sdt(&mut self, physical_channel: u8, frequency: u32, sdt: Sdt) {
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
}

#[derive(Clone, Debug, Default)]
struct ServiceDescriptor {
    service_type: Option<u8>,
    provider_name: String,
    service_name: String,
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
            provider_name: text_bytes(&descriptor.service_provider_name),
            service_name: text_bytes(&descriptor.service_name),
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
