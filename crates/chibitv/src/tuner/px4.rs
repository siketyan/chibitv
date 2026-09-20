//! A tuner driven through px4_drv, the Linux driver of the PLEX and Digibest
//! tuners (PX-W3U4, PX-MLT5PE, DTV02A-1T1S-U and the like).
//!
//! The driver exposes each tuner as a character device, `/dev/pxmlt5video0`
//! for instance, and drives it with the ioctls of the PT1/PT3 drivers it stays
//! compatible with (`include/ptx_ioctl.h` of
//! <https://github.com/tsukumijima/px4_drv>). Those name a channel by the
//! number recpt1 gives it and a slot rather than by a frequency, so the
//! frequency a channel is kept as is turned back into that pair here.
//!
//! The device is opened for as long as it is leased and given back after, as
//! opening it is what powers the tuner up, and the driver lets only one
//! program have it at a time.
//!
//! The driver buffers only `tsdev_max_packets` packets in the kernel, 2048 by
//! default, which is under a tenth of a second of a satellite stream: a reader
//! held up by a card exchange or a database write would lose packets. So the
//! device is read on its own thread, and what it reads is kept for the
//! consumer in memory.

use std::fs::File;
use std::io::{self, Read};
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, TrySendError, sync_channel};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread;

use anyhow::{Context, bail};
use libc::{Ioctl, c_int, c_ulong};
use tracing::{error, info, warn};

use crate::channel::{Channel, ChannelInner};
use crate::tuner::Tuner;

// The ioctl numbers, as `asm-generic/ioctl.h` lays them out on every
// architecture the driver is built for.
const IOC_NONE: u32 = 0;
const IOC_WRITE: u32 = 1;
const IOC_READ: u32 = 2;

const fn ioc(dir: u32, ty: u32, nr: u32, size: usize) -> Ioctl {
    ((dir << 30) | ((size as u32) << 16) | (ty << 8) | nr) as Ioctl
}

/// `PTX_SET_CHANNEL`, which tunes and waits for the demodulator to lock.
const PTX_SET_CHANNEL: Ioctl = ioc(IOC_WRITE, 0x8d, 0x01, size_of::<PtxFreq>());
const PTX_START_STREAMING: Ioctl = ioc(IOC_NONE, 0x8d, 0x02, 0);
const PTX_STOP_STREAMING: Ioctl = ioc(IOC_NONE, 0x8d, 0x03, 0);
/// `PTX_GET_CNR`, whose size is that of a pointer: the header declares it over
/// `int *` rather than `int`.
const PTX_GET_CNR: Ioctl = ioc(IOC_READ, 0x8d, 0x04, size_of::<*mut c_int>());
const PTX_ENABLE_LNB_POWER: Ioctl = ioc(IOC_WRITE, 0x8d, 0x05, size_of::<c_int>());
const PTX_DISABLE_LNB_POWER: Ioctl = ioc(IOC_NONE, 0x8d, 0x06, 0);
/// `PTX_SET_SYSTEM_MODE`, a px4_drv addition for a device receiving both
/// ISDB-T and ISDB-S, which the PT ioctls tell apart by the channel number
/// alone.
const PTX_SET_SYSTEM_MODE: Ioctl = ioc(IOC_WRITE, 0x8d, 0x0b, size_of::<c_int>());

/// How much the reader thread takes off the device at once.
const CHUNK_SIZE: usize = 188 * 1024;
/// How many chunks are kept for a consumer that falls behind, before the
/// stream is cut to catch up: some six seconds of a satellite stream.
const BUFFERED_CHUNKS: usize = 128;

/// `enum ptx_system_type`.
const PTX_ISDB_T_SYSTEM: c_ulong = 0x10;
const PTX_ISDB_S_SYSTEM: c_ulong = 0x20;

/// `struct ptx_freq`: the channel number and, for a satellite channel, the
/// stream it carries to pick; for a terrestrial one, an offset in kHz.
#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct PtxFreq {
    freq_no: c_int,
    slot: c_int,
}

