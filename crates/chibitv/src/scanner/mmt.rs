//! Scanning the 4K satellite broadcasts, which are carried on MMT/TLV.

use std::collections::{BTreeMap, BTreeSet};
use std::io::BufReader;
use std::time::Instant;

use tracing::{info, warn};

use chibitv_b60::descriptor::Descriptor as MmtDescriptor;
use chibitv_b60::message::Message;
use chibitv_b60::table::{MhSdt, ServiceInformation as MmtServiceInformation, Table as MmtTable};
use chibitv_b60::tlv_si::{Descriptor as TlvDescriptor, Table as TlvTable, TlvNit};
use chibitv_b61::Descrambler;

use super::{
    FAST_4K_TRANSPONDER, Scan, ServiceDescriptor, TELEVISION_SERVICE_TYPE, Transponder,
    transponders_4k,
};
use crate::channel::ChannelInner;
use crate::demux::{Demux, Packet, SignalingEvent, is_descrambling_refused};
use crate::mmt::MmtDemuxer;
use crate::store::{NewChannel, StoredService};
use crate::task::TaskHandle;

/// The MMT/TLV counterpart of `m2ts::scan_satellite_fast`.
pub(super) fn scan_satellite_4k_fast(scan: &Scan) -> anyhow::Result<Vec<NewChannel>> {
    let mut channels = Vec::new();

    for transponder in
        transponders_4k().filter(|transponder| transponder.number == FAST_4K_TRANSPONDER)
    {
        let Some(state) = scan.read_tlv_network(&transponder, TlvScanState::has_every_stream)?
        else {
            continue;
        };
        let Some(nit) = &state.nit else {
            warn!(transponder = transponder.name, "The network said nothing");
            continue;
        };

        for stream in tlv_streams(nit) {
            let tlv_stream_id = stream.tlv_stream_id;
            let Some(name) = state.channel_name(Some(tlv_stream_id)) else {
                continue;
            };

            channels.push(NewChannel {
                name,
                transport_stream_id: Some(tlv_stream_id),
                services: state.services(Some(tlv_stream_id)),
                inner: ChannelInner::IsdbS3 {
                    frequency: stream.frequency_khz,
                    stream_id: u32::from(tlv_stream_id),
                },
            });
        }
    }

    Ok(channels)
}

/// Walks the BS and CS110 transponders for the 4K broadcasting.
///
/// It works the way the 2K scan does, a transponder being only a way in and the
/// network naming the rest, but over MMT/TLV: the transmission control signal
/// of a TLV stream carries the TLV-NIT, which names every TLV stream of the
/// network, and the services of one are named by its own MH-SDT.
pub(super) fn scan_satellite_4k(scan: &Scan) -> anyhow::Result<Vec<NewChannel>> {
    let streams = discover_tlv_streams(scan)?;
    if streams.is_empty() {
        warn!("No 4K network answered: is the dish connected and its converter powered?");
    }

    let mut channels = Vec::new();
    for (index, stream) in streams.values().enumerate() {
        let tlv_stream_id = stream.tlv_stream_id;
        let frequency = stream.frequency_khz;
        if !scan.report(
            index,
            streams.len(),
            format!("Scanning TLV stream {tlv_stream_id:#06X}"),
        ) {
            break;
        }

        let inner = ChannelInner::IsdbS3 {
            frequency,
            stream_id: u32::from(tlv_stream_id),
        };

        info!(tlv_stream_id, frequency, "Scanning TLV stream");

        let label = format!("TLV stream {tlv_stream_id:#06X}");
        let Some(state) = scan.read_tlv_channel(
            &label,
            inner,
            Some(tlv_stream_id),
            TlvScanState::has_service_catalog,
        )?
        else {
            continue;
        };
        let Some(name) = state.channel_name(Some(tlv_stream_id)) else {
            continue;
        };

        channels.push(NewChannel {
            name,
            transport_stream_id: Some(tlv_stream_id),
            services: state.services(Some(tlv_stream_id)),
            inner: ChannelInner::IsdbS3 {
                frequency,
                stream_id: u32::from(tlv_stream_id),
            },
        });
    }

    Ok(channels)
}

