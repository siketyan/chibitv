use std::io::Read;

use async_trait::async_trait;

use super::Tuner;

pub struct StdinTuner;

#[async_trait]
impl Tuner for StdinTuner {
    async fn open(&self) -> anyhow::Result<Box<dyn Read + Send + Sync>> {
        Ok(Box::new(std::io::stdin()))
    }
}
