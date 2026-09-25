use std::io::{BufReader, BufWriter, stdout};

use chibitv_b25::B25Descrambler;
use chibitv_b61::Descrambler;
use clap::Parser;
use mpeg2ts::ts::TsPacketWriter;
use tracing::info;

use crate::cas::SharedCasModule;
use crate::channel::{self, ChannelInner};
use crate::config::Config;
use crate::demux::Demux;
use crate::m2ts::{M2tsDemuxer, M2tsMuxer};
use crate::mmt::MmtDemuxer;
use crate::remux::{Mux, Remuxer};
use crate::service_information::{ServiceInformationProcessor, Signal};
use crate::tuner::Tuners;

#[derive(Clone, Debug, Parser)]
pub struct Options {
    /// Identifier of the channel to tune to, as `channels` lists it.
    #[clap(short, long)]
    channel: usize,
}

pub async fn live(options: &Options, config: &Config) -> anyhow::Result<()> {
    let mut tuners = Tuners::default();
    for (id, tuner) in config.tuners.iter().enumerate() {
        tuners.add_tuner_from_config(id as u32, tuner)?;
    }

    let store = crate::store::open(&config.database.url).await?;
    let channel = channel::find_channel(&*store, options.channel).await?;
    let tuner = tuners.try_acquire(channel.inner.delivery_system())?;

    info!("Tuning to the channel: {:?}", channel);

    tuner.tune(channel.clone())?;

    info!("Starting live stream. Press Ctrl+C to stop.");

    let input = tuner.open()?;
    let output = stdout();
    let writer = TsPacketWriter::new(BufWriter::new(output));
    let mux = M2tsMuxer::new(writer);
    let cas = SharedCasModule::open()?;

    let (signal_tx, mut signal_rx) = tokio::sync::broadcast::channel::<Signal>(1);

    tokio::spawn(async move {
        loop {
            let Ok(signal) = signal_rx.recv().await else {
                continue;
            };

            match signal {
                Signal::EventChanged { event_id, .. } => {
                    info!(event_id, "Event changed");
                }
            }
        }
    });

    let service_information = ServiceInformationProcessor::new(None, Some(signal_tx));
    match channel.inner {
        ChannelInner::IsdbS3 { .. } | ChannelInner::BonIsdbS3 { .. } => {
            let descrambler = Descrambler::init(cas, config.cas.master_key.into(), true)?;
            let demux = MmtDemuxer::new(BufReader::new(input), descrambler);
            run_live_remuxer(Remuxer::new(demux, mux)?, service_information)
        }
        ChannelInner::IsdbT { .. }
        | ChannelInner::IsdbS { .. }
        | ChannelInner::BonIsdbT { .. }
        | ChannelInner::BonIsdbS { .. } => {
            let descrambler = B25Descrambler::init(cas, true)?;
            let demux = M2tsDemuxer::new(input, descrambler);
            run_live_remuxer(Remuxer::new(demux, mux)?, service_information)
        }
    }
}

fn run_live_remuxer<D: Demux, M: Mux>(
    mut remuxer: Remuxer<D, M>,
    mut service_information: ServiceInformationProcessor,
) -> anyhow::Result<()> {
    while let Some(signaling) = remuxer.next()? {
        service_information.process(signaling)?;
    }
    remuxer.finish()
}
