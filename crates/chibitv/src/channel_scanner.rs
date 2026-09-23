//! Finding the channels on air.
//!
//! A scan tunes to what it is told to look at and reads the signalling there
//! into [`NewChannel`] entries, which are what the channels being served are
//! kept as: the `scan` command writes them itself, as the channels of the
//! broadcast it walked, while the server hands them to whoever asked for the
//! scan and waits for `BulkCreateChannels` to say which are worth keeping.

use std::collections::{BTreeMap, BTreeSet};
use std::io::{BufReader, Read};
use std::ops::RangeInclusive;
use std::sync::Arc;
use std::time::{Duration, Instant};

use clap::ValueEnum;
use tracing::{info, warn};

use chibitv_b10::descriptor::Descriptor;
use chibitv_b10::table::{Nit, Sdt, ServiceInformation, Table};
use chibitv_b24::decode as decode_b24;
use chibitv_b25::B25Descrambler;
use chibitv_b60::descriptor::Descriptor as MmtDescriptor;
use chibitv_b60::message::Message;
use chibitv_b60::table::{MhSdt, ServiceInformation as MmtServiceInformation, Table as MmtTable};
use chibitv_b60::tlv_si::{Descriptor as TlvDescriptor, Table as TlvTable, TlvNit};
use chibitv_b61::Descrambler;

use crate::cas::PcscCasModule;
use crate::channel::{Channel, ChannelInner, DeliverySystem};
use crate::config::Config;
use crate::demux::{Demux, Packet, SignalingEvent, is_descrambling_refused};
use crate::m2ts::M2tsDemuxer;
use crate::mmt::MmtDemuxer;
use crate::store::{NewChannel, StoredService};
use crate::task::TaskHandle;
use crate::tuner::{TunerLease, Tuners};

pub const FIRST_UHF_CHANNEL: u8 = 13;
pub const LAST_UHF_CHANNEL: u8 = 52;
const FIRST_UHF_FREQUENCY_HZ: u32 = 473_142_857;
const UHF_CHANNEL_BANDWIDTH_HZ: u32 = 6_000_000;

const BS_NETWORK_ID: u16 = 4;
const BS_4K_NETWORK_ID: u16 = 11;
const FIRST_BS_FREQUENCY_KHZ: u32 = 1_049_480;
const BS_FREQUENCY_STEP_KHZ: u32 = 38_360;
const LAST_BS_TRANSPONDER: u8 = 23;

const FIRST_CS110_FREQUENCY_KHZ: u32 = 1_613_000;
const CS110_FREQUENCY_STEP_KHZ: u32 = 40_000;
const LAST_CS110_TRANSPONDER: u8 = 24;

const TELEVISION_SERVICE_TYPE: u8 = 0x01;

const FAST_2K_TRANSPONDERS: [u8; 3] = [1, 2, 4];
const FAST_4K_TRANSPONDER: u8 = 7;

const SDT_ACTUAL_TABLE_ID: u8 = 0x42;
const SDT_OTHER_TABLE_ID: u8 = 0x46;

/// The broadcast a scan walks.
#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub enum ScanDeliverySystem {
    /// The terrestrial UHF physical channels.
    #[value(name = "ISDB-T")]
    IsdbT,

    /// The BS and CS110 satellite transponders.
    #[value(name = "ISDB-S")]
    IsdbS,

    /// The BS transponders carrying 4K.
    #[value(name = "ISDB-S3")]
    IsdbS3,
}

impl From<ScanDeliverySystem> for DeliverySystem {
    fn from(value: ScanDeliverySystem) -> Self {
        match value {
            ScanDeliverySystem::IsdbT => Self::IsdbT,
            ScanDeliverySystem::IsdbS => Self::IsdbS,
            ScanDeliverySystem::IsdbS3 => Self::IsdbS3,
        }
    }
}

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

/// What a scan is asked to look at.
#[derive(Clone, Debug)]
pub struct ScanRequest {
    pub delivery_system: ScanDeliverySystem,
    /// Whether to read a satellite network out of one transponder rather than
    /// tuning to every stream of it.
    pub fast: bool,
    /// The physical channels a terrestrial scan walks.
    pub uhf_channels: RangeInclusive<u8>,
    /// How long to wait on each channel.
    pub timeout: Duration,
}

