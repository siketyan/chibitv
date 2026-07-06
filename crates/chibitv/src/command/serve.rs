use std::sync::{Arc, Mutex, RwLock};

use anyhow::bail;
use clap::Parser;

use crate::channel::Channel;
use crate::config::Config;
use crate::descrambler::{CasModule, Descrambler};
use crate::registry::Registry;
use crate::stream::{Stream, Streams};
use crate::tuner::Tuners;
use crate::workspace::Workspace;

#[derive(Clone, Debug, Parser)]
pub struct Options {}

pub async fn serve(_options: &Options, config: &Config) -> anyhow::Result<()> {
    let cas = Arc::new(Mutex::new(CasModule::open(config.cas.master_key.into())?));
    let descrambler = Descrambler::init(cas, true)?;

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
        streams.add_stream(0, stream);

        RwLock::new(streams)
    };

    let address = config.server.address;
    let state = Arc::new(Workspace::new(registry, channels, streams));

    crate::server::serve(address, state).await
}
