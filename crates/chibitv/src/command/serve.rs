use std::sync::Arc;

use anyhow::Context;
use chibitv_b61::Descrambler;
use chrono::{Local, Offset};
use clap::Parser;
use tracing::warn;

use crate::cas::PcscCasModule;
use crate::channel::{Channel, ChannelInner};
use crate::channel_scanner::ChannelScanner;
use crate::config::Config;
use crate::event_crawler::EventCrawler;
use crate::recorder::Recorder;
use crate::registry::Registry;
use crate::storage;
use crate::store::{self, EventWriter};
use crate::stream::Streams;
use crate::tuner::Tuners;
use crate::workspace::Workspace;

#[derive(Clone, Debug, Parser)]
pub struct Options {}

/// The offset ARIB SI expresses every date and time in.
const BROADCAST_UTC_OFFSET_SECONDS: i32 = 9 * 60 * 60;

/// Warns when the clock of the server disagrees with the one the broadcast
/// schedules against, which leaves the programme on air unrecognised.
fn warn_unless_broadcast_time_zone() {
    let offset = Local::now().offset().fix().local_minus_utc();
    if offset != BROADCAST_UTC_OFFSET_SECONDS {
        warn!(
            offset,
            "The server does not run on JST, so the programme on air cannot be told apart; set TZ=JST-9"
        );
    }
}

pub async fn serve(_options: &Options, config: &Config) -> anyhow::Result<()> {
    warn_unless_broadcast_time_zone();

    let store = store::open(&config.database.url)
        .await
        .with_context(|| format!("Could not open the database at `{}`", config.database.url))?;

    let registry =
        Arc::new(Registry::default().storing_events(EventWriter::spawn(Arc::clone(&store))));

    // The channels are the database's, which a scan writes: nothing is served
    // until one has found something.
    let stored_channels = store.load_channels().await?;
    if stored_channels.is_empty() {
        warn!(
            "No channel is stored yet, so there is nothing to watch; scan for the channels on air with `chibitv scan` or from the app"
        );
    }

    registry.put_channels(&stored_channels);

    // The schedule of the previous run is restored before anything is tuned,
    // so the programme guide is there without crawling first.
    registry.restore_events(&store).await?;

    let channels = stored_channels
        .iter()
        .map(Channel::from)
        .collect::<Vec<_>>();

    let cas = PcscCasModule::open_shared()?;
    let b61_descrambler = if channels.iter().any(|channel| {
        matches!(
            channel.inner,
            ChannelInner::IsdbS3 { .. } | ChannelInner::BonIsdbS3 { .. }
        )
    }) {
        Some(Descrambler::init(
            cas.clone(),
            config.cas.master_key.into(),
            true,
        )?)
    } else {
        None
    };

    let tuners = Arc::new({
        let mut tuners = Tuners::default();

        for (id, tuner) in config.tuners.iter().enumerate() {
            tuners.add_tuner_from_config(id as u32, tuner)?;
        }

        tuners
    });

    // No channel is tuned yet: a tuner is occupied only while at least one
    // client keeps a stream open.
    let streams = Streams::new(
        registry.clone(),
        Arc::clone(&tuners),
        cas.clone(),
        b61_descrambler,
    );

    let address = config.server.address;
    let event_crawler = EventCrawler::new(
        Arc::clone(&tuners),
        cas.clone(),
        config.cas.master_key.into(),
    );
    let channel_scanner = ChannelScanner::new(
        Arc::clone(&tuners),
        cas.clone(),
        config.cas.master_key.into(),
    );
    let recorder = Recorder::new(
        tuners,
        cas,
        config.cas.master_key.into(),
        storage::open(&config.storage)?.into(),
    );
    let state = Arc::new(
        Workspace::new(registry, channels, Some(streams))
            .with_channel_store(Arc::clone(&store))
            .with_event_crawler(event_crawler)
            .with_channel_scanner(channel_scanner)
            .with_recorder(recorder),
    );

    crate::server::serve(address, state).await
}