impl ScanRequest {
    /// Refuses a request nothing could be made of, before a tuner or a card is
    /// reached for.
    pub fn validate(&self) -> anyhow::Result<()> {
        if self.fast && self.delivery_system == ScanDeliverySystem::IsdbT {
            anyhow::bail!(
                "A fast scan reads a network out of one transponder, which the terrestrial channels do not share"
            );
        }

        if self.delivery_system == ScanDeliverySystem::IsdbT
            && (*self.uhf_channels.start() < FIRST_UHF_CHANNEL
                || *self.uhf_channels.end() > LAST_UHF_CHANNEL
                || self.uhf_channels.is_empty())
        {
            anyhow::bail!(
                "UHF channel range must be within {FIRST_UHF_CHANNEL}..={LAST_UHF_CHANNEL}"
            );
        }

        Ok(())
    }
}

impl Default for ScanRequest {
    fn default() -> Self {
        Self {
            delivery_system: ScanDeliverySystem::IsdbT,
            fast: false,
            uhf_channels: FIRST_UHF_CHANNEL..=LAST_UHF_CHANNEL,
            timeout: Duration::from_secs(12),
        }
    }
}

/// Finds the channels on air, with a tuner and the cards that unscramble what
/// it reaches.
pub struct ChannelScanner {
    tuners: Arc<Tuners>,
    cas: Arc<PcscCasModule>,
    cas_master_key: [u8; 32],
}

impl ChannelScanner {
    pub fn new(tuners: Arc<Tuners>, cas: Arc<PcscCasModule>, cas_master_key: [u8; 32]) -> Self {
        Self {
            tuners,
            cas,
            cas_master_key,
        }
    }

    /// A scanner of its own, for a command that does not share the tuners with
    /// a running server.
    pub fn from_config(config: &Config) -> anyhow::Result<Self> {
        let mut tuners = Tuners::default();
        for (id, tuner) in config.tuners.iter().enumerate() {
            tuners.add_tuner_from_config(id as u32, tuner)?;
        }

        Ok(Self::new(
            Arc::new(tuners),
            PcscCasModule::open_shared()?,
            config.cas.master_key.into(),
        ))
    }

    /// Walks what the request asks for and returns what was found.
    ///
    /// One tuner is held for the whole scan, so that nothing takes it away
    /// halfway through. A task, when there is one, is told how far the walk has
    /// got and asked whether it should stop.
    pub fn scan(
        &self,
        request: &ScanRequest,
        task: Option<&TaskHandle>,
    ) -> anyhow::Result<Vec<NewChannel>> {
        request.validate()?;

        let tuner = self.tuners.try_acquire(request.delivery_system.into())?;
        info!(tuner_id = tuner.id(), "Acquired tuner for scanning");

        let scanner = Scanner {
            tuner,
            cas: self.cas.clone(),
            master_key: self.cas_master_key,
            timeout: request.timeout,
            task,
        };

        match (request.delivery_system, request.fast) {
            (ScanDeliverySystem::IsdbT, _) => scan_terrestrial(&scanner, request),
            (ScanDeliverySystem::IsdbS, false) => scan_satellite(&scanner),
            (ScanDeliverySystem::IsdbS, true) => scan_satellite_fast(&scanner),
            (ScanDeliverySystem::IsdbS3, false) => scan_satellite_4k(&scanner),
            (ScanDeliverySystem::IsdbS3, true) => scan_satellite_4k_fast(&scanner),
        }
    }
}

/// What every scan needs to hand a channel: a tuner to reach it with, the card
/// that unscrambles it, and how long to wait on it.
struct Scanner<'a> {
    tuner: TunerLease,
    cas: Arc<PcscCasModule>,
    /// The key the 4K descrambler needs, which the terrestrial and 2K ones do
    /// without.
    master_key: [u8; 32],
    timeout: Duration,
    /// The task the scan runs as, when a client is following it.
    task: Option<&'a TaskHandle>,
}

impl Scanner<'_> {
    /// Says how far the walk has got, and whether it should stop.
    fn report(&self, done: usize, total: usize, message: impl Into<String>) -> bool {
        if let Some(task) = self.task {
            task.report(Some(done as f32 / total as f32), message);

            return !task.is_cancelled();
        }

        true
    }
}

