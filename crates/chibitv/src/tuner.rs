#[cfg(all(feature = "bon", windows))]
mod bon;
#[cfg(all(feature = "dvb", target_os = "linux"))]
mod dvb;
#[cfg(all(feature = "px4", target_os = "linux"))]
mod px4;
mod stdin;

use std::collections::{BTreeMap, BTreeSet};
use std::io::Read;
use std::sync::Arc;

use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tracing::warn;

use crate::channel::{Channel, DeliverySystem};
use crate::config::{TunerConfig, TunerKind};

pub trait Tuner: Send + Sync {
    fn open(&self) -> anyhow::Result<Box<dyn Read + Send + Sync>>;

    fn tune(&self, _channel: Channel) -> anyhow::Result<()> {
        warn!("This tuner does not support tuning.");
        Ok(())
    }

    /// Called once the lease on the tuner is given back, for a tuner that has
    /// something to let go of between uses, such as a device other programs
    /// could use meanwhile.
    fn close(&self) {}
}

struct TunerSlot {
    id: u32,
    tuner: Arc<dyn Tuner>,
    /// The broadcasts the tuner receives, which it is picked for.
    delivery_systems: BTreeSet<DeliverySystem>,
    semaphore: Arc<Semaphore>,
}

#[derive(Debug)]
pub enum AcquireError {
    /// No tuners are defined in the configuration.
    NotConfigured,
    /// No configured tuner receives the broadcast.
    Unsupported(DeliverySystem),
    /// Every configured tuner receiving the broadcast is currently leased.
    Busy,
}

impl std::fmt::Display for AcquireError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotConfigured => write!(f, "No tuners are configured"),
            Self::Unsupported(system) => write!(f, "No tuner receives {system}"),
            Self::Busy => write!(f, "All tuners are in use"),
        }
    }
}

impl std::error::Error for AcquireError {}

pub struct TunerLease {
    slot: Arc<TunerSlot>,
    _permit: OwnedSemaphorePermit,
}

impl Drop for TunerLease {
    fn drop(&mut self) {
        // The permit is a field, so it is only released after this.
        self.slot.tuner.close();
    }
}

impl TunerLease {
    pub fn id(&self) -> u32 {
        self.slot.id
    }

    pub fn tune(&self, channel: Channel) -> anyhow::Result<()> {
        self.slot.tuner.tune(channel)
    }

    pub fn open(self) -> anyhow::Result<TunerInput> {
        let reader = self.open_reader()?;
        Ok(TunerInput {
            reader,
            _lease: self,
        })
    }

    pub(crate) fn open_reader(&self) -> anyhow::Result<Box<dyn Read + Send + Sync>> {
        self.slot.tuner.open()
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
    /// Takes a free tuner receiving the broadcast, the first one configured
    /// that is.
    pub fn try_acquire(&self, delivery_system: DeliverySystem) -> Result<TunerLease, AcquireError> {
        if self.tuners.is_empty() {
            return Err(AcquireError::NotConfigured);
        }

        let mut supported = false;
        for slot in self.tuners.values() {
            if !slot.delivery_systems.contains(&delivery_system) {
                continue;
            }
            supported = true;

            if let Ok(permit) = Arc::clone(&slot.semaphore).try_acquire_owned() {
                return Ok(TunerLease {
                    slot: Arc::clone(slot),
                    _permit: permit,
                });
            }
        }

        if supported {
            Err(AcquireError::Busy)
        } else {
            Err(AcquireError::Unsupported(delivery_system))
        }
    }

    #[cfg(test)]
    pub fn is_in_use(&self, id: u32) -> Option<bool> {
        self.tuners
            .get(&id)
            .map(|slot| slot.semaphore.available_permits() == 0)
    }

    /// Adds a tuner, to be picked for the broadcasts it receives.
    pub fn add_tuner<T: Tuner + 'static>(
        &mut self,
        id: u32,
        tuner: T,
        delivery_systems: impl IntoIterator<Item = DeliverySystem>,
    ) {
        self.tuners.insert(
            id,
            Arc::new(TunerSlot {
                id,
                tuner: Arc::new(tuner),
                delivery_systems: delivery_systems.into_iter().collect(),
                semaphore: Arc::new(Semaphore::new(1)),
            }),
        );
    }

