use std::collections::BTreeMap;
use std::io::BufReader;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tracing::{info, warn};

use chibitv_b25::B25Descrambler;
use chibitv_b61::Descrambler;

use crate::cas::SharedCasModule;
use crate::channel::{Channel, ChannelInner, DeliverySystem};
use crate::demux::{Demux, Packet, is_descrambling_refused};
use crate::m2ts::M2tsDemuxer;
use crate::mmt::MmtDemuxer;
use crate::service_information::{ServiceInformationProcessor, ServiceInformationWriter};
use crate::task::TaskHandle;
use crate::tuner::{AcquireError, Tuners};

const READ_BUFFER_SIZE: usize = 188 * 8192;

pub struct EventCrawler {
    tuners: Arc<Tuners>,
    cas: Arc<SharedCasModule>,
    cas_master_key: [u8; 32],
    writer: ServiceInformationWriter,
}

impl EventCrawler {
    pub fn new(
        tuners: Arc<Tuners>,
        cas: Arc<SharedCasModule>,
        cas_master_key: [u8; 32],
        writer: ServiceInformationWriter,
    ) -> Self {
        Self {
            tuners,
            cas,
            cas_master_key,
            writer,
        }
    }

    /// Tunes every channel in turn and stores the services and the events it
    /// announces.
    ///
    /// The channels are walked a broadcast at a time, on a tuner receiving
    /// it, which is held for the whole of them; a broadcast no tuner receives
    /// is skipped with a warning. The task is asked to stop between packets,
    /// so cancelling it keeps the events collected so far and gives the tuner
    /// back at once.
    pub fn crawl(
        &self,
        channels: &[Channel],
        dwell_time: Duration,
        task: &TaskHandle,
    ) -> anyhow::Result<()> {
        let mut by_system: BTreeMap<DeliverySystem, Vec<&Channel>> = BTreeMap::new();
        for channel in channels {
            by_system
                .entry(channel.inner.delivery_system())
                .or_default()
                .push(channel);
        }

        let mut index = 0;
        for (system, channels_of_system) in by_system {
            if task.is_cancelled() {
                break;
            }

            let tuner = match self.tuners.try_acquire(system) {
                Ok(tuner) => tuner,
                Err(error @ AcquireError::Unsupported(_)) => {
                    warn!(%system, %error, "Skipping the channels of a broadcast no tuner receives");
                    index += channels_of_system.len();
                    continue;
                }
                Err(error) => return Err(error.into()),
            };
            info!(tuner_id = tuner.id(), %system, "Acquired tuner for event crawling");

            for channel in channels_of_system {
                if task.is_cancelled() {
                    break;
                }

                info!(channel_id = channel.id, channel = %channel.name, "Crawling events");
                task.report(
                    Some(index as f32 / channels.len() as f32),
                    format!("Crawling {}", channel.name),
                );
                index += 1;

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
                match channel.inner {
                    ChannelInner::IsdbT { .. }
                    | ChannelInner::IsdbS { .. }
                    | ChannelInner::BonIsdbT { .. }
                    | ChannelInner::BonIsdbS { .. } => {
                        let descrambler = B25Descrambler::init(self.cas.clone(), true)?;
                        let mut demux = M2tsDemuxer::new(reader, descrambler);
                        crawl_channel(&mut demux, channel, &self.writer, deadline, task)?;
                    }
                    ChannelInner::IsdbS3 { .. } | ChannelInner::BonIsdbS3 { .. } => {
                        let descrambler =
                            Descrambler::init(self.cas.clone(), self.cas_master_key, true)?;
                        let mut demux = MmtDemuxer::new(
                            BufReader::with_capacity(READ_BUFFER_SIZE, reader),
                            descrambler,
                        );
                        crawl_channel(&mut demux, channel, &self.writer, deadline, task)?;
                    }
                }
            }
        }

        Ok(())
    }
}

fn crawl_channel<D: Demux>(
    demux: &mut D,
    channel: &Channel,
    writer: &ServiceInformationWriter,
    deadline: Instant,
    task: &TaskHandle,
) -> anyhow::Result<()> {
    let mut processor = ServiceInformationProcessor::new(Some(writer.clone()), None);
    let mut refused = false;

    while Instant::now() < deadline {
        if task.is_cancelled() {
            break;
        }

        let packet = match demux.next_packet() {
            Ok(Some(packet)) => packet,
            Ok(None) => break,
            // The events are announced in the clear, so a channel the card
            // will not unscramble is crawled like any other.
            Err(error) if is_descrambling_refused(&error) => {
                if !std::mem::replace(&mut refused, true) {
                    warn!(channel_id = channel.id, %error, "Collecting the events only");
                }

                continue;
            }
            Err(error) => {
                warn!(channel_id = channel.id, %error, "Could not read event information");
                continue;
            }
        };
        let Packet::Signaling(signaling) = packet else {
            continue;
        };

        processor.process(signaling)?;
    }

    Ok(())
}
