use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use clap::{Parser, ValueEnum};
use toml_edit::{Array, ArrayOfTables, DocumentMut, InlineTable, Item, Table as TomlTable, Value};
use tracing::{info, warn};

use chibitv_b10::descriptor::Descriptor;
use chibitv_b10::table::{Nit, Sdt, ServiceInformation, Table};
use chibitv_b24::decode as decode_b24;
use chibitv_b25::B25Descrambler;

use crate::cas::PcscCasModule;
use crate::channel::{Channel, ChannelInner};
use crate::config::{ChannelConfig, ChannelConfigInner, Config, ServiceConfig};
use crate::demux::{Demux, Packet, SignalingEvent};
use crate::m2ts::M2tsDemuxer;
use crate::tuner::Tuners;

const FIRST_UHF_CHANNEL: u8 = 13;
const LAST_UHF_CHANNEL: u8 = 52;
const FIRST_UHF_FREQUENCY_HZ: u32 = 473_142_857;
const UHF_CHANNEL_BANDWIDTH_HZ: u32 = 6_000_000;

const BS_NETWORK_ID: u16 = 4;
const FIRST_BS_FREQUENCY_KHZ: u32 = 1_049_480;
const BS_FREQUENCY_STEP_KHZ: u32 = 38_360;
const LAST_BS_TRANSPONDER: u8 = 23;

const FIRST_CS110_FREQUENCY_KHZ: u32 = 1_613_000;
const CS110_FREQUENCY_STEP_KHZ: u32 = 40_000;
const LAST_CS110_TRANSPONDER: u8 = 24;

const TELEVISION_SERVICE_TYPE: u8 = 0x01;

/// The broadcast a scan walks.
#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub enum ScanDeliverySystem {
    /// The terrestrial UHF physical channels.
    #[value(name = "ISDB-T")]
    IsdbT,

    /// The BS and CS110 satellite transponders.
    #[value(name = "ISDB-S")]
    IsdbS,
}

#[derive(Clone, Debug, Parser)]
pub struct Options {
    /// Broadcast to scan.
    #[clap(long, value_enum, ignore_case = true, default_value = "ISDB-T")]
    delivery_system: ScanDeliverySystem,

    /// First UHF physical channel to scan. ISDB-T only.
    #[clap(long, default_value_t = FIRST_UHF_CHANNEL)]
    start_channel: u8,

    /// Last UHF physical channel to scan. ISDB-T only.
    #[clap(long, default_value_t = LAST_UHF_CHANNEL)]
    end_channel: u8,

    /// Maximum time in seconds to wait on each channel.
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
    let mut tuners = Tuners::default();
    for (id, tuner) in config.tuners.iter().enumerate() {
        tuners.add_tuner_from_config(id as u32, tuner)?;
    }

    if tuners.is_in_use(0).is_none() {
        anyhow::bail!("No tuners are configured");
    }

    let cas = PcscCasModule::open_shared()?;
    let timeout = Duration::from_secs(options.timeout);
    let channels = match options.delivery_system {
        ScanDeliverySystem::IsdbT => scan_terrestrial(options, &tuners, &cas, timeout)?,
        ScanDeliverySystem::IsdbS => scan_satellite(&tuners, &cas, timeout)?,
    };

    print!("{}", format_scan_output(&channels));

    Ok(())
}

