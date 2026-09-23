//! Where chibitv keeps what has to survive a restart.
//!
//! A [`Store`] is one database, and the repositories it is made of are one per
//! kind of thing kept in it — [`ChannelRepository`] for the channels being
//! served, [`ServiceRepository`] for the services they carry,
//! [`EventRepository`] for the broadcast schedule so far.
//! Everything the rest of the program asks a database for is declared here, so
//! another database is a second implementation of those traits and one more
//! URL scheme in [`open`] rather than a rewrite, and whatever is worth keeping
//! next is a trait beside them rather than a store of its own.

mod channel;
mod event;
mod service;
mod sqlite;

use std::sync::Arc;

use anyhow::bail;

pub use crate::service::StoredService;
pub use channel::{ChannelRepository, NewChannel, StoredChannel};
pub use event::{EventRepository, SectionId};
pub use service::ServiceRepository;
pub use sqlite::SqliteStore;

/// A database chibitv keeps its state in.
///
/// A backend implements every repository this is made of, so that one
/// connection serves all of them.
pub trait Store: ChannelRepository + ServiceRepository + EventRepository + Send + Sync {}

/// Opens the store the URL points at.
///
/// The scheme picks the backend, so another database becomes reachable by
/// matching one more scheme here.
pub async fn open(url: &str) -> anyhow::Result<Arc<dyn Store>> {
    match url.split_once(':') {
        Some(("sqlite", _)) => Ok(Arc::new(SqliteStore::open(url).await?)),
        Some((scheme, _)) => bail!("`{scheme}` is not a database chibitv can keep its state in"),
        None => bail!("`{url}` is not a database URL"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn refuses_a_database_it_cannot_open() {
        assert!(open("mysql://localhost/chibitv").await.is_err());
        assert!(open("chibitv.db").await.is_err());
    }
}
