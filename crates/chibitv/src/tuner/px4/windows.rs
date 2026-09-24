//! px4_drv on Windows, where the tuners are bound to WinUSB and driven by
//! `DriverHost_PX4`, a user-mode driver the BonDriver of px4_drv talks to over
//! two named pipes: one taking commands, one handing out the stream. This
//! talks to it the same way, so that the channels are tuned by frequency, as
//! the store keeps them, rather than by the channel list of a BonDriver.
//!
//! `DriverHost_PX4` is not a service: whoever needs it starts it, and it quits
//! once nobody has been connected for some fifteen seconds. So it is started
//! here too, whenever a receiver is opened and it is not running.
//!
//! The commands are the structures of `winusb/src/common/command.hpp` of
//! <https://github.com/tsukumijima/px4_drv>, each answered with the same
//! structure, its status filled in.

use std::fmt::{self, Display, Formatter};
use std::fs::{File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::windows::io::AsRawHandle;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::ptr::null_mut;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, bail, ensure};
use tracing::info;
use windows_sys::Win32::Foundation::ERROR_PIPE_BUSY;
use windows_sys::Win32::System::Pipes::PeekNamedPipe;

use super::{System, Tuning};
use crate::channel::DeliverySystem;

const CTRL_PIPE: &str = r"\\.\pipe\px4_ctrl_pipe";
const DATA_PIPE: &str = r"\\.\pipe\px4_data_pipe";

/// The version of the commands this speaks, which `DriverHost_PX4` bumps
/// whenever it changes them.
const CMD_VERSION: u32 = 0x0004_0002;

// `CtrlCmdCode`.
const GET_VERSION: u32 = 1;
const OPEN: u32 = 8;
const SET_CAPTURE: u32 = 11;
const SET_PARAMS: u32 = 17;
const TUNE: u32 = 19;
const SET_LNB_VOLTAGE: u32 = 24;
const READ_STATS: u32 = 32;

// `DataCmdCode`.
const SET_DATA_ID: u32 = 1;
const PURGE: u32 = 8;

/// `CtrlStatusCode::SUCCEEDED`.
const SUCCEEDED: u32 = 1;
/// `ParameterType::STREAM_ID`.
const PARAMETER_STREAM_ID: u32 = 16;
/// `StatType::CNR`.
const STAT_CNR: u32 = 2;

/// The layout of `struct ReceiverInfo`: a name of 96 UTF-16 units and a GUID
/// for the device, the same for the receiver, then the broadcasts it receives,
/// its index and the id its stream is handed out by.
const RECEIVER_INFO_SIZE: usize = 428;
const RECEIVER_NAME_OFFSET: usize = 208;
const NAME_LEN: usize = 96;
const SYSTEMS_OFFSET: usize = 416;
const INDEX_OFFSET: usize = 420;
const DATA_ID_OFFSET: usize = 424;
const _: () = assert!(RECEIVER_INFO_SIZE == (NAME_LEN * 2 + 16) * 2 + 4 * 3);
const _: () = assert!(RECEIVER_NAME_OFFSET == NAME_LEN * 2 + 16);

/// How long the demodulator is given to lock, as the BonDriver gives it.
const TUNE_TIMEOUT_MS: u32 = 5_000;
/// How long a connection waits while every instance of a pipe is taken.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(2);
/// How long `DriverHost_PX4` is given to come up, as the BonDriver gives it.
const STARTUP_TIMEOUT: Duration = Duration::from_secs(10);
/// How often the stream is looked at while it has nothing to read.
const POLL_INTERVAL: Duration = Duration::from_millis(5);

pub struct Target {
    /// The receiver to open, by the name `DriverHost_PX4.ini` gives it, or
    /// any free one receiving the broadcasts the tuner is configured for.
    pub receiver: Option<String>,
    /// The `DriverHost_PX4.exe` to start when it is not running, relative to
    /// the working directory.
    pub driver_host: PathBuf,
}

impl Display for Target {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        match &self.receiver {
            Some(name) => f.write_str(name),
            None => f.write_str("any free px4_drv receiver"),
        }
    }
}

/// A receiver, opened.
pub struct Device {
    ctrl: Mutex<File>,
    data: File,
    /// The broadcasts the receiver takes, as `px4::SystemType` bits.
    systems: u32,
    streaming: AtomicBool,
}

impl Device {
    pub fn check(target: &Target) -> anyhow::Result<()> {
        // Nothing tells whether a receiver is there short of opening it, but
        // the driver is what every lease starts from.
        std::fs::metadata(&target.driver_host).with_context(|| {
            format!(
                "Could not find DriverHost_PX4 at {}",
                target.driver_host.display()
            )
        })?;

        Ok(())
    }