/// Walks the terrestrial UHF band, one physical channel at a time.
fn scan_terrestrial(scanner: &Scanner, request: &ScanRequest) -> anyhow::Result<Vec<NewChannel>> {
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
        if !scanner.report(index, scanned, format!("Scanning {label}")) {
            break;
        }

        let Some(state) =
            scanner.read_channel(&label, inner, ScanState::default(), ScanState::is_complete)?
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
fn scan_satellite(scanner: &Scanner) -> anyhow::Result<Vec<NewChannel>> {
    let streams = discover_satellite_streams(scanner)?;
    if streams.is_empty() {
        warn!("No satellite network answered: is the dish connected and its converter powered?");
    }

    let mut channels = Vec::new();
    for (index, stream) in streams.values().enumerate() {
        let transport_stream_id = stream.transport_stream_id;
        let frequency = stream.frequency_khz;
        if !scanner.report(
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
        let Some(state) = scanner.read_channel(
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
fn scan_satellite_fast(scanner: &Scanner) -> anyhow::Result<Vec<NewChannel>> {
    let mut channels = Vec::new();

    for transponder in
        transponders().filter(|transponder| FAST_2K_TRANSPONDERS.contains(&transponder.number))
    {
        let Some(state) = scanner.read_network(
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

/// The MMT/TLV counterpart of [`scan_satellite_fast`].
fn scan_satellite_4k_fast(scanner: &Scanner) -> anyhow::Result<Vec<NewChannel>> {
    let mut channels = Vec::new();

    for transponder in
        transponders_4k().filter(|transponder| transponder.number == FAST_4K_TRANSPONDER)
    {
        let Some(state) = scanner.read_tlv_network(&transponder, TlvScanState::has_every_stream)?
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

/// Finds every satellite transport stream worth tuning to, by its id.
fn discover_satellite_streams(scanner: &Scanner) -> anyhow::Result<BTreeMap<u16, SatelliteStream>> {
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
        let Some(state) = scanner.read_network(&transponder, ScanState::default(), |state| {
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

/// Walks the BS and CS110 transponders for the 4K broadcasting.
///
/// It works the way the 2K scan does, a transponder being only a way in and the
/// network naming the rest, but over MMT/TLV: the transmission control signal
/// of a TLV stream carries the TLV-NIT, which names every TLV stream of the
/// network, and the services of one are named by its own MH-SDT.
fn scan_satellite_4k(scanner: &Scanner) -> anyhow::Result<Vec<NewChannel>> {
    let streams = discover_tlv_streams(scanner)?;
    if streams.is_empty() {
        warn!("No 4K network answered: is the dish connected and its converter powered?");
    }

    let mut channels = Vec::new();
    for (index, stream) in streams.values().enumerate() {
        let tlv_stream_id = stream.tlv_stream_id;
        let frequency = stream.frequency_khz;
        if !scanner.report(
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
        let Some(state) = scanner.read_tlv_channel(
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
fn discover_tlv_streams(scanner: &Scanner) -> anyhow::Result<BTreeMap<u16, TlvStream>> {
    let mut discovered = BTreeMap::<u16, TlvStream>::new();

    for transponder in transponders_4k() {
        if discovered
            .values()
            .any(|stream| stream.frequency_khz == transponder.frequency_khz)
        {
            continue;
        }

        let Some(state) = scanner.read_tlv_network(&transponder, |state| state.nit.is_some())?
        else {
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
/// rules [`satellite_streams`] reads a NIT by.
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
    /// ARIB numbers a satellite stream after the transponder it sits on and its
    /// place on it, so the first two streams of a transponder can be named
    /// without having heard from the network yet.
    fn seed_stream_ids(&self) -> impl Iterator<Item = u32> {
        // A relative number reaches whichever stream the driver counts first,
        // which is a 2K one, so it is no way in to a 4K network.
        let relative = (self.network_id != BS_4K_NETWORK_ID).then_some(0);

        relative.into_iter().chain(
            [
                satellite_stream_id(self.network_id, self.number, 0),
                satellite_stream_id(self.network_id, self.number, 1),
            ]
            .map(u32::from),
        )
    }
}

/// The transport stream id ARIB gives the `index`th stream of a transponder.
fn satellite_stream_id(network_id: u16, transponder: u8, index: u8) -> u16 {
    (network_id << 12) | (u16::from(transponder) << 4) | u16::from(index)
}

/// The BS transponders, as the network numbered `network_id` names them.
fn bs_transponders(network_id: u16) -> impl Iterator<Item = Transponder> {
    (1..=LAST_BS_TRANSPONDER)
        .step_by(2)
        .map(move |number| Transponder {
            name: format!("BS-{number}"),
            network_id,
            number,
            frequency_khz: FIRST_BS_FREQUENCY_KHZ
                + u32::from(number - 1) / 2 * BS_FREQUENCY_STEP_KHZ,
        })
}

/// Every transponder of the satellites Japan broadcasts 2K from, BS first.
fn transponders() -> impl Iterator<Item = Transponder> {
    let bs = bs_transponders(BS_NETWORK_ID);

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

/// The transponders the 4K broadcasting is carried on.
///
/// BS numbers its 4K network apart from its 2K one, and a stream id is built
/// from the network it belongs to, so the same frequencies are walked under a
/// different number. CS110 is left out: which network its 4K is numbered as is
/// in ARIB TR-B39, and probing its transponders under the BS number reaches
/// nothing.
fn transponders_4k() -> impl Iterator<Item = Transponder> {
    bs_transponders(BS_4K_NETWORK_ID)
}

impl Scanner<'_> {
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

        let descrambler = B25Descrambler::init(self.cas.clone())?;
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

    /// The MMT/TLV counterpart of [`Scanner::read_network`].
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

    /// The MMT/TLV counterpart of [`Scanner::read_channel`].
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

    /// Tunes the tuner held for the scan to the channel, reporting one it
    /// cannot reach as nothing rather than as an error.
    fn tune(
        &self,
        label: &str,
        inner: ChannelInner,
    ) -> anyhow::Result<Option<Box<dyn Read + Send + Sync>>> {
        let channel = Channel {
            id: 0,
            name: label.to_string(),
            inner,
            stream_id: None,
        };

        if let Err(error) = self.tuner.tune(channel) {
            warn!(channel = label, error = %error, "Could not tune to the channel");
            return Ok(None);
        }

        Ok(Some(self.tuner.open_reader()?))
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
    /// The MMT counterpart of [`ScanState::has_service_catalog`], and all a 4K
    /// scan waits for once the network has been heard out.
    fn has_service_catalog(&self) -> bool {
        self.sdt_last_section_number
            .is_some_and(|last_section| self.sdt_sections.len() == usize::from(last_section) + 1)
            && !self.streams.is_empty()
    }

    /// The MMT counterpart of [`ScanState::has_every_stream`].
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

/// The MMT counterpart of [`network_name`], which the 4K signalling spells in
/// UTF-8 rather than in the encoding the SI tables use.
fn tlv_network_name(nit: &TlvNit) -> Option<String> {
    nit.descriptors.iter().find_map(|descriptor| {
        let TlvDescriptor::NetworkName(descriptor) = descriptor else {
            return None;
        };

        let name = String::from_utf8_lossy(&descriptor.network_name).to_string();
        (!name.is_empty()).then_some(name)
    })
}

/// The MMT counterpart of [`service_descriptor`].
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
    use chibitv_b60::descriptor::MhServiceDescriptor;
    use chibitv_b60::tlv_si::{
        NetworkNameDescriptor as TlvNetworkNameDescriptor,
        SatelliteDeliverySystemDescriptor as TlvSatelliteDeliverySystemDescriptor,
        ServiceListDescriptor as TlvServiceListDescriptor, ServiceListItem as TlvServiceListItem,
        TlvStreamInformation,
    };

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
    fn numbers_the_4k_network_apart_from_the_2k_one() {
        let transponders = transponders_4k().collect::<Vec<_>>();

        // The same twelve BS transponders, under the other network number.
        assert_eq!(transponders.len(), 12);
        assert!(
            transponders
                .iter()
                .all(|transponder| transponder.network_id == BS_4K_NETWORK_ID)
        );

        let bs7 = transponders
            .iter()
            .find(|transponder| transponder.name == "BS-7")
            .unwrap();
        // The network names 0xB070, which is this rule under number 11.
        assert_eq!(bs7.seed_stream_ids().collect::<Vec<_>>(), [0xB070, 0xB071]);
        assert_eq!(bs7.frequency_khz, 1_164_560);
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
    fn reaches_every_network_of_a_fast_scan_on_one_transponder_each() {
        let seeds = transponders()
            .filter(|transponder| FAST_2K_TRANSPONDERS.contains(&transponder.number))
            .collect::<Vec<_>>();

        // BS, and the two CS110 networks sharing the dish.
        let names = seeds
            .iter()
            .map(|transponder| transponder.name.as_str())
            .collect::<Vec<_>>();
        assert_eq!(names, ["BS-1", "ND2", "ND4"]);
        let networks = seeds
            .iter()
            .map(|transponder| transponder.network_id)
            .collect::<Vec<_>>();
        assert_eq!(networks, [BS_NETWORK_ID, 6, 7]);

        let seeds_4k = transponders_4k()
            .filter(|transponder| transponder.number == FAST_4K_TRANSPONDER)
            .collect::<Vec<_>>();
        assert_eq!(seeds_4k.len(), 1);
        assert_eq!(seeds_4k[0].name, "BS-7");
        assert_eq!(seeds_4k[0].network_id, BS_4K_NETWORK_ID);
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
