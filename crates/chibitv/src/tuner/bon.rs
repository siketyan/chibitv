use std::io::{self, Read};
use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use anyhow::bail;
use chibitv_bon::BonDriver;
use tracing::info;

use crate::channel::{Channel, ChannelInner};
use crate::tuner::Tuner;

/// How long a read waits for the driver to buffer something before asking again.
const WAIT_TIMEOUT_MS: u32 = 1_000;

/// How long the demodulator is given to settle after a channel change.
const SETTLE_TIME: Duration = Duration::from_millis(500);

/// A tuner driven through a BonDriver DLL.
///
/// A BonDriver carries its own configuration, so unlike a DVB device it is not
/// told a frequency: it is told which of the channels it enumerates to tune to.
pub struct BonTuner {
    driver: Arc<Mutex<BonDriver>>,
}

impl BonTuner {
    pub fn new(path: impl AsRef<Path>) -> anyhow::Result<Self> {
        let mut driver = BonDriver::open(path)?;
        driver.open_tuner()?;

        info!(
            "Opened the BonDriver tuner: {}",
            driver.tuner_name().as_deref().unwrap_or("unnamed"),
        );

        Ok(Self {
            driver: Arc::new(Mutex::new(driver)),
        })
    }
}

impl Tuner for BonTuner {
    fn open(&self) -> anyhow::Result<Box<dyn Read + Send + Sync>> {
        Ok(Box::new(BonInput {
            driver: Arc::clone(&self.driver),
            chunk: Vec::new(),
            offset: 0,
        }))
    }

    fn tune(&self, channel: Channel) -> anyhow::Result<()> {
        let (space, number) = match channel.inner {
            ChannelInner::BonIsdbS { space, channel }
            | ChannelInner::BonIsdbT { space, channel } => (space, channel),
            _ => bail!("A BonDriver tuner only takes channels of the BonDriver kind"),
        };

        let mut driver = lock(&self.driver)?;
        driver.set_channel(space, number)?;

        // The demodulator keeps putting out whatever it had while it was locking.
        // Hand that to a demultiplexer and it will look for packet boundaries in
        // noise, so let it settle and throw those bytes away.
        std::thread::sleep(SETTLE_TIME);
        driver.purge();

        info!(
            "Tuned to space {space} channel {number} ({:.2} dB)",
            driver.signal_level(),
        );

        Ok(())
    }
}

/// The stream a [`BonTuner`] hands out. The driver only lends its buffer until
/// the next call, so each chunk is copied out and then drained.
struct BonInput {
    driver: Arc<Mutex<BonDriver>>,
    chunk: Vec<u8>,
    offset: usize,
}

impl Read for BonInput {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        while self.offset >= self.chunk.len() {
            let mut driver = lock(&self.driver).map_err(io::Error::other)?;
            let Some(chunk) = driver
                .next_chunk(WAIT_TIMEOUT_MS)
                .map_err(io::Error::other)?
            else {
                // Nothing buffered yet. A broadcast does not end, so wait for
                // the next chunk rather than reporting the stream as over.
                continue;
            };

            self.chunk.clear();
            self.chunk.extend_from_slice(chunk);
            self.offset = 0;
        }

        let len = (self.chunk.len() - self.offset).min(buf.len());
        buf[..len].copy_from_slice(&self.chunk[self.offset..self.offset + len]);
        self.offset += len;

        Ok(len)
    }
}

fn lock(driver: &Mutex<BonDriver>) -> anyhow::Result<MutexGuard<'_, BonDriver>> {
    driver
        .lock()
        .map_err(|_| anyhow::anyhow!("The BonDriver lock was poisoned"))
}
