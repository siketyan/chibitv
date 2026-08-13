use std::io::Read;
use std::sync::mpsc::{Receiver, channel};
use std::sync::{Arc, Mutex};

use anyhow::Context;
use bytes::Bytes;
use connectrpc::client::{ClientConfig, HttpClient};
use connectrpc::compression::{CompressionPolicy, CompressionRegistry};
use http::Uri;
use tokio::runtime::{Builder, Runtime};
use tokio::sync::{OwnedSemaphorePermit, Semaphore, oneshot};
use tracing::{info, warn};

use crate::channel::{Channel, ChannelInner};
use crate::proto::chibitv::v1::*;
use crate::tuner::Tuner;

/// Number of chunks buffered between the RPC client and the reader.
const STREAM_CAPACITY: usize = 64;

/// A tuner of another chibitv instance running in tuner mode.
pub struct RemoteTuner {
    /// Runtime driving the RPC calls. The `Tuner` API is synchronous and is
    /// called from both blocking threads and async contexts, so the calls
    /// cannot run on the runtime of the caller. Taken by [`Drop`], and present
    /// at any other time.
    runtime: Option<Runtime>,
    client: ChibitvTunerServiceClient<HttpClient>,
    tuner_id: Option<u32>,

    /// Tuner reserved by the last successful tuning on the remote instance.
    reserved_tuner_id: Mutex<Option<u32>>,
}

impl RemoteTuner {
    pub fn new(address: &str, tuner_id: Option<u32>) -> anyhow::Result<Self> {
        let uri = parse_address(address)?;
        let runtime = Builder::new_multi_thread()
            .worker_threads(1)
            .thread_name("remote-tuner")
            .enable_all()
            .build()?;

        info!(%uri, "Using a remote tuner");

        Ok(Self {
            runtime: Some(runtime),
            client: ChibitvTunerServiceClient::new(HttpClient::plaintext(), client_config(uri)),
            tuner_id,
            reserved_tuner_id: Mutex::new(None),
        })
    }

    fn runtime(&self) -> &Runtime {
        self.runtime
            .as_ref()
            .expect("the runtime is only taken away while the tuner is dropped")
    }

    /// Run a call on the tuner's own runtime and block the caller until it
    /// completes.
    fn call<F>(&self, future: F) -> anyhow::Result<F::Output>
    where
        F: Future + Send + 'static,
        F::Output: Send + 'static,
    {
        let (tx, rx) = channel();

        self.runtime().spawn(async move {
            let _ = tx.send(future.await);
        });

        Ok(rx.recv()?)
    }
}

impl Drop for RemoteTuner {
    fn drop(&mut self) {
        // Dropping a runtime waits for its tasks to stop, which panics when
        // the tuner is dropped in an async context.
        if let Some(runtime) = self.runtime.take() {
            runtime.shutdown_background();
        }
    }
}

impl Tuner for RemoteTuner {
    fn open(&self) -> anyhow::Result<Box<dyn Read + Send + Sync>> {
        let tuner_id = (*self.reserved_tuner_id.lock().unwrap()).or(self.tuner_id);
        let request = TunerStreamRequest {
            tuner_id,
            ..Default::default()
        };

        let (chunk_tx, chunk_rx) = channel::<(Bytes, OwnedSemaphorePermit)>();
        let (cancel_tx, mut cancel_rx) = oneshot::channel();
        // Backpressure: the tuner keeps producing at the rate of the
        // broadcast, so the stream must not run ahead of the reader.
        let permits = Arc::new(Semaphore::new(STREAM_CAPACITY));
        let client = self.client.clone();

        self.runtime().spawn(async move {
            let mut stream = match client.stream(request).await {
                Ok(stream) => stream,
                Err(error) => {
                    warn!(%error, "Could not stream the remote tuner");
                    return;
                }
            };

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

    fn tune(&self, channel: Channel) -> anyhow::Result<()> {
        let request = TuneRequest {
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
        };

        let client = self.client.clone();
        let response = self.call(async move { client.tune(request).await })??;
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

/// Messages are encoded as binary protobuf, which the Connect client does by
/// default. Compression is negotiated away as a broadcast stream is compressed
/// already, and deflating it again would only burn CPU on the tuner.
fn client_config(uri: Uri) -> ClientConfig {
    ClientConfig::new(uri)
        .with_compression(CompressionRegistry::new())
        .with_compression_policy(CompressionPolicy::disabled())
}

fn parse_address(address: &str) -> anyhow::Result<Uri> {
    let address = match address.contains("://") {
        true => address.to_string(),
        _ => format!("http://{address}"),
    };

    address
        .parse()
        .with_context(|| format!("`{address}` is not a valid tuner address"))
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;
    use std::net::Ipv4Addr;

    use tokio::net::TcpListener;

    use super::*;
    use crate::rpc::tuner::ChibitvTunerServiceImpl;
    use crate::tuner::Tuners;

    struct FakeTuner {
        tuned: Arc<Mutex<Vec<ChannelInner>>>,
    }

    impl Tuner for FakeTuner {
        fn open(&self) -> anyhow::Result<Box<dyn Read + Send + Sync>> {
            Ok(Box::new(Cursor::new(b"a raw stream".to_vec())))
        }

        fn tune(&self, channel: Channel) -> anyhow::Result<()> {
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

        let router = Arc::new(ChibitvTunerServiceImpl::new(Arc::new(tuners)))
            .register(connectrpc::Router::new())
            .into_axum_router();
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, router).await });

        // The tuner API blocks, so it cannot be driven from the runtime that
        // runs the server under test.
        let stream = tokio::task::spawn_blocking(move || {
            let tuner = RemoteTuner::new(&address.to_string(), None)?;

            tuner.tune(Channel {
                id: 3,
                name: "Fake".to_string(),
                inner: ChannelInner::IsdbT {
                    frequency: 515_142_857,
                    bandwidth_hz: 6_000_000,
                },
            })?;

            let mut stream = Vec::new();
            tuner.open()?.read_to_end(&mut stream)?;

            anyhow::Ok(stream)
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
    fn transfers_binary_messages_without_compressing_them() {
        let config = client_config(Uri::from_static("http://tuner.local:3002"));

        assert_eq!(config.codec_format(), connectrpc::CodecFormat::Proto);
        assert!(config.compression().accept_encoding_header().is_empty());
    }

    #[test]
    fn assumes_http_for_an_address_without_a_scheme() {
        assert_eq!(
            parse_address("tuner.local:3002").unwrap(),
            Uri::from_static("http://tuner.local:3002"),
        );
        assert_eq!(
            parse_address("http://tuner.local:3002").unwrap(),
            Uri::from_static("http://tuner.local:3002"),
        );
    }
}
