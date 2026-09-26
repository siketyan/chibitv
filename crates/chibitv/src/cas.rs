use std::sync::{Arc, Mutex};

use anyhow::bail;
use pcsc::{Card, Context, Error, Protocols, Scope, ShareMode};
use tracing::{debug, warn};

/// The CAS module every descrambler shares, speaking both ARIB STD-B25 and STD-B61.
///
/// The mutex keeps the commands of one descrambler from being interleaved with another's.
pub type SharedCasModule = Mutex<PcscCasModule>;

/// The card, reached over PC/SC.
pub struct PcscCasModule {
    /// The connection, or `None` while it has been lost and not reopened yet.
    card: Option<Card>,
}

impl PcscCasModule {
    pub fn open() -> anyhow::Result<Arc<SharedCasModule>> {
        Ok(Arc::new(Mutex::new(Self {
            card: Some(connect()?),
        })))
    }

    fn transmit(&mut self, command: &[u8]) -> anyhow::Result<Vec<u8>> {
        let card = match &mut self.card {
            Some(card) => card,
            None => self.card.insert(connect()?),
        };

        let mut buf = [0u8; 4096];
        match card.transmit(command, &mut buf) {
            Ok(response) => Ok(response.to_vec()),
            Err(e) => {
                // The command is not sent again on a new connection, as it may depend on the ones
                // before it on the old one: the descrambler asks again with the next ECM.
                if is_connection_lost(&e) {
                    warn!(error = %e, "CAS module connection lost, reopening");
                    self.card = None;
                }
                Err(e.into())
            }
        }
    }
}

impl chibitv_b25::CasModule for PcscCasModule {
    fn transmit(&mut self, command: &[u8]) -> anyhow::Result<Vec<u8>> {
        PcscCasModule::transmit(self, command)
    }
}

impl chibitv_b61::CasModule for PcscCasModule {
    fn transmit(&mut self, command: &[u8]) -> anyhow::Result<Vec<u8>> {
        PcscCasModule::transmit(self, command)
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
