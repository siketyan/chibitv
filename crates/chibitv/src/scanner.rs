//! Finding the channels on air.
//!
//! A scan tunes to what it is told to look at and reads the signalling there
//! into [`NewChannel`] entries, which are what the channels being served are
//! kept as: the `scan` command writes them itself, as the channels of the
//! broadcast it walked, while the server hands them to whoever asked for the
//! scan and waits for `BulkCreateChannels` to say which are worth keeping.

mod m2ts;
mod mmt;

use std::io::Read;
use std::ops::RangeInclusive;
use std::sync::Arc;
use std::time::Duration;

use clap::ValueEnum;
use tracing::{info, warn};

use crate::cas::SharedCasModule;
use crate::channel::{Channel, ChannelInner, DeliverySystem};
use crate::config::Config;
use crate::store::NewChannel;
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
pub struct Scanner {
    tuners: Arc<Tuners>,
    cas: Arc<SharedCasModule>,
    cas_master_key: [u8; 32],
}

impl Scanner {
    pub fn new(tuners: Arc<Tuners>, cas: Arc<SharedCasModule>, cas_master_key: [u8; 32]) -> Self {
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
            SharedCasModule::open()?,
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

        let scan = Scan {
            tuner,
            cas: self.cas.clone(),
            master_key: self.cas_master_key,
            timeout: request.timeout,
            task,
        };

        match (request.delivery_system, request.fast) {
            (ScanDeliverySystem::IsdbT, _) => m2ts::scan_terrestrial(&scan, request),
            (ScanDeliverySystem::IsdbS, false) => m2ts::scan_satellite(&scan),
            (ScanDeliverySystem::IsdbS, true) => m2ts::scan_satellite_fast(&scan),
            (ScanDeliverySystem::IsdbS3, false) => mmt::scan_satellite_4k(&scan),
            (ScanDeliverySystem::IsdbS3, true) => mmt::scan_satellite_4k_fast(&scan),
        }
    }
}

/// What every scan needs to hand a channel: a tuner to reach it with, the card
/// that unscrambles it, and how long to wait on it.
struct Scan<'a> {
    tuner: TunerLease,
    cas: Arc<SharedCasModule>,
    /// The key the 4K descrambler needs, which the terrestrial and 2K ones do
    /// without.
    master_key: [u8; 32],
    timeout: Duration,
    /// The task the scan runs as, when a client is following it.
    task: Option<&'a TaskHandle>,
}

impl Scan<'_> {
    /// Says how far the walk has got, and whether it should stop.
    fn report(&self, done: usize, total: usize, message: impl Into<String>) -> bool {
        if let Some(task) = self.task {
            task.report(Some(done as f32 / total as f32), message);

            return !task.is_cancelled();
        }

        true
    }
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

impl Scan<'_> {
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

#[derive(Clone, Debug, Default)]
struct ServiceDescriptor {
    service_type: Option<u8>,
    service_name: String,
    provider_name: String,
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
