mod live;
mod record;
mod remux;
mod serve;

use clap::Parser;

use crate::config::Config;

#[derive(Clone, Debug, Parser)]
pub(super) enum Command {
    /// Watch a channel as a remuxed M2TS stream written to stdout.
    Live(live::Options),

    /// Record a MMT/TLV stream from a tuner.
    Record(record::Options),

    /// Demux a MMT/TLV stream and mux a M2TS stream.
    Remux(remux::Options),

    /// Run the chibitv server.
    Serve(serve::Options),
}

impl Command {
    pub(crate) async fn run(&self, config: &Config) -> anyhow::Result<()> {
        match self {
            Self::Live(options) => live::live(options, config).await,
            Self::Record(options) => record::record(options, config).await,
            Self::Remux(options) => remux::remux(options, config).await,
            Self::Serve(options) => serve::serve(options, config).await,
        }
    }
}
