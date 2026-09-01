//! Recording one programme to the storage.

use std::io::{BufReader, Write};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use chrono::{DateTime, Local};
use mpeg2ts::ts::TsPacketWriter;
use tracing::info;

use chibitv_b25::B25Descrambler;
use chibitv_b61::Descrambler;

use crate::cas::PcscCasModule;
use crate::channel::{Channel, ChannelInner};
use crate::demux::Demux;
use crate::m2ts::{M2tsDemuxer, M2tsMuxer};
use crate::mmt::MmtDemuxer;
use crate::remux::{Mux, Remuxer};
use crate::storage::{Storage, StorageObject};
use crate::task::TaskHandle;
use crate::tuner::Tuners;

const READ_BUFFER_SIZE: usize = 188 * 8192;

/// How often the task is told how far the recording has got.
const REPORT_INTERVAL: Duration = Duration::from_secs(5);

/// How long a name taken from a programme title may be, in characters, so
/// that a long title still leaves a name a file system accepts.
const MAX_TITLE_LENGTH: usize = 80;

/// What is to be recorded.
#[derive(Clone, Debug)]
pub struct Recording {
    pub channel: Channel,
    pub service_id: u16,
    /// The programme being recorded, which names the object it is written to.
    pub title: String,
    /// When the programme starts, which names the object as well.
    pub starts_at: DateTime<Local>,
    /// When the recording stops, whatever the programme does.
    pub ends_at: DateTime<Local>,
}

pub struct Recorder {
    tuners: Arc<Tuners>,
    cas: Arc<PcscCasModule>,
    cas_master_key: [u8; 32],
    storage: Arc<dyn Storage>,
}

impl Recorder {
    pub fn new(
        tuners: Arc<Tuners>,
        cas: Arc<PcscCasModule>,
        cas_master_key: [u8; 32],
        storage: Arc<dyn Storage>,
    ) -> Self {
        Self {
            tuners,
            cas,
            cas_master_key,
            storage,
        }
    }

    /// Records the programme, stopping at its end or when the task is asked to.
    ///
    /// What has been recorded is kept either way: a recording cut short is
    /// finished in the storage just like one that ran to its end.
    pub fn record(&self, recording: &Recording, task: &TaskHandle) -> anyhow::Result<()> {
        let name = object_name(recording);
        let tuner = self.tuners.try_acquire()?;
        info!(
            tuner_id = tuner.id(),
            service_id = recording.service_id,
            name,
            "Acquired tuner for recording"
        );

        tuner.tune(recording.channel.clone())?;
        let reader = tuner.open()?;
        let writer = SharedObject::new(self.storage.create(&name)?);
        let mux = M2tsMuxer::new(TsPacketWriter::new(writer.clone()));

        let result = match recording.channel.inner {
            ChannelInner::IsdbS { .. } | ChannelInner::BonIsdbS { .. } => {
                let descrambler = Descrambler::init(self.cas.clone(), self.cas_master_key, false)?;
                let reader = BufReader::with_capacity(READ_BUFFER_SIZE, reader);
                run(
                    Remuxer::new(MmtDemuxer::new(reader, descrambler), mux)?,
                    recording,
                    task,
                )
            }
            ChannelInner::IsdbT { .. } | ChannelInner::BonIsdbT { .. } => {
                let descrambler = B25Descrambler::init(self.cas.clone())?;
                let demux = M2tsDemuxer::new_for_service(reader, descrambler, recording.service_id);
                run(Remuxer::new(demux, mux)?, recording, task)
            }
        };

        // The recording is finished in the storage before the failure, if any,
        // is reported, so that what was recorded until then is kept.
        let finished = writer.finish();
        result?;
        finished?;

        info!(name, "Recording finished");

        Ok(())
    }
}

fn run<D: Demux, M: Mux>(
    mut remuxer: Remuxer<D, M>,
    recording: &Recording,
    task: &TaskHandle,
) -> anyhow::Result<()> {
    let started_at = Local::now();
    let message = format!(
        "{} until {}",
        recording.channel.name,
        recording.ends_at.format("%H:%M")
    );
    let mut reported_at = None;

    while !task.is_cancelled() {
        let now = Local::now();
        if now >= recording.ends_at {
            break;
        }

        if reported_at.is_none_or(|at: Instant| at.elapsed() >= REPORT_INTERVAL) {
            let recorded = (now - started_at).as_seconds_f32();
            let total = (recording.ends_at - started_at).as_seconds_f32();
            let progress = (total > 0.0).then(|| (recorded / total).clamp(0.0, 1.0));
            task.report(progress, message.clone());
            reported_at = Some(Instant::now());
        }

        // The stream ending early is the tuner giving up, which is as far as
        // this recording goes.
        if remuxer.next()?.is_none() {
            break;
        }
    }

    remuxer.finish()
}

/// Names the object a recording is written to.
fn object_name(recording: &Recording) -> String {
    let title = sanitize(&recording.title);

    format!(
        "{} {}.m2ts",
        recording.starts_at.format("%Y%m%d-%H%M"),
        title
    )
}

/// Turns a programme title into something every file system takes as a name.
fn sanitize(title: &str) -> String {
    let title = title
        .chars()
        .take(MAX_TITLE_LENGTH)
        .map(|character| match character {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
            character if character.is_control() => ' ',
            character => character,
        })
        .collect::<String>();
    let title = title.trim().trim_matches('.').trim();

    match title.is_empty() {
        true => "Untitled".to_string(),
        false => title.to_string(),
    }
}

/// The muxer owns what it writes to, so the object is shared with it and
/// finished here once the recording is over.
#[derive(Clone)]
struct SharedObject(Arc<Mutex<Box<dyn StorageObject>>>);

impl SharedObject {
    fn new(object: Box<dyn StorageObject>) -> Self {
        Self(Arc::new(Mutex::new(object)))
    }

    fn finish(&self) -> anyhow::Result<()> {
        self.0.lock().unwrap().finish()
    }
}

impl Write for SharedObject {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().write(buf)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.0.lock().unwrap().flush()
    }
}

#[cfg(test)]
mod tests {
    use chrono::TimeZone;

    use super::*;

    fn recording(title: &str) -> Recording {
        Recording {
            channel: Channel {
                id: 0,
                name: "UHF 20".to_string(),
                inner: ChannelInner::IsdbT {
                    frequency: 515_142_857,
                    bandwidth_hz: 6_000_000,
                },
            },
            service_id: 101,
            title: title.to_string(),
            starts_at: Local.with_ymd_and_hms(2026, 9, 1, 20, 30, 0).unwrap(),
            ends_at: Local.with_ymd_and_hms(2026, 9, 1, 21, 0, 0).unwrap(),
        }
    }

    #[test]
    fn names_an_object_after_the_programme_and_when_it_starts() {
        assert_eq!(
            object_name(&recording("Evening News")),
            "20260901-2030 Evening News.m2ts"
        );
    }

    #[test]
    fn keeps_a_title_usable_as_a_file_name() {
        assert_eq!(sanitize("News: 7/9\u{7}"), "News_ 7_9");
        assert_eq!(sanitize("   "), "Untitled");
        assert_eq!(sanitize(".."), "Untitled");
        assert_eq!(
            sanitize(&"あ".repeat(200)).chars().count(),
            MAX_TITLE_LENGTH
        );
    }
}