/// Walks the terrestrial UHF band, one physical channel at a time.
fn scan_terrestrial(
    options: &Options,
    tuners: &Tuners,
    cas: &Arc<PcscCasModule>,
    timeout: Duration,
) -> anyhow::Result<Vec<ChannelConfig>> {
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

    let mut channels = Vec::new();
    for physical_channel in options.start_channel..=options.end_channel {
        let frequency = uhf_frequency(physical_channel);
        let inner = ChannelInner::IsdbT {
            frequency,
            bandwidth_hz: UHF_CHANNEL_BANDWIDTH_HZ,
        };

        info!(physical_channel, frequency, "Scanning UHF channel");

        let label = format!("UHF {physical_channel}");
        let Some(state) =
            read_channel(tuners, cas, &label, inner, timeout, ScanState::is_complete)?
        else {
            continue;
        };
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

    Ok(channels)
}

/// Walks the BS and CS110 transponders.
///
/// A satellite transport stream is picked by its id rather than by a channel
/// number, and which ids are on air changes as broadcasters come and go, so the
/// frequencies below are only a way in: the first transponder that answers hands
/// over its network's NIT, which names every transport stream of that network
/// and the transponder each one sits on. What is left is to tune to the ones
/// carrying television and read their service catalog.
fn scan_satellite(
    tuners: &Tuners,
    cas: &Arc<PcscCasModule>,
    timeout: Duration,
) -> anyhow::Result<Vec<ChannelConfig>> {
    let streams = discover_satellite_streams(tuners, cas, timeout)?;
    if streams.is_empty() {
        warn!("No satellite network answered: is the dish connected and its converter powered?");
    }

    let mut channels = Vec::new();
    for stream in streams.values() {
        let transport_stream_id = stream.transport_stream_id;
        let frequency = stream.frequency_khz;
        let inner = ChannelInner::IsdbS {
            frequency,
            stream_id: u32::from(transport_stream_id),
        };

        info!(
            transport_stream_id,
            frequency, "Scanning satellite transport stream"
        );

        let label = format!("TSID {transport_stream_id:#06X}");
        let Some(state) = read_channel(
            tuners,
            cas,
            &label,
            inner,
            timeout,
            ScanState::has_service_catalog,
        )?
        else {
            continue;
        };
        let Some(name) = state.channel_name() else {
            continue;
        };

        channels.push(ChannelConfig {
            name,
            // The network named the stream before it was tuned to, so its id is
            // known even when its own SDT did not arrive in time.
            transport_stream_id: state.transport_stream_id.or(Some(transport_stream_id)),
            services: state.service_configs(),
            inner: ChannelConfigInner::IsdbS {
                frequency,
                stream_id: u32::from(transport_stream_id),
            },
        });
    }

    Ok(channels)
}

/// Finds every satellite transport stream worth tuning to, by its id.
fn discover_satellite_streams(
    tuners: &Tuners,
    cas: &Arc<PcscCasModule>,
    timeout: Duration,
) -> anyhow::Result<BTreeMap<u16, SatelliteStream>> {
    let mut discovered = BTreeMap::<u16, SatelliteStream>::new();

    for transponder in transponders() {
        // A network describes every transponder it uses, so one already
        // accounted for is not probed again. BS is a single network across all
        // of its transponders, which is what keeps this to a couple of tunes.
        if discovered
            .values()
            .any(|stream| stream.frequency_khz == transponder.frequency_khz)
        {
            continue;
        }

        for stream_id in transponder.seed_stream_ids() {
            info!(
                transponder = transponder.name,
                frequency = transponder.frequency_khz,
                stream_id,
                "Probing satellite transponder"
            );

            let inner = ChannelInner::IsdbS {
                frequency: transponder.frequency_khz,
                stream_id,
            };
            // Any transport stream of a transponder carries the NIT of the
            // whole network, so there is nothing else to wait for here.
            let Some(state) =
                read_channel(tuners, cas, &transponder.name, inner, timeout, |state| {
                    state.nit.is_some()
                })?
            else {
                // Nothing is coming off this transponder, and a stream id is
                // not what would change that: the tuner locks on to the
                // transponder rather than on to one stream of it.
                break;
            };

            // It answered but said nothing, so the id may not be one the driver
            // picks a stream with. The next one is worth a try.
            let Some(nit) = state.nit else {
                continue;
            };

            let found = satellite_streams(&nit);
            info!(
                transponder = transponder.name,
                network_id = nit.network_id,
                streams = found.len(),
                "Satellite network found"
            );
            discovered.extend(
                found
                    .into_iter()
                    .map(|stream| (stream.transport_stream_id, stream)),
            );

            break;
        }
    }

    Ok(discovered)
}

/// One transport stream of a satellite network, as its NIT describes it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct SatelliteStream {
    transport_stream_id: u16,
    /// The frequency the dish hands the tuner, in kHz.
    frequency_khz: u32,
}