/// Finds every TLV stream worth tuning to, by its id.
fn discover_tlv_streams(scan: &Scan) -> anyhow::Result<BTreeMap<u16, TlvStream>> {
    let mut discovered = BTreeMap::<u16, TlvStream>::new();

    for transponder in transponders_4k() {
        if discovered
            .values()
            .any(|stream| stream.frequency_khz == transponder.frequency_khz)
        {
            continue;
        }

        let Some(state) = scan.read_tlv_network(&transponder, |state| state.nit.is_some())? else {
            continue;
        };
        let Some(nit) = state.nit else {
            continue;
        };

        let found = tlv_streams(&nit);
        info!(
            transponder = transponder.name,
            original_network_id = nit.original_network_id,
            streams = found.len(),
            "4K network found"
        );
        discovered.extend(
            found
                .into_iter()
                .map(|stream| (stream.tlv_stream_id, stream)),
        );
    }

    Ok(discovered)
}

/// One TLV stream of a network, as its TLV-NIT describes it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct TlvStream {
    tlv_stream_id: u16,
    /// The frequency the dish hands the tuner, in kHz.
    frequency_khz: u32,
}

/// The TLV streams a TLV-NIT describes that are worth tuning to, under the same
/// rules `m2ts::satellite_streams` reads a NIT by.
fn tlv_streams(nit: &TlvNit) -> Vec<TlvStream> {
    nit.tlv_streams
        .iter()
        .filter(|stream| tlv_stream_carries_television(&stream.descriptors))
        .filter_map(|stream| {
            let frequency_khz = stream.descriptors.iter().find_map(|descriptor| {
                let TlvDescriptor::SatelliteDeliverySystem(descriptor) = descriptor else {
                    return None;
                };

                descriptor.intermediate_frequency_khz()
            })?;

            Some(TlvStream {
                tlv_stream_id: stream.tlv_stream_id,
                frequency_khz,
            })
        })
        .collect()
}

fn tlv_stream_carries_television(descriptors: &[TlvDescriptor]) -> bool {
    let mut listed = descriptors
        .iter()
        .filter_map(|descriptor| match descriptor {
            TlvDescriptor::ServiceList(descriptor) => Some(&descriptor.services),
            _ => None,
        })
        .flatten()
        .peekable();

    listed.peek().is_none() || listed.any(|service| service.service_type == TELEVISION_SERVICE_TYPE)
}

impl Scan<'_> {
    /// The MMT/TLV counterpart of [`Scan::read_network`].
    fn read_tlv_network(
        &self,
        transponder: &Transponder,
        is_done: impl Fn(&TlvScanState) -> bool + Copy,
    ) -> anyhow::Result<Option<TlvScanState>> {
        if let Some(task) = self.task {
            task.report(None, format!("Probing {}", transponder.name));
        }

        for stream_id in transponder.seed_stream_ids() {
            info!(
                transponder = transponder.name,
                frequency = transponder.frequency_khz,
                stream_id,
                "Probing satellite transponder for 4K"
            );

            let inner = ChannelInner::IsdbS3 {
                frequency: transponder.frequency_khz,
                stream_id,
            };
            let Some(read) = self.read_tlv_channel(&transponder.name, inner, None, is_done)? else {
                break;
            };

            if read.nit.is_some() {
                return Ok(Some(read));
            }
        }

        Ok(None)
    }

    /// The MMT/TLV counterpart of [`Scan::read_channel`].
    ///
    /// `watched_stream` is the TLV stream the services are collected of, which
    /// a probe that only wants the network leaves unset.
    fn read_tlv_channel(
        &self,
        label: &str,
        inner: ChannelInner,
        watched_stream: Option<u16>,
        is_done: impl Fn(&TlvScanState) -> bool,
    ) -> anyhow::Result<Option<TlvScanState>> {
        let Some(input) = self.tune(label, inner)? else {
            return Ok(None);
        };

        // Nothing the scan reads is scrambled, but the descrambler is what the
        // demultiplexer is built around, so it is set up all the same.
        let descrambler = Descrambler::init(self.cas.clone(), self.master_key, false)?;
        let mut demux = MmtDemuxer::new(BufReader::new(input), descrambler);
        let mut state = TlvScanState {
            watched_stream,
            ..TlvScanState::default()
        };
        let mut refused = false;
        let deadline = Instant::now() + self.timeout;

        while Instant::now() < deadline && !is_done(&state) {
            if self.task.is_some_and(TaskHandle::is_cancelled) {
                break;
            }

            let packet = match demux.next_packet() {
                Ok(Some(packet)) => packet,
                Ok(None) => break,
                // The signalling is not scrambled, so the scan reads past a
                // card that will not unscramble the rest of the stream.
                Err(error) if is_descrambling_refused(&error) => {
                    if !std::mem::replace(&mut refused, true) {
                        warn!(channel = label, %error, "Scanning the signalling only");
                    }

                    continue;
                }
                Err(error) => {
                    warn!(channel = label, error = %error, "Could not read TLV stream");
                    continue;
                }
            };

            match packet {
                Packet::Signaling(SignalingEvent::TlvTable(table)) => {
                    state.read_tlv_table(label, table);
                }
                Packet::Signaling(SignalingEvent::B60Message(Message::M2Section(message))) => {
                    state.read_m2_table(label, message.table);
                }
                _ => {}
            }
        }

        Ok(Some(state))
    }
}

