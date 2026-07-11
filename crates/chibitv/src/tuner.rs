#[cfg(feature = "dvb")]
mod dvb;
mod stdin;

use std::collections::BTreeMap;
use std::io::Read;
use std::sync::Arc;

use anyhow::bail;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tracing::warn;

use crate::channel::Channel;
use crate::config::TunerConfig;

pub trait Tuner: Send + Sync {
    fn open(&self) -> anyhow::Result<Box<dyn Read + Send + Sync>>;

    fn tune(&self, _channel: Channel) -> anyhow::Result<()> {
        warn!("This tuner does not support tuning.");
        Ok(())
    }
}

struct TunerSlot {
    id: u32,
    tuner: Arc<dyn Tuner>,
    semaphore: Arc<Semaphore>,
}

pub struct TunerLease {
    slot: Arc<TunerSlot>,
    _permit: OwnedSemaphorePermit,
}

impl TunerLease {
    pub fn id(&self) -> u32 {
        self.slot.id
    }

    pub fn tune(&self, channel: Channel) -> anyhow::Result<()> {
        self.slot.tuner.tune(channel)
    }

    pub fn open(self) -> anyhow::Result<TunerInput> {
        let reader = self.slot.tuner.open()?;
        Ok(TunerInput {
            reader,
            _lease: self,
        })
    }
}

pub struct TunerInput {
    // Keep the reader before the lease so the device is closed before the tuner is released.
    reader: Box<dyn Read + Send + Sync>,
    _lease: TunerLease,
}

impl Read for TunerInput {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.reader.read(buf)
    }
}

#[derive(Default)]
pub struct Tuners {
    tuners: BTreeMap<u32, Arc<TunerSlot>>,
}

impl Tuners {
    pub fn try_acquire(&self) -> anyhow::Result<TunerLease> {
        if self.tuners.is_empty() {
            bail!("No tuners are configured");
        }

        for slot in self.tuners.values() {
            if let Ok(permit) = Arc::clone(&slot.semaphore).try_acquire_owned() {
                return Ok(TunerLease {
                    slot: Arc::clone(slot),
                    _permit: permit,
                });
            }
        }

        bail!("All tuners are in use")
    }

    pub fn try_acquire_by_id(&self, id: u32) -> anyhow::Result<TunerLease> {
        let Some(slot) = self.tuners.get(&id) else {
            bail!("Tuner {id} is not configured");
        };
        let permit = Arc::clone(&slot.semaphore)
            .try_acquire_owned()
            .map_err(|_| anyhow::anyhow!("Tuner {id} is in use"))?;

        Ok(TunerLease {
            slot: Arc::clone(slot),
            _permit: permit,
        })
    }

    pub fn is_in_use(&self, id: u32) -> Option<bool> {
        self.tuners
            .get(&id)
            .map(|slot| slot.semaphore.available_permits() == 0)
    }

    pub fn add_tuner<T: Tuner + 'static>(&mut self, id: u32, tuner: T) {
        self.tuners.insert(
            id,
            Arc::new(TunerSlot {
                id,
                tuner: Arc::new(tuner),
                semaphore: Arc::new(Semaphore::new(1)),
            }),
        );
    }

    pub fn add_tuner_from_config(&mut self, id: u32, config: &TunerConfig) -> anyhow::Result<()> {
        match config {
            TunerConfig::Stdin => {
                self.add_tuner(id, stdin::StdinTuner);
            }

            #[cfg(feature = "dvb")]
            TunerConfig::Dvb {
                adapter_num,
                frontend_num,
            } => {
                self.add_tuner(id, dvb::DvbTuner::new(*adapter_num, *frontend_num)?);
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::*;

    struct FakeTuner;

    impl Tuner for FakeTuner {
        fn open(&self) -> anyhow::Result<Box<dyn Read + Send + Sync>> {
            Ok(Box::new(Cursor::new(vec![1, 2, 3])))
        }
    }

    struct FailingTuner;

    impl Tuner for FailingTuner {
        fn open(&self) -> anyhow::Result<Box<dyn Read + Send + Sync>> {
            anyhow::bail!("Could not open tuner")
        }
    }

    #[test]
    fn keeps_tuner_locked_for_the_input_lifetime() {
        let mut tuners = Tuners::default();
        tuners.add_tuner(7, FakeTuner);
        assert_eq!(tuners.is_in_use(7), Some(false));

        let lease = tuners.try_acquire_by_id(7).unwrap();
        assert_eq!(lease.id(), 7);
        assert_eq!(tuners.is_in_use(7), Some(true));
        assert!(tuners.try_acquire_by_id(7).is_err());

        let input = lease.open().unwrap();
        assert_eq!(tuners.is_in_use(7), Some(true));

        drop(input);
        assert_eq!(tuners.is_in_use(7), Some(false));
        assert!(tuners.try_acquire_by_id(7).is_ok());
    }

    #[test]
    fn acquires_another_available_tuner() {
        let mut tuners = Tuners::default();
        tuners.add_tuner(0, FakeTuner);
        tuners.add_tuner(1, FakeTuner);

        let first = tuners.try_acquire_by_id(0).unwrap();
        let second = tuners.try_acquire().unwrap();

        assert_eq!(first.id(), 0);
        assert_eq!(second.id(), 1);
    }

    #[test]
    fn releases_tuner_when_open_fails() {
        let mut tuners = Tuners::default();
        tuners.add_tuner(0, FailingTuner);

        assert!(tuners.try_acquire_by_id(0).unwrap().open().is_err());
        assert_eq!(tuners.is_in_use(0), Some(false));
    }
}
