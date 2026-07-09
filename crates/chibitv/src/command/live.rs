use std::io::{BufReader, BufWriter, stdout};
use std::sync::{Arc, Mutex};

use chibitv_b25::B25Descrambler;
use chibitv_b61::{B61CasModule, Descrambler};
use clap::Parser;
use mpeg2ts::ts::TsPacketWriter;
use tracing::info;

use crate::channel::{Channel, ChannelInner};
use crate::config::Config;
use crate::m2ts::{M2tsDemuxer, M2tsMuxer};
use crate::mmt::MmtDemuxer;
use crate::remux::{Remux, Remuxer};
use crate::tuner::Tuners;

#[derive(Clone, Debug, Parser)]
pub struct Options {
    #[clap(short, long)]
    channel: usize,
}

pub async fn live(options: &Options, config: &Config) -> anyhow::Result<()> {
    let mut tuners = Tuners::default();
    for (id, tuner) in config.tuners.iter().enumerate() {
        tuners.add_tuner_from_config(id as u32, tuner)?;
    }

    let Some(tuner) = tuners.get_tuner(0) else {
        anyhow::bail!("No tuners are configured");
    };

    let Some(channel) = config.channels.get(options.channel).map(|channel| Channel {
        id: options.channel,
        name: channel.name.to_string(),
        inner: (&channel.inner).into(),
    }) else {
        anyhow::bail!("Could not find the channel in the config");
    };

    info!("Tuning to the channel: {:?}", channel);

    tuner.tune(channel.clone())?;

    info!("Starting live stream. Press Ctrl+C to stop.");

    let input = tuner.open()?;
    let output = stdout();
    let writer = TsPacketWriter::new(BufWriter::new(output));
    let mux = M2tsMuxer::new(writer);

    match channel.inner {
        ChannelInner::IsdbS { .. } => {
            let cas_module = Arc::new(Mutex::new(B61CasModule::open(
                config.cas.master_key.into(),
            )?));
            let descrambler = Descrambler::init(cas_module, false)?;
            let demux = MmtDemuxer::new(BufReader::new(input), descrambler);
            let mut remux = Remuxer::new(demux, mux, None, None);

            remux.run(None)
        }
        ChannelInner::IsdbT { .. } => {
            let descrambler = B25Descrambler::open()?;
            let demux = M2tsDemuxer::new(input, descrambler);
            let mut remux = Remuxer::new(demux, mux, None, None);

            remux.run(None)
        }
    }
}
