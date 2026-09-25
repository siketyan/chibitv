mod channels;
mod live;
mod record;
mod remux;
mod scan;
mod serve;
mod status;

use clap::Parser;
use tracing::info;

use crate::channel::Channel;
use crate::config::Config;
use crate::tuner::{TunerLease, Tuners};

#[derive(Clone, Debug, Parser)]
pub(super) enum Command {
    /// List the channels the database keeps.
    Channels(channels::Options),

    /// Watch a channel as a remuxed M2TS stream written to stdout.
    Live(live::Options),

    /// Record a MMT/TLV stream from a tuner.
    Record(record::Options),

    /// Demux a MMT/TLV stream and mux a M2TS stream.
    Remux(remux::Options),

    /// Scan physical channels and keep what was found in the database.
    Scan(scan::Options),

    /// Run the chibitv server.
    Serve(serve::Options),

    /// Show current broadcast status from B10 SI tables.
    Status(status::Options),
}

impl Command {
    pub(crate) async fn run(&self, config: &Config) -> anyhow::Result<()> {
        match self {
            Self::Channels(options) => channels::channels(options, config).await,
            Self::Live(options) => live::live(options, config).await,
            Self::Record(options) => record::record(options, config).await,
            Self::Remux(options) => remux::remux(options, config).await,
            Self::Scan(options) => scan::scan(options, config).await,
            Self::Serve(options) => serve::serve(options, config).await,
            Self::Status(options) => status::status(options, config).await,
        }
    }
}

/// Tunes a tuner that receives the channel, for a command that has the tuners
/// to itself.
fn tune(config: &Config, channel: &Channel) -> anyhow::Result<TunerLease> {
    let tuner =
        Tuners::from_config(&config.tuners)?.try_acquire(channel.inner.delivery_system())?;

    info!("Tuning to the channel: {:?}", channel);

    tuner.tune(channel.clone())?;

    Ok(tuner)
}
