use std::fs::File;
use std::io::{BufReader, Write, stdout};

use clap::Parser;
use tracing::info;

use crate::channel;
use crate::config::Config;

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
    let store = crate::store::open(&config.database.url).await?;
    let channel = channel::find_channel(&*store, options.channel).await?;
    let tuner = super::tune(config, &channel)?;

    let mut input = BufReader::new(tuner);
    let mut output: Box<dyn Write> = match options.output.as_deref() {
        Some("-") | None => Box::new(stdout()),
        Some(path) => Box::new(File::create(path)?),
    };

    info!("Starting to record. Press Ctrl+C to stop.");

    std::io::copy(&mut input, &mut output)?;

    Ok(())
}
