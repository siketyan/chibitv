use std::io::Read;

use super::Tuner;

pub struct StdinTuner;

impl Tuner for StdinTuner {
    fn open(&self) -> anyhow::Result<Box<dyn Read + Send + Sync>> {
        Ok(Box::new(std::io::stdin()))
    }
}
