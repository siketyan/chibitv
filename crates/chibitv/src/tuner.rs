//! The tuners, as tunelithd, the daemon of Tunelith, shares them: it holds the
//! devices (PLEX and Digibest tuners over USB, the PT4K, and whatever has a
//! Linux DVB driver) and picks a free tuner receiving what is asked for, or
//! shares the one already receiving it with another program.
//!
//! tunelithd is asked for a stream each time a channel is tuned, and the
//! stream holds the tuner there until it is dropped. A new connection is made
//! for each, so that tunelithd can be restarted under a running server.

use std::io::{self, Read};
use std::path::PathBuf;

use anyhow::Context as _;
use tokio::io::AsyncReadExt;
use tokio::runtime::Handle;
use tracing::info;
use tunelith::{AcquireOptions, Client, DeviceStatus, Polarization, StreamId, System, TuneParams};

use crate::channel::{Channel, ChannelInner, DeliverySystem};
use crate::config::TunelithConfig;

/// The local oscillators of the dual circular converter on a Japanese dish,
/// which is what the channels keep their frequencies after.
const RIGHT_HANDED_OSCILLATOR_KHZ: u32 = 10_678_000;
const LEFT_HANDED_OSCILLATOR_KHZ: u32 = 9_505_000;

/// Where a left-handed stream lands once shifted down: above every right-handed
/// one, which ends at 2,071 MHz with CS, as BS left-handed begins at 2,224 MHz.
const LEFT_HANDED_MIN_IF_KHZ: u32 = 2_200_000;

#[derive(Debug)]
pub enum AcquireError {
    /// No tuner tunelithd holds receives the broadcast.
    Unsupported(DeliverySystem),
    /// Every tuner receiving the broadcast is in use.
    Busy,
    /// tunelithd could not be talked to: not running, restarted, and so on.
    Unreachable(anyhow::Error),
    /// The channel could not be tuned to, with no signal to lock on and so on.
    Failed(anyhow::Error),
}

impl std::fmt::Display for AcquireError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unsupported(system) => write!(f, "No tuner receives {system}"),
            Self::Busy => write!(f, "All tuners are in use"),
            Self::Unreachable(error) | Self::Failed(error) => write!(f, "{error:#}"),
        }
    }
}

impl std::error::Error for AcquireError {}

/// The way to tunelithd.
pub struct Tuners {
    socket: PathBuf,
    lnb: bool,
    /// The runtime the calls to tunelithd are run on, as the tuners are used
    /// from threads of their own as well.
    handle: Handle,
}

impl Tuners {
    pub fn new(config: &TunelithConfig) -> anyhow::Result<Self> {
        Ok(Self {
            socket: config
                .socket
                .clone()
                .unwrap_or_else(tunelith::default_socket),
            lnb: config.lnb,
            handle: Handle::try_current().context("The tuners need a Tokio runtime")?,
        })
    }

    /// Tunes a tuner to the channel, which is held there until the input is
    /// dropped.
    pub fn tune(&self, channel: &Channel) -> Result<TunerInput, AcquireError> {
        let params = tune_params(&channel.inner).map_err(AcquireError::Failed)?;
        let options = AcquireOptions {
            tuner: None,
            lnb: self.lnb,
        };

        info!("Tuning to {}: {params:?}", channel.name);
        let unreachable = |error| {
            AcquireError::Unreachable(anyhow::Error::new(error).context(format!(
                "Could not reach tunelithd at {}",
                self.socket.display(),
            )))
        };
        let (stream, signal) = block_on(&self.handle, async {
            let client = Client::connect(&self.socket).await.map_err(unreachable)?;
            let stream = match client.acquire(params, options).await {
                Ok(stream) => stream,
                Err(error) => {
                    // tunelithd tells why only in words, so what it holds is
                    // looked at to tell a busy tuner from a missing one.
                    let devices = client.list().await.map_err(unreachable)?;
                    return Err(
                        refusal(channel.inner.delivery_system(), &devices).unwrap_or_else(|| {
                            AcquireError::Failed(
                                anyhow::Error::new(error).context("Could not tune"),
                            )
                        }),
                    );
                }
            };
            let signal = stream.signal().await.map_err(unreachable)?;
            Ok((stream, signal))
        })?;

        match signal.cnr_db {
            Some(cnr) => info!("Tuned {}, C/N: {cnr:.2} dB", stream.tuner()),
            None => info!("Tuned {}, C/N not available", stream.tuner()),
        }

        Ok(TunerInput {
            stream,
            handle: self.handle.clone(),
        })
    }
}

/// Why no tuner could be had for the broadcast, if it was for want of one.
fn refusal(system: DeliverySystem, devices: &[DeviceStatus]) -> Option<AcquireError> {
    let wanted = to_system(system);
    let mut receiving = devices
        .iter()
        .flat_map(|device| &device.tuners)
        .filter(|tuner| tuner.info.systems.contains(&wanted))
        .peekable();

    if receiving.peek().is_none() {
        Some(AcquireError::Unsupported(system))
    } else if receiving.all(|tuner| tuner.busy) {
        Some(AcquireError::Busy)
    } else {
        None
    }
}

fn to_system(system: DeliverySystem) -> System {
    match system {
        DeliverySystem::IsdbT => System::IsdbT,
        DeliverySystem::IsdbS => System::IsdbS,
        DeliverySystem::IsdbS3 => System::IsdbS3,
    }
}

