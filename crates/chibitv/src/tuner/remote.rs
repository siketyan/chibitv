use std::io::Read;
use std::sync::mpsc::{Receiver, channel};
use std::sync::{Arc, Mutex};

use anyhow::{Context, bail};
use async_trait::async_trait;
use bytes::Bytes;
use connectrpc::client::{ClientConfig, HttpClient};
use http::Uri;
use tokio::sync::{OwnedSemaphorePermit, Semaphore, oneshot};
use tracing::{info, warn};

use crate::channel::{Channel, ChannelInner};
use crate::proto::chibitv::v1::*;
use crate::tuner::Tuner;

/// Number of chunks buffered between the RPC client and the reader.
const STREAM_CAPACITY: usize = 64;

/// A tuner of another chibitv instance running in tuner mode.
pub struct RemoteTuner {
    client: ChibitvTunerServiceClient<HttpClient>,
    tuner_id: Option<u32>,

    /// Tuner reserved by the last successful tuning on the remote instance.
    reserved_tuner_id: Mutex<Option<u32>>,
}

impl RemoteTuner {
    pub fn new(address: &str, tuner_id: Option<u32>) -> anyhow::Result<Self> {
        let uri = parse_address(address)?;

        info!(%uri, "Using a remote tuner");

        Ok(Self {
            client: ChibitvTunerServiceClient::new(HttpClient::plaintext(), ClientConfig::new(uri)),
            tuner_id,
            reserved_tuner_id: Mutex::new(None),
        })
    }
}

#[async_trait]
impl Tuner for RemoteTuner {
    async fn open(&self) -> anyhow::Result<Box<dyn Read + Send + Sync>> {
        let tuner_id = (*self.reserved_tuner_id.lock().unwrap()).or(self.tuner_id);
        let mut stream = self
            .client
            .stream(TunerStreamRequest {
                tuner_id,
                ..Default::default()
            })
            .await?;

        let (chunk_tx, chunk_rx) = channel::<(Bytes, OwnedSemaphorePermit)>();
        let (cancel_tx, mut cancel_rx) = oneshot::channel();
        // Backpressure: the tuner keeps producing at the rate of the
        // broadcast, so the stream must not run ahead of the reader.
        let permits = Arc::new(Semaphore::new(STREAM_CAPACITY));

        tokio::spawn(async move {
            loop {
                let Ok(permit) = Arc::clone(&permits).acquire_owned().await else {
                    break;
                };

                let message = tokio::select! {
                    _ = &mut cancel_rx => break,
                    message = stream.message() => message,
                };

                match message {
                    Ok(Some(message)) => {
                        let chunk = Bytes::from(message.to_owned_message().chunk);
                        if chunk_tx.send((chunk, permit)).is_err() {
                            break;
                        }
                    }
                    Ok(None) => break,
                    Err(error) => {
                        warn!(%error, "The remote tuner stream was interrupted");
                        break;
                    }
                }
            }
        });

        Ok(Box::new(RemoteTunerReader {
            chunks: Mutex::new(chunk_rx),
            pending: Bytes::new(),
            _cancel: cancel_tx,
        }))
    }

    async fn tune(&self, channel: Channel) -> anyhow::Result<()> {
        let response = self
            .client
            .tune(TuneRequest {
                tuner_id: self.tuner_id,
                channel_name: channel.name,
                channel: Some(match channel.inner {
                    ChannelInner::IsdbS {
                        frequency,
                        stream_id,
                    } => IsdbSChannel {
                        frequency,
                        stream_id,
                        ..Default::default()
                    }
                    .into(),
                    ChannelInner::IsdbT {
                        frequency,
                        bandwidth_hz,
                    } => IsdbTChannel {
                        frequency,
                        bandwidth_hz,
                        ..Default::default()
                    }
                    .into(),
                }),
                ..Default::default()
            })
            .await?;
        let tuner_id = response.into_owned().tuner_id;

        info!(tuner_id, "Tuned a remote tuner");
        *self.reserved_tuner_id.lock().unwrap() = Some(tuner_id);

        Ok(())
    }
}

