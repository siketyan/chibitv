use std::sync::{Arc, Mutex, MutexGuard};

use anyhow::bail;
use pcsc::{Card, Context, Error, Protocols, Scope, ShareMode};
use tracing::{debug, warn};

pub struct PcscCasModule {
    /// The card connection, or `None` while it has been lost and not reopened yet.
    card: Mutex<Option<Card>>,
}

struct PcscCasModuleGuard<'a> {
    card: MutexGuard<'a, Option<Card>>,
}

impl PcscCasModuleGuard<'_> {
    fn transmit(&mut self, command: &[u8], response: &mut [u8]) -> anyhow::Result<usize> {
        if let Some(card) = self.card.as_ref() {
            match card.transmit(command, response) {
                Ok(received) => return Ok(received.len()),
                Err(e) if is_connection_lost(&e) => {
                    warn!(error = %e, "CAS module connection lost, reopening");
                    *self.card = None;
                }
                Err(e) => return Err(e.into()),
            }
        }

        let card = self.card.insert(connect()?);
        Ok(card.transmit(command, response)?.len())
    }
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

impl PcscCasModule {
    pub fn open() -> anyhow::Result<Self> {
        Ok(Self {
            card: Mutex::new(Some(connect()?)),
        })
    }

    pub fn open_shared() -> anyhow::Result<Arc<Self>> {
        Ok(Arc::new(Self::open()?))
    }

    fn lock(&self) -> anyhow::Result<PcscCasModuleGuard<'_>> {
        let card = self
            .card
            .lock()
            .map_err(|_| anyhow::anyhow!("CAS module lock is poisoned"))?;
        Ok(PcscCasModuleGuard { card })
    }
}

impl chibitv_b25::CasModule for PcscCasModule {
    fn transmit(&self, command: &[u8], response: &mut [u8]) -> anyhow::Result<usize> {
        self.lock()?.transmit(command, response)
    }
}

impl chibitv_b61::CasModule for PcscCasModule {
    fn transmit(&self, command: &[u8], response: &mut [u8]) -> anyhow::Result<usize> {
        self.lock()?.transmit(command, response)
    }

    fn lock(&self) -> anyhow::Result<Box<dyn chibitv_b61::CasModuleGuard + '_>> {
        Ok(Box::new(self.lock()?))
    }
}

impl chibitv_b61::CasModuleGuard for PcscCasModuleGuard<'_> {
    fn transmit(&mut self, command: &[u8], response: &mut [u8]) -> anyhow::Result<usize> {
        PcscCasModuleGuard::transmit(self, command, response)
    }
}
