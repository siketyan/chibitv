use std::sync::Arc;
use std::time::Duration;

use chibitv_b61::Descrambler;
use chrono::{Local, Offset, TimeDelta};
use clap::Parser;
use tracing::{error, info, warn};

use crate::cas::PcscCasModule;
use crate::channel::{Channel, ChannelInner};
use crate::config::{ChannelConfig, Config, DatabaseConfig};
use crate::event_crawler::EventCrawler;
use crate::registry::Registry;
use crate::store::{self, EventStore, EventWriter};
use crate::stream::Streams;
use crate::tuner::Tuners;
use crate::workspace::Workspace;

#[derive(Clone, Debug, Parser)]
pub struct Options {}

/// The offset ARIB SI expresses every date and time in.
const BROADCAST_UTC_OFFSET_SECONDS: i32 = 9 * 60 * 60;

/// How often the programmes that have been broadcast are dropped.
const PRUNE_INTERVAL: Duration = Duration::from_secs(60 * 60);

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

    let registry = Arc::new(Registry::default());
    seed_registry(&registry, &config.channels);

    // The schedule of the previous run is restored before anything is tuned,
    // so the programme guide is there without crawling first.
    let store = open_store(&config.database, &registry).await;
    let events = store
        .as_ref()
        .map(|store| EventWriter::spawn(store.clone()));

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

    let cas = PcscCasModule::open_shared()?;
    let b61_descrambler = if channels
        .iter()
        .any(|channel| matches!(channel.inner, ChannelInner::IsdbS { .. }))
    {
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
        events.clone(),
    );

    let address = config.server.address;
    let event_crawler =
        EventCrawler::new(tuners, cas, config.cas.master_key.into()).storing_events(events);
    let state = Arc::new(
        Workspace::new(registry, channels, Some(streams)).with_event_crawler(event_crawler),
    );

    crate::server::serve(address, state).await
}

/// Opens the store the schedule is kept in and hands the registry what it
/// holds.
///
/// A database that cannot be opened is reported rather than refused over: the
/// schedule is collected from the broadcast anyway, so the server is still
/// worth running without one.
async fn open_store(config: &DatabaseConfig, registry: &Registry) -> Option<Arc<dyn EventStore>> {
    let store = match store::open(&config.url).await {
        Ok(store) => store,
        Err(error) => {
            error!(url = %config.url, %error, "Could not open the database, so the schedule is not kept");
            return None;
        }
    };

    let retention = TimeDelta::days(i64::from(config.retention_days));
    prune_events(&store, retention).await;
    store::restore(&store, registry).await;
    spawn_pruning(Arc::clone(&store), retention);

    Some(store)
}

/// Drops the programmes that finished longer than the retention ago.
async fn prune_events(store: &Arc<dyn EventStore>, retention: TimeDelta) {
    // The SI carries JST wall-clock time and the server runs on that zone.
    let at = Local::now().naive_local() - retention;
    match store.prune_events_before(at).await {
        Ok(0) => {}
        Ok(pruned) => info!(pruned, "Dropped the programmes already broadcast"),
        Err(error) => error!(%error, "Could not drop the programmes already broadcast"),
    }
}

fn spawn_pruning(store: Arc<dyn EventStore>, retention: TimeDelta) {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(PRUNE_INTERVAL);
        // The first tick is immediate and the schedule has just been pruned.
        interval.tick().await;

        loop {
            interval.tick().await;
            prune_events(&store, retention).await;
        }
    });
}

fn seed_registry(registry: &Registry, channels: &[ChannelConfig]) {
    for (channel_id, channel) in channels.iter().enumerate() {
        let Some(transport_stream_id) = channel.transport_stream_id else {
            continue;
        };
        for service in &channel.services {
            registry.put_cached_service(
                channel_id,
                transport_stream_id,
                service.id,
                service.name.clone(),
                service.provider_name.clone(),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::config::{ChannelConfigInner, ServiceConfig};

    use super::*;

    #[test]
    fn seeds_services_from_every_configured_physical_channel() {
        let channels = [
            ChannelConfig {
                name: "UHF 20".to_string(),
                transport_stream_id: Some(100),
                services: vec![ServiceConfig {
                    id: 101,
                    name: "Service A".to_string(),
                    provider_name: "Provider A".to_string(),
                }],
                inner: ChannelConfigInner::IsdbT {
                    frequency: 515_142_857,
                    bandwidth_hz: 6_000_000,
                },
            },
            ChannelConfig {
                name: "UHF 21".to_string(),
                transport_stream_id: Some(200),
                services: vec![ServiceConfig {
                    id: 201,
                    name: "Service B".to_string(),
                    provider_name: "Provider B".to_string(),
                }],
                inner: ChannelConfigInner::IsdbT {
                    frequency: 521_142_857,
                    bandwidth_hz: 6_000_000,
                },
            },
        ];
        let registry = Registry::default();

        seed_registry(&registry, &channels);

        assert_eq!(registry.get_all_services().len(), 2);
        let first = registry.get_service_by_id(101).unwrap();
        assert_eq!(first.channel_id, 0);
        assert_eq!(first.transport_stream_id, 100);
        let second = registry.get_service_by_id(201).unwrap();
        assert_eq!(second.channel_id, 1);
        assert_eq!(second.transport_stream_id, 200);
    }
}