/// The centre frequency of a channel number, in kHz, as the driver works it
/// out: UHF 13 to 62 are numbered 63 to 112, and the CATV channels C13 to C22
/// then C23 to C63 are numbered 3 to 12 then 22 to 62.
fn terrestrial_frequency_khz(freq_no: u32) -> Option<u32> {
    match freq_no {
        3..=12 => Some(93_143 + freq_no * 6_000 + if freq_no == 12 { 2_000 } else { 0 }),
        22..=62 => Some(93_143 + freq_no * 6_000),
        63..=112 => Some(95_143 + freq_no * 6_000),
        _ => None,
    }
}

/// The frequency of a satellite channel number, in kHz: the BS transponders
/// are numbered 0 to 11 and the CS110 ones 12 to 23.
fn satellite_frequency_khz(freq_no: u32) -> Option<u32> {
    match freq_no {
        0..=11 => Some(1_049_480 + 38_360 * freq_no),
        12..=23 => Some(1_613_000 + 40_000 * (freq_no - 12)),
        _ => None,
    }
}

/// The channel number nearest to the frequency, out of the numbers `frequency`
/// gives a frequency for, as long as it is within `tolerance_khz` of it.
fn nearest_channel_number(
    frequency_khz: u32,
    numbers: impl Iterator<Item = u32>,
    frequency: impl Fn(u32) -> Option<u32>,
    tolerance_khz: u32,
) -> Option<(u32, i32)> {
    numbers
        .filter_map(|number| {
            let offset = frequency_khz as i64 - i64::from(frequency(number)?);
            (offset.unsigned_abs() <= u64::from(tolerance_khz)).then_some((number, offset as i32))
        })
        .min_by_key(|(_, offset)| offset.unsigned_abs())
}

/// The channel number and slot a terrestrial channel is tuned with, given
/// its centre frequency in Hz.
fn terrestrial_channel(frequency_hz: u32) -> anyhow::Result<PtxFreq> {
    let frequency_khz = (frequency_hz + 500) / 1_000;

    // Anything within half a channel is a channel with an offset, which the
    // slot carries in kHz.
    let Some((freq_no, offset_khz)) =
        nearest_channel_number(frequency_khz, 3..=112, terrestrial_frequency_khz, 3_000)
    else {
        bail!("{frequency_hz} Hz is not on a UHF or CATV channel px4_drv can tune to");
    };

    Ok(PtxFreq {
        freq_no: freq_no as c_int,
        slot: offset_khz,
    })
}

/// The channel number and slot a satellite stream is tuned with, given the
/// frequency of its transponder in kHz and its stream id, which is either the
/// transport stream id or the stream's number on the transponder when below 12.
fn satellite_channel(frequency_khz: u32, stream_id: u32) -> anyhow::Result<PtxFreq> {
    let Some((freq_no, _)) =
        nearest_channel_number(frequency_khz, 0..=23, satellite_frequency_khz, 1_000)
    else {
        bail!("{frequency_khz} kHz is not a BS or CS110 transponder px4_drv can tune to");
    };
    let Ok(slot) = c_int::try_from(stream_id) else {
        bail!("{stream_id:#X} is not a stream id px4_drv can pick");
    };

    Ok(PtxFreq {
        freq_no: freq_no as c_int,
        slot,
    })
}

/// The character device, opened.
struct Device {
    file: File,
}

impl Device {
    fn open(path: &Path) -> anyhow::Result<Self> {
        let file = File::open(path).with_context(|| {
            format!(
                "Could not open the px4_drv device {}; is it plugged in, and is it in use by another program?",
                path.display()
            )
        })?;

        Ok(Self { file })
    }

    /// An ioctl taking a value, or nothing.
    fn ioctl(&self, request: Ioctl, arg: c_ulong) -> io::Result<()> {
        // SAFETY: `request` is one the driver takes a value, or nothing, for.
        check(unsafe { libc::ioctl(self.file.as_raw_fd(), request, arg) })
    }

