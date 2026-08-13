use std::net::{Ipv6Addr, SocketAddr};
use std::sync::Arc;

use anyhow::{anyhow, bail};
use clap::Parser;
use connectrpc::Router;
use connectrpc::server::Server;

use crate::config::Config;
use crate::rpc::tuner::ChibitvTunerServiceImpl;
use crate::tuner::Tuners;

#[derive(Clone, Debug, Parser)]
pub struct Options {
    /// Address to listen on. Pass a wildcard address such as `[::]:3002` to
    /// accept connections from other hosts.
    #[clap(short, long, default_value_t = SocketAddr::from((Ipv6Addr::LOCALHOST, 3002)))]
    address: SocketAddr,
}

pub async fn tuner(options: &Options, config: &Config) -> anyhow::Result<()> {
    if config.tuners.is_empty() {
        bail!("No tuners are defined in the config. At least one tuner is required.");
    }

    let tuners = Arc::new(Tuners::from_config(&config.tuners)?);

    // Only the tuner service is served here, so it needs no HTTP framework.
    Server::new(ChibitvTunerServiceImpl::new(tuners).register(Router::new()))
        .serve(options.address)
        .await
        .map_err(|error| anyhow!("Could not serve the tuner: {error}"))
}
