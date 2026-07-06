mod record;
mod remux;
mod serve;

use clap::Parser;

use crate::config::Config;

#[derive(Clone, Debug, Parser)]
pub(super) enum Command {
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
            Self::Record(options) => record::record(options, config).await,
            Self::Remux(options) => remux::remux(options, config).await,
            Self::Serve(options) => serve::serve(options, config).await,
        }
    }
}
