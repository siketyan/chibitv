use std::collections::BTreeMap;
use std::io::{ErrorKind, Read};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use connectrpc::{
    ConnectError, RequestContext, Response, Router, ServiceRequest, ServiceResult, ServiceStream,
};
use tokio_stream::wrappers::ReceiverStream;
use tracing::{info, warn};

use crate::channel::{Channel, ChannelInner};
use crate::proto::chibitv::v1::*;
use crate::tuner::{TunerLease, Tuners};

/// Size of a stream chunk sent to a client.
const CHUNK_SIZE: usize = 188 * 512;

/// Number of chunks buffered before the tuner reader is throttled.
const CHUNK_CAPACITY: usize = 64;

/// How long a tuned tuner stays reserved for the Stream call that follows.
const RESERVATION_TIMEOUT: Duration = Duration::from_secs(30);

/// How long to wait for a tuner that a finished stream has not released yet.
const ACQUIRE_TIMEOUT: Duration = Duration::from_secs(5);
const ACQUIRE_INTERVAL: Duration = Duration::from_millis(50);

/// A tuner tuned by a client, kept until the client streams it.
struct Reservation {
    lease: TunerLease,
    reserved_at: Instant,
}

pub struct ChibitvTunerServiceImpl {
    tuners: Arc<Tuners>,
    reservations: Mutex<BTreeMap<u32, Reservation>>,
}

impl ChibitvTunerServiceImpl {
    pub fn new(tuners: Arc<Tuners>) -> Self {
        Self {
            tuners,
            reservations: Mutex::new(BTreeMap::new()),
        }
    }

    pub fn register(self, router: Router) -> Router {
        Arc::new(self).register(router)
    }

    /// Take the tuner a client tuned earlier, or acquire one for it.
    async fn lease(&self, tuner_id: Option<u32>) -> Result<TunerLease, ConnectError> {
        self.release_expired_reservations();

        if let Some(lease) = tuner_id.and_then(|tuner_id| self.take_reservation(tuner_id)) {
            return Ok(lease);
        }

        // A stream that just ended may still be releasing its tuner, so give
        // it a moment before turning the client away.
        let deadline = Instant::now() + ACQUIRE_TIMEOUT;
        loop {
            let result = match tuner_id {
                Some(tuner_id) => self.tuners.try_acquire_by_id(tuner_id),
                None => self.tuners.try_acquire(),
            };

            match result {
                Ok(lease) => return Ok(lease),
                Err(error) if Instant::now() >= deadline => {
                    warn!(%error, "Could not lease a tuner to a client");
                    return Err(ConnectError::resource_exhausted(error.to_string()));
                }
                Err(_) => tokio::time::sleep(ACQUIRE_INTERVAL).await,
            }
        }
    }

    fn take_reservation(&self, tuner_id: u32) -> Option<TunerLease> {
        self.reservations
            .lock()
            .unwrap()
            .remove(&tuner_id)
            .map(|reservation| reservation.lease)
    }

    fn reserve(&self, lease: TunerLease) {
        self.reservations.lock().unwrap().insert(
            lease.id(),
            Reservation {
                lease,
                reserved_at: Instant::now(),
            },
        );
    }

    /// Release the tuners of the clients that tuned but never streamed.
    fn release_expired_reservations(&self) {
        self.reservations
            .lock()
            .unwrap()
            .retain(|tuner_id, reservation| {
                let reserved = reservation.reserved_at.elapsed() < RESERVATION_TIMEOUT;
                if !reserved {
                    info!(tuner_id, "Released a tuner reserved by an abandoned client");
                }

                reserved
            });
    }
}

#[allow(refining_impl_trait)]
impl ChibitvTunerService for ChibitvTunerServiceImpl {
    async fn tune(
        &self,
        _ctx: RequestContext,
        request: ServiceRequest<'_, TuneRequest>,
    ) -> ServiceResult<TuneResponse> {
        let channel = requested_channel(&request)?;
        let lease = self.lease(request.tuner_id).await?;
        let tuner_id = lease.id();

        info!(tuner_id, channel = %channel.name, "Tuning for a client");

        lease.tune(channel).await.map_err(|error| {
            warn!(tuner_id, %error, "Could not tune for a client");
            ConnectError::unavailable(error.to_string())
        })?;

        self.reserve(lease);

        Response::ok(TuneResponse {
            tuner_id,
            ..Default::default()
        })
    }

