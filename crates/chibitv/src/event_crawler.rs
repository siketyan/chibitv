use std::io::BufReader;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tracing::{info, warn};

use chibitv_b10::table::Table as B10Table;
use chibitv_b25::B25Descrambler;
use chibitv_b60::message::Message;
use chibitv_b60::table::Table as B60Table;
use chibitv_b61::Descrambler;

use crate::cas::PcscCasModule;
use crate::channel::{Channel, ChannelInner};
use crate::demux::{Demux, Packet, SignalingEvent};
use crate::m2ts::M2tsDemuxer;
use crate::mmt::MmtDemuxer;
use crate::registry::{Event, Registry};
use crate::service_information::ServiceInformationProcessor;
use crate::tuner::Tuners;

const READ_BUFFER_SIZE: usize = 188 * 8192;
const EIT_ACTUAL_PRESENT_FOLLOWING_TABLE_ID: u8 = 0x4E;
const EIT_ACTUAL_SCHEDULE_TABLE_IDS: std::ops::RangeInclusive<u8> = 0x50..=0x5F;

pub struct CrawledEvent {
    pub service_id: u16,
    pub event: Event,
}

pub struct EventCrawler {
    tuners: Arc<Tuners>,
    cas: Arc<PcscCasModule>,
    cas_master_key: [u8; 32],
}

impl EventCrawler {
    pub fn new(tuners: Arc<Tuners>, cas: Arc<PcscCasModule>, cas_master_key: [u8; 32]) -> Self {
        Self {
            tuners,
            cas,
            cas_master_key,
        }
    }

    pub fn crawl(
        &self,
        channels: &[Channel],
        registry: Arc<Registry>,
        dwell_time: Duration,
        mut emit: impl FnMut(CrawledEvent) -> bool,
    ) -> anyhow::Result<()> {
        let tuner = self.tuners.try_acquire()?;
        info!(tuner_id = tuner.id(), "Acquired tuner for event crawling");

        for channel in channels {
            info!(channel_id = channel.id, channel = %channel.name, "Crawling events");
            if let Err(error) = tuner.tune(channel.clone()) {
                warn!(channel_id = channel.id, %error, "Could not tune while crawling events");
                continue;
            }

            let reader = match tuner.open_reader() {
                Ok(reader) => reader,
                Err(error) => {
                    warn!(channel_id = channel.id, %error, "Could not open tuner input");
                    continue;
                }
            };
            let deadline = Instant::now() + dwell_time;
            let keep_crawling = match channel.inner {
                ChannelInner::IsdbT { .. } => {
                    let descrambler = B25Descrambler::init(self.cas.clone())?;
                    let mut demux = M2tsDemuxer::new(reader, descrambler);
                    crawl_channel(&mut demux, channel, &registry, deadline, &mut emit)?
                }
                ChannelInner::IsdbS { .. } => {
                    let descrambler =
                        Descrambler::init(self.cas.clone(), self.cas_master_key, false)?;
                    let mut demux = MmtDemuxer::new(
                        BufReader::with_capacity(READ_BUFFER_SIZE, reader),
                        descrambler,
                    );
                    crawl_channel(&mut demux, channel, &registry, deadline, &mut emit)?
                }
            };

            if !keep_crawling {
                break;
            }
        }

        Ok(())
    }
}

fn crawl_channel<D: Demux>(
    demux: &mut D,
    channel: &Channel,
    registry: &Arc<Registry>,
    deadline: Instant,
    emit: &mut impl FnMut(CrawledEvent) -> bool,
) -> anyhow::Result<bool> {
    let mut processor =
        ServiceInformationProcessor::new(channel.id, Some(Arc::clone(registry)), None);

    while Instant::now() < deadline {
        let packet = match demux.next_packet() {
            Ok(Some(packet)) => packet,
            Ok(None) => break,
            Err(error) => {
                warn!(channel_id = channel.id, %error, "Could not read event information");
                continue;
            }
        };
        let Packet::Signaling(signaling) = packet else {
            continue;
        };

        let event_ids = signaling_event_ids(&signaling);
        processor.process(signaling)?;

        let Some((service_id, event_ids)) = event_ids else {
            continue;
        };
        let Some(service) = registry.get_service_by_id(service_id) else {
            continue;
        };
        if service.channel_id != channel.id {
            continue;
        }

        for event_id in event_ids {
            let Some(event) = registry.get_event_by_id(service_id, event_id) else {
                continue;
            };
            if !emit(CrawledEvent { service_id, event }) {
                return Ok(false);
            }
        }
    }

    Ok(true)
}

fn signaling_event_ids(signaling: &SignalingEvent) -> Option<(u16, Vec<u16>)> {
    match signaling {
        SignalingEvent::B10Table {
            table_id,
            table: B10Table::Eit(table),
        } if *table_id == EIT_ACTUAL_PRESENT_FOLLOWING_TABLE_ID
            || EIT_ACTUAL_SCHEDULE_TABLE_IDS.contains(table_id) =>
        {
            Some((
                table.service_id,
                table.events.iter().map(|event| event.event_id).collect(),
            ))
        }
        SignalingEvent::B60Message(Message::M2Section(message)) => match &message.table {
            B60Table::MhEit(table) => Some((
                table.service_id,
                table.events.iter().map(|event| event.event_id).collect(),
            )),
            _ => None,
        },
        _ => None,
    }
}