/// What a 4K scan collects from one TLV stream.
#[derive(Clone, Debug, Default)]
struct TlvScanState {
    /// The TLV stream whose services are being collected, unset to collect
    /// every stream the network describes.
    watched_stream: Option<u16>,
    nit: Option<TlvNit>,
    /// The services of each stream, by the stream carrying them.
    streams: BTreeMap<u16, BTreeMap<u16, MmtServiceInformation>>,
    sdt_sections: BTreeSet<u8>,
    sdt_last_section_number: Option<u8>,
    logged_networks: BTreeSet<u16>,
    logged_services: BTreeSet<u16>,
}

impl TlvScanState {
    /// The MMT counterpart of `m2ts::ScanState::has_service_catalog`, and all
    /// a 4K scan waits for once the network has been heard out.
    fn has_service_catalog(&self) -> bool {
        self.sdt_last_section_number
            .is_some_and(|last_section| self.sdt_sections.len() == usize::from(last_section) + 1)
            && !self.streams.is_empty()
    }

    /// The MMT counterpart of `m2ts::ScanState::has_every_stream`.
    fn has_every_stream(&self) -> bool {
        let Some(nit) = &self.nit else {
            return false;
        };

        tlv_streams(nit)
            .iter()
            .all(|stream| self.streams.contains_key(&stream.tlv_stream_id))
    }

    fn read_tlv_table(&mut self, channel: &str, table: TlvTable) {
        let TlvTable::TlvNit(nit) = table else {
            return;
        };

        if self.logged_networks.insert(nit.original_network_id) {
            info!(
                channel,
                original_network_id = nit.original_network_id,
                network_name = tlv_network_name(&nit).unwrap_or_default(),
                "Network found"
            );
        }

        if self.nit.is_none() {
            self.nit = Some(nit);
        }
    }

    fn read_m2_table(&mut self, channel: &str, table: MmtTable) {
        let MmtTable::MhSdt(sdt) = table else {
            return;
        };

        // A stream describes the others as well as itself, and only its own
        // services belong to the channel being scanned.
        if self
            .watched_stream
            .is_some_and(|watched| watched != sdt.tlv_stream_id)
        {
            return;
        }

        self.read_mh_sdt(channel, sdt);
    }

    fn read_mh_sdt(&mut self, channel: &str, sdt: MhSdt) {
        self.sdt_sections.insert(sdt.section_number);
        self.sdt_last_section_number = Some(sdt.last_section_number);

        for service in sdt.services {
            if self.logged_services.insert(service.service_id) {
                let descriptor = mmt_service_descriptor(&service).unwrap_or_default();

                info!(
                    channel,
                    tlv_stream_id = sdt.tlv_stream_id,
                    service_id = service.service_id,
                    service_type = descriptor.service_type.unwrap_or_default(),
                    service_name = descriptor.service_name,
                    "Service found"
                );
            }

            self.streams
                .entry(sdt.tlv_stream_id)
                .or_default()
                .insert(service.service_id, service);
        }
    }