    pub fn add_tuner_from_config(&mut self, id: u32, config: &TunerConfig) -> anyhow::Result<()> {
        let systems = config.delivery_systems();
        match &config.kind {
            TunerKind::Stdin => {
                self.add_tuner(id, stdin::StdinTuner, systems);
            }

            #[cfg(all(feature = "dvb", target_os = "linux"))]
            TunerKind::Dvb {
                adapter_num,
                frontend_num,
            } => {
                self.add_tuner(
                    id,
                    dvb::DvbTuner::new(*adapter_num, *frontend_num)?,
                    systems,
                );
            }

            #[cfg(all(feature = "bon", windows))]
            TunerKind::Bon { path } => {
                self.add_tuner(id, bon::BonTuner::new(path)?, systems);
            }

            #[cfg(all(feature = "px4", target_os = "linux"))]
            TunerKind::Px4 { path, lnb_voltage } => {
                self.add_tuner(id, px4::Px4Tuner::new(path, *lnb_voltage)?, systems);
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
        tuners.add_tuner(7, FakeTuner, DeliverySystem::ALL);
        assert_eq!(tuners.is_in_use(7), Some(false));

        let lease = tuners.try_acquire(DeliverySystem::IsdbT).unwrap();
        assert_eq!(lease.id(), 7);
        assert_eq!(tuners.is_in_use(7), Some(true));
        assert!(matches!(
            tuners.try_acquire(DeliverySystem::IsdbT),
            Err(AcquireError::Busy)
        ));

        let input = lease.open().unwrap();
        assert_eq!(tuners.is_in_use(7), Some(true));

        drop(input);
        assert_eq!(tuners.is_in_use(7), Some(false));
        assert!(tuners.try_acquire(DeliverySystem::IsdbT).is_ok());
    }

    #[test]
    fn keeps_tuner_locked_while_reopening_inputs_from_a_lease() {
        let mut tuners = Tuners::default();
        tuners.add_tuner(7, FakeTuner, DeliverySystem::ALL);

        let lease = tuners.try_acquire(DeliverySystem::IsdbT).unwrap();
        let input = lease.open_reader().unwrap();
        assert_eq!(tuners.is_in_use(7), Some(true));

        drop(input);
        assert_eq!(tuners.is_in_use(7), Some(true));
        assert!(lease.open_reader().is_ok());

        drop(lease);
        assert_eq!(tuners.is_in_use(7), Some(false));
    }

    #[test]
    fn acquires_another_available_tuner() {
        let mut tuners = Tuners::default();
        tuners.add_tuner(0, FakeTuner, DeliverySystem::ALL);
        tuners.add_tuner(1, FakeTuner, DeliverySystem::ALL);

        let first = tuners.try_acquire(DeliverySystem::IsdbT).unwrap();
        let second = tuners.try_acquire(DeliverySystem::IsdbT).unwrap();

        assert_eq!(first.id(), 0);
        assert_eq!(second.id(), 1);
    }

    #[test]
    fn acquires_a_tuner_receiving_the_broadcast() {
        let mut tuners = Tuners::default();
        tuners.add_tuner(0, FakeTuner, [DeliverySystem::IsdbT, DeliverySystem::IsdbS]);
        tuners.add_tuner(1, FakeTuner, [DeliverySystem::IsdbS3]);

        // The 4K tuner is passed over for a broadcast the first one receives.
        let terrestrial = tuners.try_acquire(DeliverySystem::IsdbT).unwrap();
        assert_eq!(terrestrial.id(), 0);
        assert!(matches!(
            tuners.try_acquire(DeliverySystem::IsdbS),
            Err(AcquireError::Busy)
        ));

        let satellite_4k = tuners.try_acquire(DeliverySystem::IsdbS3).unwrap();
        assert_eq!(satellite_4k.id(), 1);

        drop(satellite_4k);
        tuners.add_tuner(2, FakeTuner, [DeliverySystem::IsdbT]);
        assert!(matches!(
            tuners.try_acquire(DeliverySystem::IsdbS),
            Err(AcquireError::Busy)
        ));
        assert_eq!(tuners.try_acquire(DeliverySystem::IsdbT).unwrap().id(), 2);
    }

    #[test]
    fn tells_a_broadcast_no_tuner_receives_from_a_busy_one() {
        let mut tuners = Tuners::default();
        assert!(matches!(
            tuners.try_acquire(DeliverySystem::IsdbS3),
            Err(AcquireError::NotConfigured)
        ));

        tuners.add_tuner(0, FakeTuner, [DeliverySystem::IsdbT]);
        assert!(matches!(
            tuners.try_acquire(DeliverySystem::IsdbS3),
            Err(AcquireError::Unsupported(DeliverySystem::IsdbS3))
        ));
        assert_eq!(
            AcquireError::Unsupported(DeliverySystem::IsdbS3).to_string(),
            "No tuner receives ISDB-S3"
        );
    }

    #[test]
    fn releases_tuner_when_open_fails() {
        let mut tuners = Tuners::default();
        tuners.add_tuner(0, FailingTuner, DeliverySystem::ALL);

        assert!(
            tuners
                .try_acquire(DeliverySystem::IsdbT)
                .unwrap()
                .open()
                .is_err()
        );
        assert_eq!(tuners.is_in_use(0), Some(false));
    }
}