    pub fn open(target: &Target, delivery_systems: &[DeliverySystem]) -> anyhow::Result<Self> {
        let mut ctrl = connect_ctrl(&target.driver_host)?;

        let version = call(&mut ctrl, GET_VERSION, &[0; 8])?;
        let cmd_version = u32_at(&version, 4);
        ensure!(
            cmd_version == CMD_VERSION,
            "DriverHost_PX4 speaks version {cmd_version:#X} of its commands, not {CMD_VERSION:#X}"
        );

        let mut key = [0; RECEIVER_INFO_SIZE];
        if let Some(name) = &target.receiver {
            let name: Vec<u16> = name.encode_utf16().collect();
            ensure!(
                name.len() < NAME_LEN,
                "The receiver name {target} is too long"
            );
            for (i, unit) in name.into_iter().enumerate() {
                key[RECEIVER_NAME_OFFSET + i * 2..][..2].copy_from_slice(&unit.to_le_bytes());
            }
        }
        let systems =
            delivery_systems
                .iter()
                .fold(0, |systems, delivery_system| match delivery_system {
                    DeliverySystem::IsdbT => systems | System::IsdbT as u32,
                    DeliverySystem::IsdbS => systems | System::IsdbS as u32,
                    DeliverySystem::IsdbS3 => systems,
                });
        put_u32(&mut key, SYSTEMS_OFFSET, systems);
        // Any index.
        put_u32(&mut key, INDEX_OFFSET, u32::MAX);

        let info = call(&mut ctrl, OPEN, &key).with_context(|| {
            format!("Could not open {target}; is it in use by another program?")
        })?;

        let mut data =
            connect(DATA_PIPE).with_context(|| format!("Could not connect to {DATA_PIPE}"))?;
        data.write_all(&data_cmd(SET_DATA_ID, u32_at(&info, DATA_ID_OFFSET)))
            .context("Could not attach to the stream of the px4_drv receiver")?;

        let name: Vec<u16> = info[RECEIVER_NAME_OFFSET..][..NAME_LEN * 2]
            .as_chunks::<2>()
            .0
            .iter()
            .map(|&unit| u16::from_le_bytes(unit))
            .take_while(|&unit| unit != 0)
            .collect();
        info!(
            "Opened the px4_drv receiver {}",
            String::from_utf16_lossy(&name)
        );

        Ok(Self {
            ctrl: Mutex::new(ctrl),
            data,
            systems: u32_at(&info, SYSTEMS_OFFSET),
            streaming: AtomicBool::new(false),
        })
    }

    // ponytail: polls rather than blocks, as a blocking read on a pipe could
    // not be let out of once the stream stops; overlapped I/O if the latency
    // of the poll ever matters.
    pub fn read(&self, buf: &mut [u8]) -> io::Result<usize> {
        loop {
            if !self.streaming.load(Ordering::Acquire) {
                return Ok(0);
            }

            let mut available = 0;
            // SAFETY: the handle is open for as long as `self` is, and only
            // the count of what is buffered is written, to a live local.
            let peeked = unsafe {
                PeekNamedPipe(
                    self.data.as_raw_handle(),
                    null_mut(),
                    0,
                    null_mut(),
                    &mut available,
                    null_mut(),
                )
            };
            if peeked == 0 {
                return Err(io::Error::last_os_error());
            }

            if available > 0 {
                let len = buf.len().min(available as usize);
                return (&self.data).read(&mut buf[..len]);
            }

            thread::sleep(POLL_INTERVAL);
        }
    }

    pub fn set_system(&self, system: System, target: &Target) -> anyhow::Result<()> {
        if self.systems & system as u32 == 0 {
            bail!("{target} does not receive {system}");
        }

        Ok(())
    }

    pub fn tune(&self, tuning: Tuning) -> anyhow::Result<()> {
        let (frequency_khz, num, stream_id) = match tuning {
            Tuning::Terrestrial { frequency_hz } => ((frequency_hz + 500) / 1_000, 0, 0),
            Tuning::Satellite {
                frequency_khz,
                stream_id,
            } => (frequency_khz, 1, stream_id),
        };

        // `struct ParameterSet`, with room for its one parameter even when
        // it is not used, as the driver reads that much regardless.
        let mut params = Vec::with_capacity(20);
        for value in [
            tuning.system() as u32,
            frequency_khz,
            num,
            if num == 0 { 0 } else { PARAMETER_STREAM_ID },
            stream_id,
        ] {
            params.extend_from_slice(&value.to_le_bytes());
        }

        self.ctrl(SET_PARAMS, &params)
            .context("Could not set the tuning parameters")?;
        // The driver waits for the demodulator to lock, and fails the
        // command when it does not.
        self.ctrl(TUNE, &TUNE_TIMEOUT_MS.to_le_bytes())
            .context("No signal")?;

        // What was buffered before the tune is of the previous channel.
        (&self.data)
            .write_all(&data_cmd(PURGE, 0))
            .context("Could not purge the stream")
    }