struct RemoteTunerReader {
    // The receiver is not `Sync` on its own, while a tuner input is.
    chunks: Mutex<Receiver<(Bytes, OwnedSemaphorePermit)>>,
    pending: Bytes,

    /// Dropped along with the reader to stop the RPC stream even while the
    /// tuner produces nothing.
    _cancel: oneshot::Sender<()>,
}

impl Read for RemoteTunerReader {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        while self.pending.is_empty() {
            // The permit is released here, letting the stream read one more
            // chunk ahead.
            let Ok((chunk, _permit)) = self.chunks.get_mut().unwrap().recv() else {
                return Ok(0);
            };

            self.pending = chunk;
        }

        let length = self.pending.len().min(buf.len());
        buf[..length].copy_from_slice(&self.pending.split_to(length));

        Ok(length)
    }
}

fn parse_address(address: &str) -> anyhow::Result<Uri> {
    let uri: Uri = address
        .parse()
        .with_context(|| format!("`{address}` is not a valid tuner address"))?;

    if uri.scheme().is_none() {
        bail!(
            "`{address}` is not a fully qualified tuner address, such as `http://tuner.local:3002`"
        );
    }

    Ok(uri)
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;
    use std::net::Ipv4Addr;

    use connectrpc::server::Server;

    use super::*;
    use crate::rpc::tuner::ChibitvTunerServiceImpl;
    use crate::tuner::Tuners;

    struct FakeTuner {
        tuned: Arc<Mutex<Vec<ChannelInner>>>,
    }

    #[async_trait]
    impl Tuner for FakeTuner {
        async fn open(&self) -> anyhow::Result<Box<dyn Read + Send + Sync>> {
            Ok(Box::new(Cursor::new(b"a raw stream".to_vec())))
        }

        async fn tune(&self, channel: Channel) -> anyhow::Result<()> {
            self.tuned.lock().unwrap().push(channel.inner);

            Ok(())
        }
    }

    #[tokio::test]
    async fn tunes_and_streams_a_tuner_of_another_instance() {
        let tuned = Arc::new(Mutex::new(Vec::new()));
        let mut tuners = Tuners::default();
        tuners.add_tuner(
            0,
            FakeTuner {
                tuned: Arc::clone(&tuned),
            },
        );

        let router = ChibitvTunerServiceImpl::new(Arc::new(tuners)).register(Default::default());
        let server = Server::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let address = server.local_addr().unwrap();
        tokio::spawn(async move { server.serve(router).await });

        let tuner = RemoteTuner::new(&format!("http://{address}"), None).unwrap();
        tuner
            .tune(Channel {
                id: 3,
                name: "Fake".to_string(),
                inner: ChannelInner::IsdbT {
                    frequency: 515_142_857,
                    bandwidth_hz: 6_000_000,
                },
            })
            .await
            .unwrap();
        let mut input = tuner.open().await.unwrap();

        // Reading a tuner blocks, as every demuxer does it on a thread of its own.
        let stream = tokio::task::spawn_blocking(move || {
            let mut stream = Vec::new();
            input.read_to_end(&mut stream).map(|_| stream)
        })
        .await
        .unwrap()
        .unwrap();

        assert_eq!(stream, b"a raw stream");
        assert!(matches!(
            tuned.lock().unwrap().as_slice(),
            [ChannelInner::IsdbT {
                frequency: 515_142_857,
                bandwidth_hz: 6_000_000,
            }],
        ));
    }

    #[test]
    fn rejects_an_address_without_a_scheme() {
        assert_eq!(
            parse_address("http://tuner.local:3002").unwrap(),
            Uri::from_static("http://tuner.local:3002"),
        );
        assert!(parse_address("tuner.local:3002").is_err());
    }
}