    async fn stream(
        &self,
        _ctx: RequestContext,
        request: ServiceRequest<'_, TunerStreamRequest>,
    ) -> ServiceResult<ServiceStream<TunerStreamResponse>> {
        let lease = self.lease(request.tuner_id).await?;
        let tuner_id = lease.id();
        let mut input = lease.open().await.map_err(|error| {
            warn!(tuner_id, %error, "Could not open a tuner for a client");
            ConnectError::unavailable(error.to_string())
        })?;
        let (tx, rx) = tokio::sync::mpsc::channel(CHUNK_CAPACITY);

        // Reading a tuner blocks, so it cannot be done on the runtime.
        std::thread::spawn(move || {
            info!(tuner_id, "Streaming a tuner to a client");

            let mut buffer = vec![0; CHUNK_SIZE];
            loop {
                let length = match input.read(&mut buffer) {
                    Ok(0) => break,
                    Ok(length) => length,
                    Err(error) if error.kind() == ErrorKind::Interrupted => continue,
                    Err(error) => {
                        warn!(tuner_id, %error, "Could not read a tuner for a client");
                        break;
                    }
                };

                let response = TunerStreamResponse {
                    chunk: buffer[..length].to_vec(),
                    ..Default::default()
                };

                if tx.blocking_send(Ok(response)).is_err() {
                    break;
                }
            }

            info!(tuner_id, "Stopped streaming a tuner to a client");
        });

        Response::stream_ok(ReceiverStream::new(rx))
    }
}

fn requested_channel(request: &ServiceRequest<'_, TuneRequest>) -> Result<Channel, ConnectError> {
    let inner = match request.channel.as_ref() {
        Some(tune_request::ChannelView::IsdbS(channel)) => ChannelInner::IsdbS {
            frequency: channel.frequency,
            stream_id: channel.stream_id,
        },
        Some(tune_request::ChannelView::IsdbT(channel)) => ChannelInner::IsdbT {
            frequency: channel.frequency,
            bandwidth_hz: channel.bandwidth_hz,
        },
        None => return Err(ConnectError::invalid_argument("channel is required")),
    };

    Ok(Channel {
        // Channels are numbered by the client, which knows nothing about the
        // channels configured here.
        id: 0,
        name: request.channel_name.to_string(),
        inner,
    })
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;
    use std::net::Ipv4Addr;

    use async_trait::async_trait;
    use connectrpc::ErrorCode;
    use connectrpc::client::{ClientConfig, HttpClient};
    use connectrpc::server::Server;

    use super::*;
    use crate::tuner::Tuner;

    struct FakeTuner;

    #[async_trait]
    impl Tuner for FakeTuner {
        async fn open(&self) -> anyhow::Result<Box<dyn Read + Send + Sync>> {
            Ok(Box::new(Cursor::new(b"a raw stream".to_vec())))
        }
    }

    fn tuners() -> Arc<Tuners> {
        let mut tuners = Tuners::default();
        tuners.add_tuner(0, FakeTuner);

        Arc::new(tuners)
    }

    async fn serve(tuners: Arc<Tuners>) -> ChibitvTunerServiceClient<HttpClient> {
        let router = ChibitvTunerServiceImpl::new(tuners).register(Router::new());
        let server = Server::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let address = server.local_addr().unwrap();
        tokio::spawn(async move { server.serve(router).await });

        ChibitvTunerServiceClient::new(
            HttpClient::plaintext(),
            ClientConfig::new(format!("http://{address}").parse().unwrap()),
        )
    }

    #[tokio::test]
    async fn keeps_the_tuner_reserved_after_tuning_it() {
        let tuners = tuners();
        let client = serve(Arc::clone(&tuners)).await;

        let response = client
            .tune(TuneRequest {
                channel: Some(
                    IsdbTChannel {
                        frequency: 515_142_857,
                        bandwidth_hz: 6_000_000,
                        ..Default::default()
                    }
                    .into(),
                ),
                ..Default::default()
            })
            .await
            .unwrap();

        assert_eq!(response.into_owned().tuner_id, 0);
        assert_eq!(tuners.is_in_use(0), Some(true));
    }

    #[tokio::test]
    async fn rejects_tuning_without_a_channel() {
        let client = serve(tuners()).await;

        let error = client.tune(TuneRequest::default()).await.unwrap_err();

        assert_eq!(error.code, ErrorCode::InvalidArgument);
    }

    #[tokio::test]
    async fn streams_the_raw_tuner_output() {
        let client = serve(tuners()).await;

        let mut stream = client.stream(TunerStreamRequest::default()).await.unwrap();
        let mut received = Vec::new();
        while let Some(message) = stream.message().await.unwrap() {
            received.extend_from_slice(&message.to_owned_message().chunk);
        }

        assert_eq!(received, b"a raw stream");
    }
}