    pub fn cnr_db(&self, _system: System) -> io::Result<f64> {
        // `struct StatSet` asking for the one value, in thousandths of a dB.
        let mut stats = Vec::with_capacity(12);
        for value in [1, STAT_CNR, 0] {
            stats.extend_from_slice(&value.to_le_bytes());
        }

        let stats = self.ctrl(READ_STATS, &stats)?;
        Ok(f64::from(u32_at(&stats, 8) as i32) / 1_000.0)
    }

    pub fn set_lnb_voltage(&self, voltage: u8) -> io::Result<()> {
        self.ctrl(SET_LNB_VOLTAGE, &i32::from(voltage).to_le_bytes())
            .map(drop)
    }

    pub fn start_streaming(&self) -> io::Result<()> {
        self.set_capture(true)?;
        self.streaming.store(true, Ordering::Release);
        Ok(())
    }

    pub fn stop_streaming(&self) -> io::Result<()> {
        self.streaming.store(false, Ordering::Release);
        self.set_capture(false)
    }

    fn set_capture(&self, capture: bool) -> io::Result<()> {
        // A `bool` padded to the alignment of the structure.
        self.ctrl(SET_CAPTURE, &[u8::from(capture), 0, 0, 0])
            .map(drop)
    }

    fn ctrl(&self, cmd: u32, body: &[u8]) -> io::Result<Vec<u8>> {
        let mut ctrl = self
            .ctrl
            .lock()
            .map_err(|_| io::Error::other("The px4_drv command pipe lock was poisoned"))?;

        call(&mut ctrl, cmd, body)
    }
}

impl Drop for Device {
    fn drop(&mut self) {
        // The driver closes the receiver once the command pipe goes, but
        // leaves the LNB as it was.
        if self.systems & System::IsdbS as u32 != 0 {
            let _ = self.set_lnb_voltage(0);
        }
    }
}

/// Connects to the command pipe, starting `DriverHost_PX4` first when it is
/// not there. Starting it twice is harmless: the second one quits at once.
fn connect_ctrl(driver_host: &Path) -> anyhow::Result<File> {
    match connect(CTRL_PIPE) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        result => return result.with_context(|| format!("Could not connect to {CTRL_PIPE}")),
    }

    // A bare file name would be looked for on the `PATH` rather than in the
    // working directory.
    let driver_host = Path::new(".").join(driver_host);
    info!("Starting {}", driver_host.display());
    Command::new(&driver_host)
        .spawn()
        .with_context(|| format!("Could not start {}", driver_host.display()))?;

    let deadline = Instant::now() + STARTUP_TIMEOUT;
    loop {
        match connect(CTRL_PIPE) {
            Err(error) if error.kind() == io::ErrorKind::NotFound && Instant::now() < deadline => {
                thread::sleep(Duration::from_millis(100));
            }
            result => {
                return result
                    .with_context(|| format!("DriverHost_PX4 did not come up at {CTRL_PIPE}"));
            }
        }
    }
}

fn connect(path: &str) -> io::Result<File> {
    let deadline = Instant::now() + CONNECT_TIMEOUT;
    loop {
        match OpenOptions::new().read(true).write(true).open(path) {
            // Every instance of the pipe is taken for the moment.
            Err(error)
                if error.raw_os_error() == Some(ERROR_PIPE_BUSY as i32)
                    && Instant::now() < deadline =>
            {
                thread::sleep(Duration::from_millis(50));
            }
            result => return result,
        }
    }
}

/// Sends a command, a `CtrlCmdHeader` followed by `body`, and returns the body
/// of the answer, which is as long.
fn call(pipe: &mut File, cmd: u32, body: &[u8]) -> io::Result<Vec<u8>> {
    let mut message = Vec::with_capacity(8 + body.len());
    message.extend_from_slice(&cmd.to_le_bytes());
    message.extend_from_slice(&0u32.to_le_bytes());
    message.extend_from_slice(body);

    pipe.write_all(&message)?;
    pipe.read_exact(&mut message)?;

    if u32_at(&message, 4) != SUCCEEDED {
        return Err(io::Error::other(format!(
            "DriverHost_PX4 failed command {cmd}"
        )));
    }

    Ok(message.split_off(8))
}

/// `struct DataCmd`.
fn data_cmd(cmd: u32, data_id: u32) -> [u8; 8] {
    let mut message = [0; 8];
    put_u32(&mut message, 0, cmd);
    put_u32(&mut message, 4, data_id);
    message
}

fn u32_at(buf: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(buf[offset..offset + 4].try_into().unwrap())
}

fn put_u32(buf: &mut [u8], offset: usize, value: u32) {
    buf[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}
