//! A tuner driven through px4_drv, the driver of the PLEX and Digibest tuners
//! (PX-W3U4, PX-MLT5PE, DTV02A-1T1S-U and the like), on either platform it
//! exists for: a character device on Linux (`linux.rs`) and the named pipes
//! of `DriverHost_PX4`, its user-mode driver over WinUSB, on Windows
//! (`windows.rs`). Each platform supplies a `Device` with the same methods,
//! and the tuning and the reading are done here over it.
//!
//! The device is opened for as long as it is leased and given back after, as
//! opening it is what powers the tuner up, and the driver lets only one
//! program have it at a time.
//!
//! The driver buffers well under a second of a satellite stream: a reader
//! held up by a card exchange or a database write would lose packets. So the
//! device is read on its own thread, and what it reads is kept for the
//! consumer in memory.

#[cfg(target_os = "linux")]
mod linux;
#[cfg(windows)]
mod windows;

#[cfg(target_os = "linux")]
use linux::Device;
#[cfg(target_os = "linux")]
pub use linux::Target;
#[cfg(windows)]
use windows::Device;
#[cfg(windows)]
pub use windows::Target;

use std::fmt::{self, Display, Formatter};
use std::io;
use std::io::Read;
use std::sync::mpsc::{Receiver, TrySendError, sync_channel};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread;

use anyhow::{Context, bail};
use tracing::{error, info, warn};

use crate::channel::{Channel, ChannelInner, DeliverySystem};
use crate::tuner::Tuner;

/// How much the reader thread takes off the device at once.
const CHUNK_SIZE: usize = 188 * 1024;
/// How many chunks are kept for a consumer that falls behind, before the
/// stream is cut to catch up: some six seconds of a satellite stream.
const BUFFERED_CHUNKS: usize = 128;

/// The broadcast a tuning is for, numbered as both drivers number it
/// (`enum ptx_system_type` and `px4::SystemType`).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum System {
    IsdbT = 0x10,
    IsdbS = 0x20,
}

impl Display for System {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Self::IsdbT => DeliverySystem::IsdbT.fmt(f),
            Self::IsdbS => DeliverySystem::IsdbS.fmt(f),
        }
    }
}

/// What a channel is tuned with.
#[derive(Clone, Copy, Debug)]
enum Tuning {
    Terrestrial {
        frequency_hz: u32,
    },
    /// A transponder and the stream to pick on it, which is either the
    /// transport stream id or the stream's number on the transponder.
    Satellite {
        frequency_khz: u32,
        stream_id: u32,
    },
}

impl Tuning {
    fn system(self) -> System {
        match self {
            Self::Terrestrial { .. } => System::IsdbT,
            Self::Satellite { .. } => System::IsdbS,
        }
    }
}

pub struct Px4Tuner {
    target: Target,
    lnb_voltage: u8,
    /// The broadcasts the configuration says the tuner receives.
    delivery_systems: Vec<DeliverySystem>,
    /// The device, while leased. The input hands out reads on it and stops
    /// the stream when it goes, and giving the lease back closes it.
    device: Mutex<Option<Arc<Device>>>,
}

impl Px4Tuner {
    pub fn new(
        target: Target,
        lnb_voltage: u8,
        delivery_systems: &[DeliverySystem],
    ) -> anyhow::Result<Self> {
        if !matches!(lnb_voltage, 0 | 11 | 15) {
            bail!("The LNB voltage of {target} has to be 0, 11 or 15");
        }
        if delivery_systems.contains(&DeliverySystem::IsdbS3) {
            bail!("A px4_drv tuner receives 2K only, not ISDB-S3");
        }

        // The device is only held while leased, so a wrong one is caught now
        // rather than at the first tune.
        Device::check(&target)?;

        info!("Found the px4_drv tuner: {target}");

        Ok(Self {
            target,
            lnb_voltage,
            delivery_systems: delivery_systems.to_vec(),
            device: Mutex::new(None),
        })
    }

    /// The device, opened if the lease has not reached for it yet.
    fn device(&self) -> anyhow::Result<Arc<Device>> {
        let mut device = lock(&self.device)?;
        if let Some(device) = device.as_ref() {
            return Ok(Arc::clone(device));
        }

        let opened = Arc::new(Device::open(&self.target, &self.delivery_systems)?);
        *device = Some(Arc::clone(&opened));

        Ok(opened)
    }
}