    /// The services of one stream, which are none when nothing has described it.
    fn services_of(
        &self,
        tlv_stream_id: Option<u16>,
    ) -> impl Iterator<Item = &MmtServiceInformation> {
        tlv_stream_id
            .and_then(|tlv_stream_id| self.streams.get(&tlv_stream_id))
            .into_iter()
            .flat_map(BTreeMap::values)
    }

    fn channel_name(&self, tlv_stream_id: Option<u16>) -> Option<String> {
        self.services_of(tlv_stream_id)
            .filter_map(mmt_service_descriptor)
            .find_map(|descriptor| {
                (!descriptor.service_name.is_empty()).then_some(descriptor.service_name)
            })
            .or_else(|| self.nit.as_ref().and_then(tlv_network_name))
    }

    fn services(&self, tlv_stream_id: Option<u16>) -> Vec<StoredService> {
        self.services_of(tlv_stream_id)
            .filter_map(|service| {
                let descriptor = mmt_service_descriptor(service)?;
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

/// The MMT counterpart of `m2ts::network_name`, which the 4K signalling spells
/// in UTF-8 rather than in the encoding the SI tables use.
fn tlv_network_name(nit: &TlvNit) -> Option<String> {
    nit.descriptors.iter().find_map(|descriptor| {
        let TlvDescriptor::NetworkName(descriptor) = descriptor else {
            return None;
        };

        let name = String::from_utf8_lossy(&descriptor.network_name).to_string();
        (!name.is_empty()).then_some(name)
    })
}

/// The MMT counterpart of `m2ts::service_descriptor`.
///
/// The 4K signalling spells its names in UTF-8 rather than in the encoding the
/// SI tables use.
fn mmt_service_descriptor(service: &MmtServiceInformation) -> Option<ServiceDescriptor> {
    service.descriptors.iter().find_map(|descriptor| {
        let MmtDescriptor::MhService(descriptor) = descriptor else {
            return None;
        };

        Some(ServiceDescriptor {
            service_type: Some(descriptor.service_type),
            service_name: String::from_utf8_lossy(&descriptor.service_name).to_string(),
            provider_name: String::from_utf8_lossy(&descriptor.service_provider_name).to_string(),
        })
    })
}

#[cfg(test)]
mod tests {
    use chibitv_b60::descriptor::MhServiceDescriptor;
    use chibitv_b60::tlv_si::{
        NetworkNameDescriptor as TlvNetworkNameDescriptor,
        SatelliteDeliverySystemDescriptor as TlvSatelliteDeliverySystemDescriptor,
        ServiceListDescriptor as TlvServiceListDescriptor, ServiceListItem as TlvServiceListItem,
        TlvStreamInformation,
    };

    use super::super::{BS_4K_NETWORK_ID, BS_NETWORK_ID};
    use super::*;

    fn tlv_satellite_descriptor(frequency_khz: u32) -> TlvDescriptor {
        TlvDescriptor::SatelliteDeliverySystem(TlvSatelliteDeliverySystemDescriptor {
            frequency_khz,
            orbital_position: 1100,
            west_east_flag: true,
            polarisation: 0x01,
            modulation: 0x0B,
            symbol_rate: 288_600,
            fec_inner: 0x0F,
        })
    }

    fn tlv_service_list(service_types: &[u8]) -> TlvDescriptor {
        TlvDescriptor::ServiceList(TlvServiceListDescriptor {
            services: service_types
                .iter()
                .enumerate()
                .map(|(index, service_type)| TlvServiceListItem {
                    service_id: 0x4065 + index as u16,
                    service_type: *service_type,
                })
                .collect(),
        })
    }

    fn tlv_nit(tlv_streams: Vec<TlvStreamInformation>) -> TlvNit {
        TlvNit {
            section_syntax_indicator: true,
            section_length: 0,
            original_network_id: BS_4K_NETWORK_ID,
            version_number: 0,
            current_next_indicator: true,
            section_number: 0,
            last_section_number: 0,
            descriptors: vec![TlvDescriptor::NetworkName(TlvNetworkNameDescriptor {
                network_name: b"BS4".to_vec(),
            })],
            tlv_streams,
            crc_32: 0,
        }
    }

    fn mh_service(service_id: u16, name: &str, service_type: u8) -> MmtServiceInformation {
        MmtServiceInformation {
            service_id,
            eit_user_defined_flags: 0,
            eit_schedule_flag: true,
            eit_present_following_flag: true,
            running_status: 4,
            free_ca_mode: true,
            descriptors: vec![MmtDescriptor::MhService(MhServiceDescriptor {
                service_type,
                service_provider_name: "NHK".as_bytes().to_vec(),
                service_name: name.as_bytes().to_vec(),
            })],
        }
    }

    fn mh_sdt(tlv_stream_id: u16, services: Vec<MmtServiceInformation>) -> MmtTable {
        MmtTable::MhSdt(MhSdt {
            section_syntax_indicator: true,
            section_length: 0,
            tlv_stream_id,
            version_number: 0,
            current_next_indicator: true,
            section_number: 0,
            last_section_number: 0,
            original_network_id: BS_NETWORK_ID,
            services,
            crc_32: 0,
        })
    }

    #[test]
    fn reads_the_tlv_streams_of_a_network_from_its_nit() {
        let streams = tlv_streams(&tlv_nit(vec![
            TlvStreamInformation {
                tlv_stream_id: 0xB070,
                original_network_id: BS_4K_NETWORK_ID,
                descriptors: vec![
                    tlv_satellite_descriptor(11_996_000),
                    tlv_service_list(&[TELEVISION_SERVICE_TYPE]),
                ],
            },
            // Data only, so there is nothing to watch on it.
            TlvStreamInformation {
                tlv_stream_id: 0xB071,
                original_network_id: BS_4K_NETWORK_ID,
                descriptors: vec![
                    tlv_satellite_descriptor(11_996_000),
                    tlv_service_list(&[0xC0]),
                ],
            },
            // Nothing says which transponder it is on, so it cannot be tuned.
            TlvStreamInformation {
                tlv_stream_id: 0xB0F0,
                original_network_id: BS_4K_NETWORK_ID,
                descriptors: vec![tlv_service_list(&[TELEVISION_SERVICE_TYPE])],
            },
        ]));

        assert_eq!(
            streams,
            [TlvStream {
                tlv_stream_id: 0xB070,
                frequency_khz: 1_318_000,
            }]
        );
    }

    #[test]
    fn collects_the_services_of_the_stream_being_scanned() {
        let mut state = TlvScanState {
            watched_stream: Some(0xB070),
            ..TlvScanState::default()
        };

        // A stream describes its neighbours as well as itself.
        state.read_m2_table(
            "BS-7",
            mh_sdt(0xB071, vec![mh_service(0x4066, "Elsewhere", 0x01)]),
        );
        assert!(state.services(Some(0xB070)).is_empty());
        assert!(!state.has_service_catalog());

        state.read_m2_table(
            "BS-7",
            mh_sdt(
                0xB070,
                vec![
                    mh_service(0x4065, "NHK BS4K", 0x01),
                    // Not television, so it is not a service to tune to.
                    mh_service(0x4067, "Data", 0xC0),
                ],
            ),
        );
        let services = state.services(Some(0xB070));
        assert_eq!(services.len(), 1);
        assert_eq!(services[0].id, 0x4065);
        assert_eq!(services[0].name, "NHK BS4K");
        assert_eq!(services[0].provider_name, "NHK");
        assert_eq!(
            state.channel_name(Some(0xB070)).as_deref(),
            Some("NHK BS4K")
        );
        // The network was heard out while the transponder was probed, so there
        // is nothing left to wait for.
        assert!(state.has_service_catalog());
    }

    #[test]
    fn falls_back_to_the_network_name_of_a_stream_without_a_named_service() {
        let mut state = TlvScanState::default();

        state.read_tlv_table("BS-15", TlvTable::TlvNit(tlv_nit(vec![])));

        assert_eq!(state.channel_name(None).as_deref(), Some("BS4"));
    }
}
