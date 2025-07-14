#[cfg(feature = "dvb")]
mod dvb;
mod stdin;

use std::collections::BTreeMap;
use std::io::Read;
use std::sync::Arc;

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

#[derive(Default)]
pub struct Tuners {
    tuners: BTreeMap<u32, Arc<dyn Tuner>>,
}

impl Tuners {
    pub fn get_tuner(&self, id: u32) -> Option<Arc<dyn Tuner>> {
        self.tuners.get(&id).cloned()
    }

    pub fn add_tuner<T: Tuner + 'static>(&mut self, id: u32, tuner: T) {
        self.tuners.insert(id, Arc::new(tuner));
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