/// The transport streams a NIT describes that are worth tuning to.
///
/// One the network says carries no television is left out, as is one whose
/// transponder it does not name, which there would be no way to tune to.
fn satellite_streams(nit: &Nit) -> Vec<SatelliteStream> {
    nit.transport_streams
        .iter()
        .filter(|stream| carries_television(&stream.descriptors))
        .filter_map(|stream| {
            let frequency_khz = stream.descriptors.iter().find_map(|descriptor| {
                let Descriptor::SatelliteDeliverySystem(descriptor) = descriptor else {
                    return None;
                };

                descriptor.intermediate_frequency_khz()
            })?;

            Some(SatelliteStream {
                transport_stream_id: stream.transport_stream_id,
                frequency_khz,
            })
        })
        .collect()
}

/// Whether the services the network lists for a transport stream include
/// television.
///
/// A stream the network lists no services of at all counts as carrying it: its
/// own SDT is what settles that, and reading it means tuning to it.
fn carries_television(descriptors: &[Descriptor]) -> bool {
    let mut listed = descriptors
        .iter()
        .filter_map(|descriptor| match descriptor {
            Descriptor::ServiceList(descriptor) => Some(&descriptor.services),
            _ => None,
        })
        .flatten()
        .peekable();

    listed.peek().is_none() || listed.any(|service| service.service_type == TELEVISION_SERVICE_TYPE)
}

/// A transponder the scan reaches for before the network has described itself.
#[derive(Clone, Debug, Eq, PartialEq)]
struct Transponder {
    /// How the transponder is named, as `BS-15` or `ND2`.
    name: String,
    /// The network its transport streams belong to.
    network_id: u16,
    /// The transponder number, which its transport stream ids are built from.
    number: u8,
    /// The frequency the dish hands the tuner, in kHz.
    frequency_khz: u32,
}

impl Transponder {
    /// The stream ids to reach the transponder with, before anything on air is
    /// known.
    ///
    /// ARIB numbers a satellite transport stream after the transponder it sits
    /// on and its place on it, so the first two streams of a transponder can be
    /// named without having heard from the network yet. A relative number comes
    /// first, for the drivers that pick a stream by its place instead.
    fn seed_stream_ids(&self) -> impl Iterator<Item = u32> {
        [
            0,
            u32::from(satellite_stream_id(self.network_id, self.number, 0)),
            u32::from(satellite_stream_id(self.network_id, self.number, 1)),
        ]
        .into_iter()
    }
}

/// The transport stream id ARIB gives the `index`th stream of a transponder.
fn satellite_stream_id(network_id: u16, transponder: u8, index: u8) -> u16 {
    (network_id << 12) | (u16::from(transponder) << 4) | u16::from(index)
}

/// Every transponder of the satellites Japan broadcasts from, BS first.
fn transponders() -> impl Iterator<Item = Transponder> {
    let bs = (1..=LAST_BS_TRANSPONDER)
        .step_by(2)
        .map(|number| Transponder {
            name: format!("BS-{number}"),
            network_id: BS_NETWORK_ID,
            number,
            frequency_khz: FIRST_BS_FREQUENCY_KHZ
                + u32::from(number - 1) / 2 * BS_FREQUENCY_STEP_KHZ,
        });

    let cs110 = (2..=LAST_CS110_TRANSPONDER)
        .step_by(2)
        .map(|number| Transponder {
            name: format!("ND{number}"),
            // CS110 is two networks sharing the dish, one on every other
            // transponder.
            network_id: if number % 4 == 2 { 6 } else { 7 },
            number,
            frequency_khz: FIRST_CS110_FREQUENCY_KHZ
                + u32::from(number - 2) / 2 * CS110_FREQUENCY_STEP_KHZ,
        });

    bs.chain(cs110)
}

