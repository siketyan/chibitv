use std::io::{Cursor, Read};
use std::sync::{Arc, Mutex};

use anyhow::bail;

use crate::channel::{Channel, ChannelInner};
use crate::tuner::{AnyTuner, Tuner};

/// A tuner that replays a fixed stream and records what it was tuned to.
pub(crate) struct FakeTuner {
    stream: Vec<u8>,
    opens: bool,
    tuned: Arc<Mutex<Vec<ChannelInner>>>,
}

impl FakeTuner {
    pub(crate) fn new(stream: impl Into<Vec<u8>>) -> Self {
        Self {
            stream: stream.into(),
            opens: true,
            tuned: Arc::default(),
        }
    }

    /// A tuner that cannot be opened at all.
    pub(crate) fn failing() -> Self {
        Self {
            opens: false,
            ..Self::new([])
        }
    }

    /// Channels this tuner was tuned to, shared with the tuner itself.
    pub(crate) fn tuned(&self) -> Arc<Mutex<Vec<ChannelInner>>> {
        Arc::clone(&self.tuned)
    }
}

impl Tuner for FakeTuner {
    async fn open(&self) -> anyhow::Result<Box<dyn Read + Send + Sync>> {
        if !self.opens {
            bail!("Could not open tuner");
        }

        Ok(Box::new(Cursor::new(self.stream.clone())))
    }

    async fn tune(&self, channel: Channel) -> anyhow::Result<()> {
        self.tuned.lock().unwrap().push(channel.inner);

        Ok(())
    }
}

impl From<FakeTuner> for AnyTuner {
    fn from(value: FakeTuner) -> Self {
        Self::Fake(value)
    }
}
