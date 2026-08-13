use std::io::Read;

use super::{AnyTuner, Tuner};

pub struct StdinTuner;

impl Tuner for StdinTuner {
    async fn open(&self) -> anyhow::Result<Box<dyn Read + Send + Sync>> {
        Ok(Box::new(std::io::stdin()))
    }
}

impl From<StdinTuner> for AnyTuner {
    fn from(value: StdinTuner) -> Self {
        Self::Stdin(value)
    }
}