/// Tunes to one physical channel and reads its tables until `is_done` is
/// satisfied or the time runs out.
///
/// A channel nothing could be tuned to is reported as nothing found rather than
/// as an error, so that the rest of the scan carries on.
fn read_channel(
    tuners: &Tuners,
    cas: &Arc<PcscCasModule>,
    label: &str,
    inner: ChannelInner,
    timeout: Duration,
    is_done: impl Fn(&ScanState) -> bool,
) -> anyhow::Result<Option<ScanState>> {
    let channel = Channel {
        id: 0,
        name: label.to_string(),
        inner,
    };

    let tuner = tuners.try_acquire_by_id(0)?;
    if let Err(error) = tuner.tune(channel) {
        warn!(channel = label, error = %error, "Could not tune to the channel");
        return Ok(None);
    }

    let descrambler = B25Descrambler::init(cas.clone())?;
    let mut demux = M2tsDemuxer::new(tuner.open()?, descrambler);
    let mut state = ScanState::default();
    let deadline = Instant::now() + timeout;

    while Instant::now() < deadline && !is_done(&state) {
        let packet = match demux.next_packet() {
            Ok(Some(packet)) => packet,
            Ok(None) => break,
            Err(error) => {
                warn!(channel = label, error = %error, "Could not read transport stream");
                continue;
            }
        };

        let Packet::Signaling(SignalingEvent::B10Table { table_id, table }) = packet else {
            continue;
        };

        state.read_table(label, table_id, table);
    }

    Ok(Some(state))
}

impl ScanState {
    /// Whether everything a channel is scanned for has arrived.
    fn is_complete(&self) -> bool {
        self.nit.is_some() && self.has_service_catalog()
    }

    /// Whether every service the channel carries has been named.
    ///
    /// This is all a satellite scan waits for: it heard the network out on the
    /// transponder it came in on, and a NIT repeats far more slowly than an SDT
    /// does, so waiting for another one on every stream would be most of what
    /// the scan spends its time on.
    fn has_service_catalog(&self) -> bool {
        self.sdt_last_section_number
            .is_some_and(|last_section| self.sdt_sections.len() == usize::from(last_section) + 1)
            && !self.services.is_empty()
    }

    fn read_table(&mut self, channel: &str, table_id: u8, table: Table) {
        match table {
            Table::Nit(nit) if table_id == 0x40 => self.read_nit(channel, nit),
            Table::Sdt(sdt) if table_id == 0x42 => self.read_sdt(channel, sdt),
            _ => {}
        }
    }

    fn read_nit(&mut self, channel: &str, nit: Nit) {
        if self.logged_networks.insert(nit.network_id) {
            info!(
                channel,
                network_id = nit.network_id,
                network_name = network_name(&nit).unwrap_or_default(),
                "Network found"
            );
        }

        if self.nit.is_none() {
            self.nit = Some(nit);
        }
    }