impl Tuner for Px4Tuner {
    fn open(&self) -> anyhow::Result<Box<dyn Read + Send + Sync>> {
        let device = self.device()?;
        device
            .start_streaming()
            .context("Could not start streaming from the px4_drv device")?;

        let (tx, rx) = sync_channel(BUFFERED_CHUNKS);
        let reader = Arc::clone(&device);
        thread::Builder::new()
            .name("px4-reader".to_string())
            .spawn(move || {
                let mut buf = vec![0; CHUNK_SIZE];
                loop {
                    match reader.read(&mut buf) {
                        // The stream was stopped, which is what a read
                        // returns nothing for.
                        Ok(0) => break,
                        Ok(len) => match tx.try_send(buf[..len].to_vec()) {
                            Ok(()) => {}
                            // Dropping here is what keeps the driver's own
                            // buffer from overflowing silently.
                            Err(TrySendError::Full(_)) => error!("Buffer overrun!"),
                            Err(TrySendError::Disconnected(_)) => break,
                        },
                        Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                        Err(error) => {
                            warn!("Could not read from the px4_drv device: {error}");
                            break;
                        }
                    }
                }
            })
            .context("Could not start the px4_drv reader thread")?;

        Ok(Box::new(Px4Input {
            device,
            chunks: Mutex::new(rx),
            chunk: Vec::new(),
            offset: 0,
        }))
    }

    fn tune(&self, channel: Channel) -> anyhow::Result<()> {
        let tuning = match channel.inner {
            ChannelInner::IsdbT { frequency, .. } => Tuning::Terrestrial {
                frequency_hz: frequency,
            },
            ChannelInner::IsdbS {
                frequency,
                stream_id,
            } => Tuning::Satellite {
                frequency_khz: frequency,
                stream_id,
            },
            ChannelInner::IsdbS3 { .. } => {
                bail!("A px4_drv tuner receives 2K only, not ISDB-S3");
            }
            ChannelInner::BonIsdbT { .. }
            | ChannelInner::BonIsdbS { .. }
            | ChannelInner::BonIsdbS3 { .. } => {
                bail!("A px4_drv tuner cannot take a BonDriver channel");
            }
        };
        let system = tuning.system();

        let device = self.device()?;

        // A device receiving only one of the two says so here, rather than
        // tuning to a frequency of the other.
        device.set_system(system, &self.target)?;

        if system == System::IsdbS && self.lnb_voltage != 0 {
            device
                .set_lnb_voltage(self.lnb_voltage)
                .context("Could not power the LNB")?;
        }

        info!("Tuning to {}: {tuning:?}", channel.name);
        device.tune(tuning)?;

        match device.cnr_db(system) {
            Ok(cnr) => info!("C/N: {cnr:.2} dB"),
            Err(error) => info!("Tuned, C/N not available: {error}"),
        }

        Ok(())
    }

    fn close(&self) {
        if let Ok(mut device) = self.device.lock() {
            device.take();
        }
    }
}

/// The stream a [`Px4Tuner`] hands out: what the reader thread took off the
/// device, chunk by chunk, with the stream stopped once it is dropped.
struct Px4Input {
    device: Arc<Device>,
    // Only ever taken from here, but a `Receiver` is not `Sync` on its own.
    chunks: Mutex<Receiver<Vec<u8>>>,
    chunk: Vec<u8>,
    offset: usize,
}

impl Read for Px4Input {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        while self.offset >= self.chunk.len() {
            let chunks = self
                .chunks
                .lock()
                .map_err(|_| io::Error::other("The px4_drv input lock was poisoned"))?;
            // The reader thread is gone, so the stream is over.
            let Ok(chunk) = chunks.recv() else {
                return Ok(0);
            };

            self.chunk = chunk;
            self.offset = 0;
        }

        let len = (self.chunk.len() - self.offset).min(buf.len());
        buf[..len].copy_from_slice(&self.chunk[self.offset..self.offset + len]);
        self.offset += len;

        Ok(len)
    }
}

impl Drop for Px4Input {
    fn drop(&mut self) {
        // Stopping the stream is also what lets the reader thread out of its
        // read. Nothing can be done about a failure here, and the driver
        // stops the stream itself once the device is closed anyway.
        let _ = self.device.stop_streaming();
    }
}

fn lock(
    device: &Mutex<Option<Arc<Device>>>,
) -> anyhow::Result<MutexGuard<'_, Option<Arc<Device>>>> {
    device
        .lock()
        .map_err(|_| anyhow::anyhow!("The px4_drv device lock was poisoned"))
}
