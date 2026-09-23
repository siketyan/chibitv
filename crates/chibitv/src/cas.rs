use std::sync::{Arc, mpsc};

use anyhow::bail;
use pcsc::{Card, Context, Error, Protocols, Scope, ShareMode};
use tracing::{debug, warn};

/// A run of commands for the card, and where its responses go.
type Job = (Vec<Vec<u8>>, mpsc::SyncSender<anyhow::Result<Vec<Vec<u8>>>>);

/// The CAS module every descrambler shares, speaking both ARIB STD-B25 and STD-B61.
///
/// One thread talks to the card and takes the commands in the order they come, so that a run of
/// them is never interleaved with another, and whoever sent them goes on without waiting.
pub struct SharedCasModule {
    jobs: mpsc::Sender<Job>,
}

impl SharedCasModule {
    pub fn open() -> anyhow::Result<Arc<Self>> {
        let mut card = PcscCard {
            card: Some(connect()?),
        };
        let (jobs, jobs_rx) = mpsc::channel::<Job>();

        std::thread::spawn(move || {
            for (commands, responses) in jobs_rx {
                // Whoever sent them may have stopped waiting.
                let _ = responses.send(card.transmit(&commands));
            }
        });

        Ok(Arc::new(Self { jobs }))
    }

    fn transmit(&self, commands: Vec<Vec<u8>>) -> mpsc::Receiver<anyhow::Result<Vec<Vec<u8>>>> {
        let (tx, rx) = mpsc::sync_channel(1);
        // Should the thread have gone, the sender is dropped with the job, which the receiver
        // tells as the module no longer running.
        let _ = self.jobs.send((commands, tx));
        rx
    }
}

impl chibitv_b25::CasModule for SharedCasModule {
    fn transmit(&self, commands: Vec<Vec<u8>>) -> chibitv_b25::PendingResponses {
        SharedCasModule::transmit(self, commands)
    }
}

impl chibitv_b61::CasModule for SharedCasModule {
    fn transmit(&self, commands: Vec<Vec<u8>>) -> chibitv_b61::PendingResponses {
        SharedCasModule::transmit(self, commands)
    }
}

/// The card, reached over PC/SC.
struct PcscCard {
    /// The connection, or `None` while it has been lost and not reopened yet.
    card: Option<Card>,
}

impl PcscCard {
    fn transmit(&mut self, commands: &[Vec<u8>]) -> anyhow::Result<Vec<Vec<u8>>> {
        if let Some(card) = self.card.as_ref() {
            match transmit_all(card, commands) {
                Err(e) if is_connection_lost(&e) => {
                    warn!(error = %e, "CAS module connection lost, reopening");
                    self.card = None;
                }
                result => return Ok(result?),
            }
        }

        // The whole run is sent again, as a command may depend on the ones before it on the same
        // connection.
        let card = self.card.insert(connect()?);
        Ok(transmit_all(card, commands)?)
    }
}

fn transmit_all(card: &Card, commands: &[Vec<u8>]) -> Result<Vec<Vec<u8>>, Error> {
    let mut buf = [0u8; 4096];
    commands
        .iter()
        .map(|command| Ok(card.transmit(command, &mut buf)?.to_vec()))
        .collect()
}

/// Whether the error means the card handle is no longer usable and the connection must be
/// opened again, rather than the command itself having failed.
///
/// Another process resetting the card, even if it releases it afterwards, is the typical case:
/// every command on the old handle keeps failing with [`Error::ResetCard`] until it is reopened.
fn is_connection_lost(error: &Error) -> bool {
    matches!(
        error,
        Error::ResetCard
            | Error::RemovedCard
            | Error::UnpoweredCard
            | Error::UnresponsiveCard
            | Error::NoSmartcard
            | Error::SharingViolation
            | Error::ReaderUnavailable
            | Error::InvalidHandle
            | Error::CommError
            | Error::CommDataLost
            | Error::NoService
            | Error::ServiceStopped
            | Error::Shutdown
    )
}

fn connect() -> anyhow::Result<Card> {
    let context = Context::establish(Scope::System)?;
    let mut readers = vec![0u8; 4096];
    let Some(reader_name) = context.list_readers(&mut readers)?.next() else {
        bail!("CAS reader not found");
    };

    debug!(
        reader = %String::from_utf8_lossy(reader_name.to_bytes()),
        "Opening CAS module"
    );
    Ok(context.connect(reader_name, ShareMode::Shared, Protocols::ANY)?)
}