    /// An ioctl taking a pointer, which lives for the call.
    fn ioctl_ptr<T>(&self, request: Ioctl, arg: &mut T) -> io::Result<()> {
        // SAFETY: `request` is one the driver takes a pointer to a `T` for,
        // and `arg` outlives the call.
        check(unsafe { libc::ioctl(self.file.as_raw_fd(), request, arg as *mut T) })
    }

    fn set_system_mode(&self, system: c_ulong) -> io::Result<()> {
        self.ioctl(PTX_SET_SYSTEM_MODE, system)
    }

    fn set_channel(&self, freq: PtxFreq) -> io::Result<()> {
        let mut freq = freq;
        self.ioctl_ptr(PTX_SET_CHANNEL, &mut freq)
    }

    fn set_lnb_voltage(&self, voltage: u8) -> io::Result<()> {
        match voltage {
            0 => self.ioctl(PTX_DISABLE_LNB_POWER, 0),
            11 => self.ioctl(PTX_ENABLE_LNB_POWER, 1),
            15 => self.ioctl(PTX_ENABLE_LNB_POWER, 2),
            _ => Err(io::Error::from(io::ErrorKind::InvalidInput)),
        }
    }

    fn start_streaming(&self) -> io::Result<()> {
        self.ioctl(PTX_START_STREAMING, 0)
    }

    fn stop_streaming(&self) -> io::Result<()> {
        self.ioctl(PTX_STOP_STREAMING, 0)
    }

    /// The carrier-to-noise ratio, as the register value the PT drivers report.
    fn cnr_raw(&self) -> io::Result<u32> {
        let mut value: u32 = 0;
        self.ioctl_ptr(PTX_GET_CNR, &mut value)?;

        Ok(value)
    }
}

fn check(ret: c_int) -> io::Result<()> {
    if ret < 0 {
        return Err(io::Error::last_os_error());
    }

    Ok(())
}

/// The carrier-to-noise ratio in dB, out of the value the driver reports, by
/// the curve and the table recpt1 converts it with.
fn cnr_db(system: c_ulong, raw: u32) -> f64 {
    if system == PTX_ISDB_T_SYSTEM {
        if raw == 0 {
            return 0.0;
        }

        let p = (5_505_024.0 / f64::from(raw)).log10() * 10.0;
        return 0.000024 * p.powi(4) - 0.0016 * p.powi(3)
            + 0.0398 * p.powi(2)
            + 0.5491 * p
            + 3.0965;
    }

    const LEVELS: [f64; 14] = [
        24.07, 24.07, 18.61, 15.21, 12.50, 10.19, 8.140, 6.270, 4.550, 3.730, 3.630, 2.940, 1.420,
        0.000,
    ];

    // A 16-bit value, interpolated between the levels of every 0x1000.
    let high = ((raw >> 8) & 0xFF) as usize;
    if high <= 0x10 {
        return 24.07;
    }
    if high >= 0xB0 {
        return 0.0;
    }

    let mix = f64::from(((high & 0x0F) << 8) as u16 | (raw & 0xFF) as u16) / 4096.0;
    LEVELS[high >> 4] * (1.0 - mix) + LEVELS[(high >> 4) + 1] * mix
}

pub struct Px4Tuner {
    path: PathBuf,
    lnb_voltage: u8,
    /// The device, while leased. The input hands out reads on it and stops
    /// the stream when it goes, and giving the lease back closes it.
    device: Mutex<Option<Arc<Device>>>,
}

