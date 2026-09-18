use std::fs::File;
use std::io::{BufReader, Write, stdout};

use clap::Parser;
use tracing::info;

use crate::channel;
use crate::config::Config;
use crate::tuner::Tuners;

#[derive(Clone, Debug, Parser)]
pub struct Options {
    /// Identifier of the channel to tune to, as `channels` lists it.
    #[clap(short, long)]
    channel: usize,

    /// Destination path of the output stream. Defaults to stdout.
    #[clap(short, long)]
    output: Option<String>,
}

pub async fn record(options: &Options, config: &Config) -> anyhow::Result<()> {
    let mut tuners = Tuners::default();
    for (id, tuner) in config.tuners.iter().enumerate() {
        tuners.add_tuner_from_config(id as u32, tuner)?;
    }

    let tuner = tuners.try_acquire_by_id(0)?;

    let channel = channel::find_channel(config, options.channel).await?;

    info!("Tuning to the channel: {:?}", channel);

    tuner.tune(channel)?;

    let mut input = BufReader::new(tuner.open()?);
    let mut output: Box<dyn Write> = match options.output.as_deref() {
        Some("-") | None => Box::new(stdout()),
        Some(path) => Box::new(File::create(path)?),
    };

    info!("Starting to record. Press Ctrl+C to stop.");

    std::io::copy(&mut input, &mut output)?;

    Ok(())
}