/// What tunelithd is asked to tune to for the channel.
fn tune_params(channel: &ChannelInner) -> anyhow::Result<TuneParams> {
    let params = match *channel {
        ChannelInner::IsdbT { frequency, .. } => TuneParams {
            system: System::IsdbT,
            frequency_khz: (frequency + 500) / 1_000,
            stream_id: None,
            polarization: None,
        },
        ChannelInner::IsdbS {
            frequency,
            stream_id,
        } => satellite(System::IsdbS, frequency, stream_id)?,
        ChannelInner::IsdbS3 {
            frequency,
            stream_id,
        } => satellite(System::IsdbS3, frequency, stream_id)?,
    };

    Ok(params)
}

/// A channel keeps the frequency the dish hands the tuner, while tunelithd
/// takes the one on air and shifts it down itself.
fn satellite(system: System, if_khz: u32, stream_id: u32) -> anyhow::Result<TuneParams> {
    let (polarization, oscillator) = if if_khz >= LEFT_HANDED_MIN_IF_KHZ {
        (Polarization::Left, LEFT_HANDED_OSCILLATOR_KHZ)
    } else {
        (Polarization::Right, RIGHT_HANDED_OSCILLATOR_KHZ)
    };
    let stream_id = u16::try_from(stream_id)
        .with_context(|| format!("The stream id {stream_id:#x} does not fit in 16 bits"))?;

    Ok(TuneParams {
        system,
        frequency_khz: if_khz + oscillator,
        stream_id: Some(StreamId(stream_id)),
        polarization: Some(polarization),
    })
}

/// Runs a call to tunelithd to its end, from a thread of the runtime or not.
fn block_on<F: Future>(handle: &Handle, future: F) -> F::Output {
    tokio::task::block_in_place(|| handle.block_on(future))
}

/// What a tuner receives, which gives the tuner back to tunelithd once
/// dropped.
pub struct TunerInput {
    stream: tunelith::Stream,
    handle: Handle,
}

impl TunerInput {
    /// The tuner the input comes from, as `tunelith list` shows it.
    pub fn tuner(&self) -> &str {
        self.stream.tuner()
    }
}

impl Read for TunerInput {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        block_on(&self.handle, self.stream.read(buf))
    }
}

#[cfg(test)]
mod tests {
    use tunelith::{DeviceInfo, TunerInfo, TunerStatus};

    use super::*;

    #[test]
    fn tunes_to_the_frequency_on_air() {
        let terrestrial = tune_params(&ChannelInner::IsdbT {
            frequency: 515_142_857,
            bandwidth_hz: 6_000_000,
        })
        .unwrap();
        assert_eq!(terrestrial.frequency_khz, 515_143);
        assert_eq!(terrestrial.stream_id, None);

        // BS-1, right-handed.
        let bs = tune_params(&ChannelInner::IsdbS {
            frequency: 1_049_480,
            stream_id: 0x4010,
        })
        .unwrap();
        assert_eq!(bs.frequency_khz, 11_727_480);
        assert_eq!(bs.polarization, Some(Polarization::Right));
        assert_eq!(bs.stream_id, Some(StreamId(0x4010)));

        // ND24, the highest right-handed CS transponder.
        let cs = tune_params(&ChannelInner::IsdbS {
            frequency: 2_071_000,
            stream_id: 0x7010,
        })
        .unwrap();
        assert_eq!(cs.polarization, Some(Polarization::Right));

        // BS-8, left-handed.
        let left = tune_params(&ChannelInner::IsdbS3 {
            frequency: 2_302_880,
            stream_id: 0xB180,
        })
        .unwrap();
        assert_eq!(left.frequency_khz, 11_807_880);
        assert_eq!(left.polarization, Some(Polarization::Left));
        assert!(left.validate().is_ok());
    }

    #[test]
    fn tells_a_busy_tuner_from_a_missing_one() {
        let tuner = |systems: &[System], busy| TunerStatus {
            info: TunerInfo {
                id: "0000000001#0".to_string(),
                systems: systems.to_vec(),
            },
            busy,
        };
        let devices = |tuners| {
            vec![DeviceStatus {
                info: DeviceInfo {
                    id: "0000000001".to_string(),
                    name: "PX-MLT5PE".to_string(),
                },
                tuners,
            }]
        };

        assert!(matches!(
            refusal(DeliverySystem::IsdbT, &[]),
            Some(AcquireError::Unsupported(DeliverySystem::IsdbT)),
        ));
        assert!(matches!(
            refusal(
                DeliverySystem::IsdbS3,
                &devices(vec![tuner(&[System::IsdbT, System::IsdbS], false)]),
            ),
            Some(AcquireError::Unsupported(DeliverySystem::IsdbS3)),
        ));
        assert!(matches!(
            refusal(
                DeliverySystem::IsdbT,
                &devices(vec![
                    tuner(&[System::IsdbT], true),
                    tuner(&[System::IsdbS], false)
                ]),
            ),
            Some(AcquireError::Busy),
        ));
        // A free tuner that could not tune is not for want of one.
        assert!(
            refusal(
                DeliverySystem::IsdbT,
                &devices(vec![
                    tuner(&[System::IsdbT], true),
                    tuner(&[System::IsdbT], false)
                ]),
            )
            .is_none()
        );
    }
}
