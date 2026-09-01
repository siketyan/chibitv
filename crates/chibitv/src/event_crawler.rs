use std::io::BufReader;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tracing::{info, warn};

use chibitv_b25::B25Descrambler;
use chibitv_b61::Descrambler;

use crate::cas::PcscCasModule;
use crate::channel::{Channel, ChannelInner};
use crate::demux::{Demux, Packet};
use crate::m2ts::M2tsDemuxer;
use crate::mmt::MmtDemuxer;
use crate::registry::Registry;
use crate::service_information::ServiceInformationProcessor;
use crate::task::TaskHandle;
use crate::tuner::Tuners;

const READ_BUFFER_SIZE: usize = 188 * 8192;

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

    /// Tunes every channel in turn and collects the events it announces into
    /// the registry, which stores them.
    ///
    /// The task is asked to stop between packets, so cancelling it keeps the
    /// events collected so far and gives the tuner back at once.
    pub fn crawl(
        &self,
        channels: &[Channel],
        registry: Arc<Registry>,
        dwell_time: Duration,
        task: &TaskHandle,
    ) -> anyhow::Result<()> {
        let tuner = self.tuners.try_acquire()?;
        info!(tuner_id = tuner.id(), "Acquired tuner for event crawling");

        for (index, channel) in channels.iter().enumerate() {
            if task.is_cancelled() {
                break;
            }

            info!(channel_id = channel.id, channel = %channel.name, "Crawling events");
            task.report(
                Some(index as f32 / channels.len() as f32),
                format!("Crawling {}", channel.name),
            );

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
                ChannelInner::IsdbT { .. } | ChannelInner::BonIsdbT { .. } => {
                    let descrambler = B25Descrambler::init(self.cas.clone())?;
                    let mut demux = M2tsDemuxer::new(reader, descrambler);
                    crawl_channel(&mut demux, channel, &registry, deadline, task)?;
                }
                ChannelInner::IsdbS { .. } | ChannelInner::BonIsdbS { .. } => {
                    let descrambler =
                        Descrambler::init(self.cas.clone(), self.cas_master_key, false)?;
                    let mut demux = MmtDemuxer::new(
                        BufReader::with_capacity(READ_BUFFER_SIZE, reader),
                        descrambler,
                    );
                    crawl_channel(&mut demux, channel, &registry, deadline, task)?;
                }
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
    task: &TaskHandle,
) -> anyhow::Result<()> {
    let mut processor =
        ServiceInformationProcessor::new(channel.id, Some(Arc::clone(registry)), None);

    while Instant::now() < deadline {
        if task.is_cancelled() {
            break;
        }

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

        processor.process(signaling)?;
    }

    Ok(())
}
