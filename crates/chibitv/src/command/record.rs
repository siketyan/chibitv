use std::fs::File;
use std::io::{BufReader, Read, Write, stdout};

use clap::Parser;
use tracing::info;

use crate::channel::{Channel, ChannelInner};
use crate::config::Config;
use crate::tuner::Tuners;

#[derive(Clone, Debug, Parser)]
pub struct Options {
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

    let Some(tuner) = tuners.get_tuner(0) else {
        anyhow::bail!("No tuners are configured");
    };

    let mut input = BufReader::new(tuner.open()?);
    let mut output: Box<dyn Write> = match options.output.as_deref() {
        Some("-") | None => Box::new(stdout()),
        Some(path) => Box::new(File::create(path)?),
    };

    let Some(channel) = config.channels.get(options.channel).map(|channel| Channel {
        id: options.channel,
        name: channel.name.to_string(),
        inner: (&channel.inner).into(),
    }) else {
        anyhow::bail!("Could not find the channel in the config");
    };

    info!("Tuning to the channel: {:?}", channel);

    tuner.tune(channel)?;

    info!("Starting to record. Press Ctrl+C to stop.");

    std::io::copy(&mut input, &mut output)?;

    Ok(())
}