impl Px4Tuner {
    pub fn new(path: impl AsRef<Path>, lnb_voltage: u8) -> anyhow::Result<Self> {
        let path = path.as_ref().to_path_buf();
        if !matches!(lnb_voltage, 0 | 11 | 15) {
            bail!(
                "The LNB voltage of {} has to be 0, 11 or 15",
                path.display()
            );
        }

        // The device is only held while leased, so a wrong path is caught now
        // rather than at the first tune.
        std::fs::metadata(&path)
            .with_context(|| format!("Could not find the px4_drv device {}", path.display()))?;

        info!("Found the px4_drv tuner: {}", path.display());

        Ok(Self {
            path,
            lnb_voltage,
            device: Mutex::new(None),
        })
    }

    /// The device, opened if the lease has not reached for it yet.
    fn device(&self) -> anyhow::Result<Arc<Device>> {
        let mut device = lock(&self.device)?;
        if let Some(device) = device.as_ref() {
            return Ok(Arc::clone(device));
        }

        let opened = Arc::new(Device::open(&self.path)?);
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
                    match (&reader.file).read(&mut buf) {
                        // The stream was stopped, which is what a read
                        // returns nothing for.
                        Ok(0) => break,
                        Ok(len) => match tx.try_send(buf[..len].to_vec()) {
                            Ok(()) => {}
                            // Dropping here is what keeps the kernel's own
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
        let (system, freq) = match channel.inner {
            ChannelInner::IsdbT { frequency, .. } => {
                (PTX_ISDB_T_SYSTEM, terrestrial_channel(frequency)?)
            }
            ChannelInner::IsdbS {
                frequency,
                stream_id,
            } => (PTX_ISDB_S_SYSTEM, satellite_channel(frequency, stream_id)?),
            ChannelInner::IsdbS3 { .. } => {
                bail!("A px4_drv tuner receives 2K only, not ISDB-S3");
            }
            ChannelInner::BonIsdbT { .. }
            | ChannelInner::BonIsdbS { .. }
            | ChannelInner::BonIsdbS3 { .. } => {
                bail!("A px4_drv tuner cannot take a BonDriver channel");
            }
        };

        let device = self.device()?;

        // A device receiving only one of the two says so here, rather than
        // reading the channel number as one of the other.
        match device.set_system_mode(system) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::InvalidInput => {
                bail!(
                    "{} does not receive {}",
                    self.path.display(),
                    channel.inner.delivery_system(),
                );
            }
            // The PT drivers the ioctls come from have no such mode and tell
            // the two apart by the channel number, which is fine for a UHF
            // channel and a satellite one.
            Err(error) if error.kind() == io::ErrorKind::Unsupported => {
                warn!(
                    "{} does not take a system mode; going by the channel number",
                    self.path.display()
                );
            }
            Err(error) => {
                return Err(anyhow::Error::from(error).context("Could not set the system mode"));
            }
        }

        if system == PTX_ISDB_S_SYSTEM && self.lnb_voltage != 0 {
            device
                .set_lnb_voltage(self.lnb_voltage)
                .context("Could not power the LNB")?;
        }

        info!(
            "Tuning to {}, channel number {} slot {}",
            channel.name, freq.freq_no, freq.slot
        );

        // The driver waits for the demodulator to lock itself, and gives up
        // with `EAGAIN` when it does not.
        device.set_channel(freq).map_err(|error| {
            if error.kind() == io::ErrorKind::WouldBlock {
                anyhow::anyhow!("No signal")
            } else {
                anyhow::Error::from(error).context("Could not tune")
            }
        })?;

        match device.cnr_raw() {
            Ok(raw) => info!("C/N: {:.2} dB", cnr_db(system, raw)),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_the_ioctls_as_the_driver_does() {
        // As `include/ptx_ioctl.h` compiles to on a 64-bit build.
        if size_of::<usize>() == 8 {
            assert_eq!(PTX_GET_CNR as u32, 0x8008_8d04);
        }
        assert_eq!(PTX_SET_CHANNEL as u32, 0x4008_8d01);
        assert_eq!(PTX_START_STREAMING as u32, 0x8d02);
        assert_eq!(PTX_STOP_STREAMING as u32, 0x8d03);
        assert_eq!(PTX_ENABLE_LNB_POWER as u32, 0x4004_8d05);
        assert_eq!(PTX_DISABLE_LNB_POWER as u32, 0x8d06);
        assert_eq!(PTX_SET_SYSTEM_MODE as u32, 0x4004_8d0b);
    }

    #[test]
    fn numbers_a_uhf_channel_after_the_pt_drivers() {
        // UHF 13, as a scan keeps it: 473.142857 MHz, or 63 to the driver.
        assert_eq!(
            terrestrial_channel(473_142_857).unwrap(),
            PtxFreq {
                freq_no: 63,
                slot: 0
            }
        );
        // UHF 27, TOKYO MX.
        assert_eq!(
            terrestrial_channel(557_142_857).unwrap(),
            PtxFreq {
                freq_no: 77,
                slot: 0
            }
        );
        assert_eq!(
            terrestrial_channel(767_142_857).unwrap(),
            PtxFreq {
                freq_no: 112,
                slot: 0
            }
        );
    }

    #[test]
    fn carries_an_offset_from_the_channel_centre_in_the_slot() {
        assert_eq!(
            terrestrial_channel(473_143_000 + 142_000).unwrap(),
            PtxFreq {
                freq_no: 63,
                slot: 142
            }
        );
        // C13, the first CATV channel, sits below UHF and is numbered 3.
        assert_eq!(
            terrestrial_channel(111_143_000).unwrap(),
            PtxFreq {
                freq_no: 3,
                slot: 0
            }
        );
    }

    #[test]
    fn refuses_a_frequency_off_the_channel_grid() {
        assert!(terrestrial_channel(1_000_000).is_err());
        assert!(terrestrial_channel(800_000_000).is_err());
        assert!(satellite_channel(1_000_000, 0).is_err());
    }

    #[test]
    fn numbers_a_satellite_transponder_after_the_pt_drivers() {
        // BS-1 with the relative stream number a scan starts from.
        assert_eq!(
            satellite_channel(1_049_480, 0).unwrap(),
            PtxFreq {
                freq_no: 0,
                slot: 0
            }
        );
        // BS-15 by transport stream id.
        assert_eq!(
            satellite_channel(1_049_480 + 38_360 * 7, 0x40F1).unwrap(),
            PtxFreq {
                freq_no: 7,
                slot: 0x40F1
            }
        );
        // ND2, the first CS110 transponder.
        assert_eq!(
            satellite_channel(1_613_000, 0x6020).unwrap(),
            PtxFreq {
                freq_no: 12,
                slot: 0x6020
            }
        );
        assert_eq!(
            satellite_channel(1_613_000 + 40_000 * 11, 0).unwrap(),
            PtxFreq {
                freq_no: 23,
                slot: 0
            }
        );
    }

    #[test]
    fn converts_the_carrier_to_noise_ratio_as_recpt1_does() {
        assert!((cnr_db(PTX_ISDB_T_SYSTEM, 5_505_024) - 3.0965).abs() < 0.001);
        // Two values off the PX-MLT driver's table, the smaller the cleaner.
        let noisier = cnr_db(PTX_ISDB_T_SYSTEM, 0x36F9D);
        let cleaner = cnr_db(PTX_ISDB_T_SYSTEM, 0x1844D);
        assert!((14.0..16.0).contains(&noisier), "{noisier}");
        assert!((18.0..19.0).contains(&cleaner), "{cleaner}");
        assert_eq!(cnr_db(PTX_ISDB_S_SYSTEM, 0x1000), 24.07);
        assert_eq!(cnr_db(PTX_ISDB_S_SYSTEM, 0xB000), 0.0);
        assert_eq!(cnr_db(PTX_ISDB_S_SYSTEM, 0x4000), 12.50);
        let mid = cnr_db(PTX_ISDB_S_SYSTEM, 0x4800);
        assert!((mid - (12.50 + 10.19) / 2.0).abs() < 0.001);
    }
}
