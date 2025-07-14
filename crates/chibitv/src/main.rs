mod channel;
mod config;
mod descrambler;
mod hevc;
mod m2ts;
mod mmt;
mod registry;
mod remux;
mod server;
mod stream;
mod tuner;
mod workspace;

use std::sync::{Arc, RwLock};

use anyhow::bail;
use bpaf::Bpaf;
use chibitv_b61::CasModule;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::filter::LevelFilter;

use crate::channel::Channel;
use crate::config::Config;
use crate::descrambler::Descrambler;
use crate::registry::Registry;
use crate::server::serve;
use crate::stream::{Stream, Streams};
use crate::tuner::Tuners;
use crate::workspace::Workspace;

#[derive(Bpaf, Clone, Debug)]
#[bpaf(options)]
struct Options {
    /// Perform verbose logging
    #[bpaf(short, long)]
    verbose: bool,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let options = options().run();

    let env_filter = EnvFilter::builder()
        .with_default_directive(
            match options.verbose {
                true => LevelFilter::TRACE,
                _ => LevelFilter::INFO,
            }
            .into(),
        )
        .from_env_lossy();

    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(env_filter)
        .init();

    let config = Config::load_from_file("./config.toml")?;
    let cas = CasModule::open()?;
    let descrambler = Descrambler::init(cas, config.cas.master_key.clone().into())?;

    let registry = Arc::new(Registry::default());

    let channels = config
        .channels
        .iter()
        .enumerate()
        .map(|(id, channel)| Channel {
            id,
            name: channel.name.to_string(),
            inner: (&channel.inner).into(),
        })
        .collect::<Vec<_>>();

    let Some(default_channel) = channels.first() else {
        bail!("No channels are defined in the config. At least one channel is required.");
    };

    let tuners = {
        let mut tuners = Tuners::default();

        for (id, tuner) in config.tuners.iter().enumerate() {
            tuners.add_tuner_from_config(id as u32, tuner)?;
        }

        Arc::new(RwLock::new(tuners))
    };

    let streams = {
        let tuners = tuners.read().unwrap();
        let tuner = tuners.get_tuner(0).unwrap();
        let stream = Stream::open(registry.clone(), tuner, descrambler)?;
        let mut streams = Streams::new();

        stream.set_channel(0, default_channel)?;
        stream.run();
        streams.add_stream(0, stream);

        RwLock::new(streams)
    };

    let address = config.server.address;
    let state = Arc::new(Workspace::new(registry, channels, streams));

    serve(address, state).await
}