    fn read_sdt(&mut self, channel: &str, sdt: Sdt) {
        self.transport_stream_id = Some(sdt.transport_stream_id);
        self.sdt_sections.insert(sdt.section_number);
        self.sdt_last_section_number = Some(sdt.last_section_number);

        for service in sdt.services {
            if self.logged_services.insert(service.service_id) {
                let descriptor = service_descriptor(&service).unwrap_or_default();

                info!(
                    channel,
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
                (descriptor.service_type == Some(TELEVISION_SERVICE_TYPE)).then_some(
                    ServiceConfig {
                        id: service.service_id,
                        name: descriptor.service_name,
                        provider_name: descriptor.provider_name,
                    },
                )
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
            ChannelConfigInner::IsdbS {
                frequency,
                stream_id,
            } => {
                table["delivery_system"] = toml_edit::value("ISDB-S");
                table["frequency"] = toml_edit::value(i64::from(frequency));
                table["stream_id"] = toml_edit::value(i64::from(stream_id));
            }
            ChannelConfigInner::IsdbS3 {
                frequency,
                stream_id,
            } => {
                table["delivery_system"] = toml_edit::value("ISDB-S3");
                table["frequency"] = toml_edit::value(i64::from(frequency));
                table["stream_id"] = toml_edit::value(i64::from(stream_id));
            }
            ChannelConfigInner::BonIsdbT {
                space,
                channel: number,
            } => {
                table["delivery_system"] = toml_edit::value("Bon-ISDB-T");
                table["space"] = toml_edit::value(i64::from(space));
                table["channel"] = toml_edit::value(i64::from(number));
            }
            ChannelConfigInner::BonIsdbS {
                space,
                channel: number,
            } => {
                table["delivery_system"] = toml_edit::value("Bon-ISDB-S");
                table["space"] = toml_edit::value(i64::from(space));
                table["channel"] = toml_edit::value(i64::from(number));
            }
            ChannelConfigInner::BonIsdbS3 {
                space,
                channel: number,
            } => {
                table["delivery_system"] = toml_edit::value("Bon-ISDB-S3");
                table["space"] = toml_edit::value(i64::from(space));
                table["channel"] = toml_edit::value(i64::from(number));
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
    use chibitv_b10::descriptor::{
        SatelliteDeliverySystemDescriptor, ServiceListDescriptor, ServiceListItem,
    };
    use chibitv_b10::table::TransportStreamInformation;

    use super::*;

    fn satellite_descriptor(frequency_khz: u32) -> Descriptor {
        Descriptor::SatelliteDeliverySystem(SatelliteDeliverySystemDescriptor {
            frequency_khz,
            orbital_position: 1100,
            west_east_flag: true,
            polarisation: 0x01,
            modulation: 0x0B,
            symbol_rate: 288_600,
            fec_inner: 0x0F,
        })
    }

    fn service_list(service_types: &[u8]) -> Descriptor {
        Descriptor::ServiceList(ServiceListDescriptor {
            services: service_types
                .iter()
                .enumerate()
                .map(|(index, service_type)| ServiceListItem {
                    service_id: 0x0400 + index as u16,
                    service_type: *service_type,
                })
                .collect(),
        })
    }

    fn nit(transport_streams: Vec<TransportStreamInformation>) -> Nit {
        Nit {
            section_syntax_indicator: true,
            section_length: 0,
            network_id: BS_NETWORK_ID,
            version_number: 0,
            current_next_indicator: true,
            section_number: 0,
            last_section_number: 0,
            descriptors: vec![],
            transport_streams,
            crc_32: 0,
        }
    }

    #[test]
    fn places_every_transponder_of_both_satellites() {
        let transponders = transponders().collect::<Vec<_>>();

        // BS is the twelve odd numbers, CS110 the twelve even ones.
        assert_eq!(transponders.len(), 24);

        let first = transponders.first().unwrap();
        assert_eq!(first.name, "BS-1");
        assert_eq!(first.frequency_khz, FIRST_BS_FREQUENCY_KHZ);

        let of = |name: &str| {
            transponders
                .iter()
                .find(|transponder| transponder.name == name)
                .unwrap()
                .clone()
        };

        // BS-15 is where the 4K broadcasts sit, at the frequency every tuning
        // table for it names.
        assert_eq!(of("BS-15").frequency_khz, 1_318_000);
        assert_eq!(of("BS-15").network_id, BS_NETWORK_ID);
        assert_eq!(of("ND2").frequency_khz, 1_613_000);
        assert_eq!(of("ND2").network_id, 6);
        // The two CS110 networks sit on every other transponder.
        assert_eq!(of("ND4").frequency_khz, 1_653_000);
        assert_eq!(of("ND4").network_id, 7);

        let last = transponders.last().unwrap();
        assert_eq!(last.name, "ND24");
        assert_eq!(last.frequency_khz, 2_053_000);
    }

    #[test]
    fn names_the_streams_of_a_transponder_the_way_arib_does() {
        // BS-15 carries 0x40F1, as every tuning table for it says.
        assert_eq!(satellite_stream_id(BS_NETWORK_ID, 15, 1), 0x40F1);
        assert_eq!(satellite_stream_id(BS_NETWORK_ID, 3, 1), 0x4031);
        assert_eq!(satellite_stream_id(BS_NETWORK_ID, 23, 0), 0x4170);
        assert_eq!(satellite_stream_id(6, 2, 0), 0x6020);
        assert_eq!(satellite_stream_id(7, 4, 0), 0x7040);
    }

    #[test]
    fn reaches_a_transponder_by_its_place_before_its_stream_ids() {
        let transponder = transponders().next().unwrap();

        assert_eq!(
            transponder.seed_stream_ids().collect::<Vec<_>>(),
            [0, 0x4010, 0x4011]
        );
    }

    #[test]
    fn reads_the_transport_streams_of_a_satellite_network_from_its_nit() {
        let streams = satellite_streams(&nit(vec![
            TransportStreamInformation {
                transport_stream_id: 0x40F1,
                original_network_id: BS_NETWORK_ID,
                descriptors: vec![
                    satellite_descriptor(11_996_000),
                    service_list(&[TELEVISION_SERVICE_TYPE]),
                ],
            },
            // Data only, so there is nothing to watch on it.
            TransportStreamInformation {
                transport_stream_id: 0x40F2,
                original_network_id: BS_NETWORK_ID,
                descriptors: vec![satellite_descriptor(11_996_000), service_list(&[0xC0])],
            },
            // The network says nothing about its services, so its own SDT is
            // what settles them.
            TransportStreamInformation {
                transport_stream_id: 0x4031,
                original_network_id: BS_NETWORK_ID,
                descriptors: vec![satellite_descriptor(11_765_840)],
            },
            // Nothing says which transponder it is on, so it cannot be tuned.
            TransportStreamInformation {
                transport_stream_id: 0x4111,
                original_network_id: BS_NETWORK_ID,
                descriptors: vec![service_list(&[TELEVISION_SERVICE_TYPE])],
            },
        ]));

        assert_eq!(
            streams,
            [
                SatelliteStream {
                    transport_stream_id: 0x40F1,
                    frequency_khz: 1_318_000,
                },
                SatelliteStream {
                    transport_stream_id: 0x4031,
                    frequency_khz: 1_087_840,
                },
            ]
        );
    }

    fn television_service(service_id: u16, name: &str) -> ServiceInformation {
        ServiceInformation {
            service_id,
            eit_user_defined_flags: 0,
            eit_schedule_flag: true,
            eit_present_following_flag: true,
            running_status: 4,
            free_ca_mode: true,
            descriptors: vec![Descriptor::Service(
                chibitv_b10::descriptor::ServiceDescriptor {
                    service_type: TELEVISION_SERVICE_TYPE,
                    service_provider_name: b"NHK".to_vec(),
                    service_name: name.as_bytes().to_vec(),
                },
            )],
        }
    }

    #[test]
    fn stops_a_satellite_stream_at_its_service_catalog() {
        let mut state = ScanState {
            sdt_last_section_number: Some(0),
            ..ScanState::default()
        };
        state.sdt_sections.insert(0);
        state
            .services
            .insert(0x0400, television_service(0x0400, "BS"));

        // The network was heard out on the transponder the stream came in on,
        // so there is nothing left for a satellite scan to wait for.
        assert!(state.has_service_catalog());
        // A terrestrial scan has not heard it yet, and still waits.
        assert!(!state.is_complete());
    }

    #[test]
    fn waits_for_the_rest_of_a_service_catalog_that_is_still_arriving() {
        let mut state = ScanState {
            sdt_last_section_number: Some(1),
            ..ScanState::default()
        };
        state.sdt_sections.insert(0);
        state
            .services
            .insert(0x0400, television_service(0x0400, "BS"));

        assert!(!state.has_service_catalog());

        state.sdt_sections.insert(1);

        assert!(state.has_service_catalog());
    }

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

    #[test]
    fn serializes_a_satellite_channel_with_the_stream_it_is_picked_by() {
        let channels = vec![ChannelConfig {
            name: "NHK BS".to_string(),
            transport_stream_id: Some(0x4031),
            services: vec![ServiceConfig {
                id: 101,
                name: "NHK BS".to_string(),
                provider_name: "NHK".to_string(),
            }],
            inner: ChannelConfigInner::IsdbS {
                frequency: 1_087_840,
                stream_id: 0x4031,
            },
        }];

        let toml = format_scan_output(&channels);

        assert!(toml.contains("delivery_system = \"ISDB-S\""));
        assert!(toml.contains("frequency = 1087840"));
        assert!(toml.contains("stream_id = 16433"));
    }
}
