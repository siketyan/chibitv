use std::net::SocketAddr;
use std::sync::Arc;

use anyhow::bail;
use clap::Parser;

use crate::config::Config;
use crate::tuner::Tuners;

#[derive(Clone, Debug, Parser)]
pub struct Options {
    /// Address to listen on. Overrides `tuner_server.address` in the config.
    #[clap(short, long)]
    address: Option<SocketAddr>,
}

pub async fn tuner(options: &Options, config: &Config) -> anyhow::Result<()> {
    if config.tuners.is_empty() {
        bail!("No tuners are defined in the config. At least one tuner is required.");
    }

    let tuners = Arc::new(Tuners::from_config(&config.tuners)?);
    let address = options.address.unwrap_or(config.tuner_server.address);

    crate::server::serve_tuner(address, tuners).await
}
