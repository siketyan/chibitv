//! Scanning the broadcasts carried on MPEG-2 TS: the terrestrial channels and
//! the 2K satellite ones.

use std::collections::{BTreeMap, BTreeSet};
use std::time::Instant;

use tracing::{info, warn};

use chibitv_b10::descriptor::Descriptor;
use chibitv_b10::table::{Nit, Sdt, ServiceInformation, Table};
use chibitv_b24::decode as decode_b24;
use chibitv_b25::B25Descrambler;

use super::{
    FAST_2K_TRANSPONDERS, FIRST_UHF_CHANNEL, FIRST_UHF_FREQUENCY_HZ, Scan, ScanRequest,
    ServiceDescriptor, TELEVISION_SERVICE_TYPE, Transponder, UHF_CHANNEL_BANDWIDTH_HZ,
    transponders,
};
use crate::channel::ChannelInner;
use crate::demux::{Demux, Packet, SignalingEvent, is_descrambling_refused};
use crate::m2ts::M2tsDemuxer;
use crate::store::{NewChannel, StoredService};
use crate::task::TaskHandle;

const SDT_ACTUAL_TABLE_ID: u8 = 0x42;
const SDT_OTHER_TABLE_ID: u8 = 0x46;

#[derive(Clone, Debug, Default)]
struct ScanState {
    nit: Option<Nit>,
    /// Whether the streams the network describes beside the one being tuned
    /// are collected as well, which is what a fast scan is after.
    reads_other_streams: bool,
    /// The stream being tuned, once its own SDT has said which it is.
    transport_stream_id: Option<u16>,
    /// The services of each stream, by the stream carrying them.
    streams: BTreeMap<u16, BTreeMap<u16, ServiceInformation>>,
    sdt_sections: BTreeSet<u8>,
    sdt_last_section_number: Option<u8>,
    logged_networks: BTreeSet<u16>,
    logged_services: BTreeSet<u16>,
}
/// Walks the terrestrial UHF band, one physical channel at a time.
pub(super) fn scan_terrestrial(
    scan: &Scan,
    request: &ScanRequest,
) -> anyhow::Result<Vec<NewChannel>> {
    let channels_to_scan = request.uhf_channels.clone();
    let scanned = channels_to_scan.clone().count();
    let mut channels = Vec::new();
    for (index, physical_channel) in channels_to_scan.enumerate() {
        let frequency = uhf_frequency(physical_channel);
        let inner = ChannelInner::IsdbT {
            frequency,
            bandwidth_hz: UHF_CHANNEL_BANDWIDTH_HZ,
        };

        info!(physical_channel, frequency, "Scanning UHF channel");

        let label = format!("UHF {physical_channel}");
        if !scan.report(index, scanned, format!("Scanning {label}")) {
            break;
        }

        let Some(state) =
            scan.read_channel(&label, inner, ScanState::default(), ScanState::is_complete)?
        else {
            continue;
        };
        let Some(name) = state.channel_name(state.transport_stream_id) else {
            continue;
        };

        channels.push(NewChannel {
            name,
            transport_stream_id: state.transport_stream_id,
            services: state.services(state.transport_stream_id),
            inner: ChannelInner::IsdbT {
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
pub(super) fn scan_satellite(scan: &Scan) -> anyhow::Result<Vec<NewChannel>> {
    let streams = discover_satellite_streams(scan)?;
    if streams.is_empty() {
        warn!("No satellite network answered: is the dish connected and its converter powered?");
    }

    let mut channels = Vec::new();
    for (index, stream) in streams.values().enumerate() {
        let transport_stream_id = stream.transport_stream_id;
        let frequency = stream.frequency_khz;
        if !scan.report(
            index,
            streams.len(),
            format!("Scanning TSID {transport_stream_id:#06X}"),
        ) {
            break;
        }

        let inner = ChannelInner::IsdbS {
            frequency,
            stream_id: u32::from(transport_stream_id),
        };

        info!(
            transport_stream_id,
            frequency, "Scanning satellite transport stream"
        );

        let label = format!("TSID {transport_stream_id:#06X}");
        let Some(state) = scan.read_channel(
            &label,
            inner,
            ScanState::default(),
            ScanState::has_service_catalog,
        )?
        else {
            continue;
        };
        let Some(name) = state.channel_name(Some(transport_stream_id)) else {
            continue;
        };

        channels.push(NewChannel {
            name,
            transport_stream_id: Some(transport_stream_id),
            services: state.services(Some(transport_stream_id)),
            inner: ChannelInner::IsdbS {
                frequency,
                stream_id: u32::from(transport_stream_id),
            },
        });
    }

    Ok(channels)
}

/// Reads the whole channel list out of the signalling on one transponder per
/// network.
///
/// A satellite network describes itself in full: its NIT names every stream it
/// is made of and the transponder each one sits on, and every stream carries
/// the service description of the others beside its own. So one that answers is
/// enough to write the lot down, without tuning to a single one of them.
pub(super) fn scan_satellite_fast(scan: &Scan) -> anyhow::Result<Vec<NewChannel>> {
    let mut channels = Vec::new();

    for transponder in
        transponders().filter(|transponder| FAST_2K_TRANSPONDERS.contains(&transponder.number))
    {
        let Some(state) = scan.read_network(
            &transponder,
            ScanState {
                reads_other_streams: true,
                ..ScanState::default()
            },
            ScanState::has_every_stream,
        )?
        else {
            continue;
        };
        let Some(nit) = &state.nit else {
            warn!(transponder = transponder.name, "The network said nothing");
            continue;
        };

        for stream in satellite_streams(nit) {
            let transport_stream_id = stream.transport_stream_id;
            let Some(name) = state.channel_name(Some(transport_stream_id)) else {
                continue;
            };

            channels.push(NewChannel {
                name,
                transport_stream_id: Some(transport_stream_id),
                services: state.services(Some(transport_stream_id)),
                inner: ChannelInner::IsdbS {
                    frequency: stream.frequency_khz,
                    stream_id: u32::from(transport_stream_id),
                },
            });
        }
    }

    Ok(channels)
}

/// Finds every satellite transport stream worth tuning to, by its id.
fn discover_satellite_streams(scan: &Scan) -> anyhow::Result<BTreeMap<u16, SatelliteStream>> {
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

        // Any transport stream of a transponder carries the NIT of the whole
        // network, so there is nothing else to wait for here.
        let Some(state) = scan.read_network(&transponder, ScanState::default(), |state| {
            state.nit.is_some()
        })?
        else {
            continue;
        };
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

impl Scan<'_> {
    /// Tunes to one channel and reads its tables until `is_done` is satisfied
    /// or the time runs out.
    ///
    /// A channel nothing could be tuned to is reported as nothing found rather
    /// than as an error, so that the rest of the scan carries on.
    fn read_channel(
        &self,
        label: &str,
        inner: ChannelInner,
        mut state: ScanState,
        is_done: impl Fn(&ScanState) -> bool,
    ) -> anyhow::Result<Option<ScanState>> {
        let Some(input) = self.tune(label, inner)? else {
            return Ok(None);
        };

        let descrambler = B25Descrambler::init(self.cas.clone(), false)?;
        let mut demux = M2tsDemuxer::new(input, descrambler);
        let mut refused = false;
        let deadline = Instant::now() + self.timeout;

        while Instant::now() < deadline && !is_done(&state) {
            if self.task.is_some_and(TaskHandle::is_cancelled) {
                break;
            }

            let packet = match demux.next_packet() {
                Ok(Some(packet)) => packet,
                Ok(None) => break,
                // The tables are not scrambled, so the scan reads past a card
                // that will not unscramble the rest of the channel.
                Err(error) if is_descrambling_refused(&error) => {
                    if !std::mem::replace(&mut refused, true) {
                        warn!(channel = label, %error, "Scanning the tables only");
                    }

                    continue;
                }
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

    /// Tunes to a transponder and reads the network out of the signalling
    /// there, trying each seed stream id in turn.
    ///
    /// Nothing coming off the transponder is the end of it, while an id the
    /// driver does not pick a stream with is worth another try.
    fn read_network(
        &self,
        transponder: &Transponder,
        state: ScanState,
        is_done: impl Fn(&ScanState) -> bool + Copy,
    ) -> anyhow::Result<Option<ScanState>> {
        if let Some(task) = self.task {
            task.report(None, format!("Probing {}", transponder.name));
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
            let Some(read) = self.read_channel(&transponder.name, inner, state.clone(), is_done)?
            else {
                break;
            };

            if read.nit.is_some() {
                return Ok(Some(read));
            }
        }

        Ok(None)
    }
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
            && !self.streams.is_empty()
    }

    /// Whether every stream the network describes has been named.
    ///
    /// This is what a fast scan waits for, having no intention of tuning to any
    /// of them.
    fn has_every_stream(&self) -> bool {
        let Some(nit) = &self.nit else {
            return false;
        };

        satellite_streams(nit)
            .iter()
            .all(|stream| self.streams.contains_key(&stream.transport_stream_id))
    }

    fn read_table(&mut self, channel: &str, table_id: u8, table: Table) {
        match table {
            Table::Nit(nit) if table_id == 0x40 => self.read_nit(channel, nit),
            Table::Sdt(sdt) if table_id == SDT_ACTUAL_TABLE_ID => self.read_sdt(channel, sdt, true),
            Table::Sdt(sdt) if table_id == SDT_OTHER_TABLE_ID && self.reads_other_streams => {
                self.read_sdt(channel, sdt, false)
            }
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

    /// Reads one service description, of the stream being tuned or of another
    /// the network describes beside it.
    fn read_sdt(&mut self, channel: &str, sdt: Sdt, is_actual: bool) {
        if is_actual {
            self.transport_stream_id = Some(sdt.transport_stream_id);
            self.sdt_sections.insert(sdt.section_number);
            self.sdt_last_section_number = Some(sdt.last_section_number);
        }

        let transport_stream_id = sdt.transport_stream_id;
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

            self.streams
                .entry(transport_stream_id)
                .or_default()
                .insert(service.service_id, service);
        }
    }

    /// The services of one stream, which are none when nothing has described it.
    fn services_of(
        &self,
        transport_stream_id: Option<u16>,
    ) -> impl Iterator<Item = &ServiceInformation> {
        transport_stream_id
            .and_then(|transport_stream_id| self.streams.get(&transport_stream_id))
            .into_iter()
            .flat_map(BTreeMap::values)
    }

    fn channel_name(&self, transport_stream_id: Option<u16>) -> Option<String> {
        self.services_of(transport_stream_id)
            .find_map(service_name)
            .or_else(|| self.nit.as_ref().and_then(network_name))
    }

    fn services(&self, transport_stream_id: Option<u16>) -> Vec<StoredService> {
        self.services_of(transport_stream_id)
            .filter_map(|service| {
                let descriptor = service_descriptor(service)?;
                (descriptor.service_type == Some(TELEVISION_SERVICE_TYPE)).then_some(
                    StoredService {
                        id: service.service_id,
                        name: descriptor.service_name,
                        provider_name: descriptor.provider_name,
                    },
                )
            })
            .collect()
    }
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

#[cfg(test)]
mod tests {
    use chibitv_b10::descriptor::{
        SatelliteDeliverySystemDescriptor, ServiceListDescriptor, ServiceListItem,
    };
    use chibitv_b10::table::TransportStreamInformation;

    use super::super::BS_NETWORK_ID;
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

    fn sdt(transport_stream_id: u16, services: Vec<ServiceInformation>) -> Sdt {
        Sdt {
            section_syntax_indicator: true,
            section_length: 0,
            transport_stream_id,
            version_number: 0,
            current_next_indicator: true,
            section_number: 0,
            last_section_number: 0,
            original_network_id: BS_NETWORK_ID,
            services,
            crc_32: 0,
        }
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
            .streams
            .entry(0x4011)
            .or_default()
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
            .streams
            .entry(0x4011)
            .or_default()
            .insert(0x0400, television_service(0x0400, "BS"));

        assert!(!state.has_service_catalog());

        state.sdt_sections.insert(1);

        assert!(state.has_service_catalog());
    }

    #[test]
    fn takes_the_other_streams_of_the_network_from_their_own_descriptions() {
        let mut state = ScanState {
            reads_other_streams: true,
            ..ScanState::default()
        };
        let nit = nit(vec![TransportStreamInformation {
            transport_stream_id: 0x4031,
            original_network_id: BS_NETWORK_ID,
            descriptors: vec![
                satellite_descriptor(11_765_840),
                service_list(&[TELEVISION_SERVICE_TYPE]),
            ],
        }]);

        state.read_table("BS-1", 0x40, Table::Nit(nit));
        // Nothing has described the other stream yet.
        assert!(!state.has_every_stream());

        state.read_table(
            "BS-1",
            SDT_OTHER_TABLE_ID,
            Table::Sdt(sdt(0x4031, vec![television_service(0x0400, "NHK BS")])),
        );

        assert!(state.has_every_stream());
        // The names go through the encoding the SI spells them in, so what
        // matters here is that they reached the stream they belong to.
        assert!(state.channel_name(Some(0x4031)).is_some());
        let services = state.services(Some(0x4031));
        assert_eq!(services.len(), 1);
        assert_eq!(services[0].id, 0x0400);
        // The stream being tuned said nothing about itself, and a description
        // of another one does not stand in for it.
        assert!(state.transport_stream_id.is_none());
        assert!(!state.has_service_catalog());
    }

    #[test]
    fn leaves_the_other_streams_alone_unless_a_fast_scan_asked_for_them() {
        let mut state = ScanState::default();

        state.read_table(
            "UHF 20",
            SDT_OTHER_TABLE_ID,
            Table::Sdt(sdt(0x4031, vec![television_service(0x0400, "Elsewhere")])),
        );

        assert!(state.services(Some(0x4031)).is_empty());
    }
}
