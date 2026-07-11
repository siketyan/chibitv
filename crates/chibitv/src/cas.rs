use std::sync::{Arc, Mutex, MutexGuard};

use anyhow::bail;
use pcsc::{Card, Context, Protocols, Scope, ShareMode};
use tracing::debug;

pub struct PcscCasModule {
    card: Mutex<Card>,
}

struct PcscCasModuleGuard<'a> {
    card: MutexGuard<'a, Card>,
}

impl PcscCasModuleGuard<'_> {
    fn transmit(&mut self, command: &[u8], response: &mut [u8]) -> anyhow::Result<usize> {
        Ok(self.card.transmit(command, response)?.len())
    }
}

impl PcscCasModule {
    pub fn open() -> anyhow::Result<Self> {
        let context = Context::establish(Scope::System)?;
        let mut readers = vec![0u8; 4096];
        let Some(reader_name) = context.list_readers(&mut readers)?.next() else {
            bail!("CAS reader not found");
        };

        debug!(
            reader = %String::from_utf8_lossy(reader_name.to_bytes()),
            "Opening CAS module"
        );
        let card = context.connect(reader_name, ShareMode::Shared, Protocols::ANY)?;

        Ok(Self {
            card: Mutex::new(card),
        })
    }

    pub fn open_shared() -> anyhow::Result<Arc<Self>> {
        Ok(Arc::new(Self::open()?))
    }
}

impl chibitv_b25::CasModule for PcscCasModule {
    fn transmit(&self, command: &[u8], response: &mut [u8]) -> anyhow::Result<usize> {
        let card = self
            .card
            .lock()
            .map_err(|_| anyhow::anyhow!("CAS module lock is poisoned"))?;
        Ok(card.transmit(command, response)?.len())
    }
}

impl chibitv_b61::CasModule for PcscCasModule {
    fn transmit(&self, command: &[u8], response: &mut [u8]) -> anyhow::Result<usize> {
        let mut module = self.lock()?;
        module.transmit(command, response)
    }

    fn lock(&self) -> anyhow::Result<Box<dyn chibitv_b61::CasModuleGuard + '_>> {
        let card = self
            .card
            .lock()
            .map_err(|_| anyhow::anyhow!("CAS module lock is poisoned"))?;
        Ok(Box::new(PcscCasModuleGuard { card }))
    }
}

impl chibitv_b61::CasModuleGuard for PcscCasModuleGuard<'_> {
    fn transmit(&mut self, command: &[u8], response: &mut [u8]) -> anyhow::Result<usize> {
        PcscCasModuleGuard::transmit(self, command, response)
    }
}
