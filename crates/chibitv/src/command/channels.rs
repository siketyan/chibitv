use clap::Parser;

use crate::channel::{format_channel_list, load_channels};
use crate::config::Config;
use crate::store;

#[derive(Clone, Debug, Parser)]
pub struct Options {}

/// Prints the channels the database keeps, which is where the `--channel`
/// option of the other commands reads its identifiers from.
pub async fn channels(_options: &Options, config: &Config) -> anyhow::Result<()> {
    let store = store::open(&config.database.url).await?;
    let channels = load_channels(&store, &config.channels).await?;

    print!("{}", format_channel_list(&channels));

    Ok(())
}
