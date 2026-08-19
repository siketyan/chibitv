use std::sync::Arc;

use chibitv_b61::Descrambler;
use chrono::{Local, Offset};
use clap::Parser;
use tracing::warn;

use crate::cas::PcscCasModule;
use crate::channel::{Channel, ChannelInner};
use crate::config::{ChannelConfig, Config};
use crate::event_crawler::EventCrawler;
use crate::registry::Registry;
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

    let registry = Arc::new(Registry::default());
    seed_registry(&registry, &config.channels);

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
    );

    let address = config.server.address;
    let event_crawler = EventCrawler::new(tuners, cas, config.cas.master_key.into());
    let state = Arc::new(
        Workspace::new(registry, channels, Some(streams)).with_event_crawler(event_crawler),
    );

    crate::server::serve(address, state).await
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
