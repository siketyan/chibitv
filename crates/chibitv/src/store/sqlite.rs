use std::str::FromStr;
use std::time::Duration;

use async_trait::async_trait;
use chrono::{DateTime, NaiveDateTime, TimeDelta, Utc};
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous};
use sqlx::{QueryBuilder, Row, Sqlite, SqlitePool};

use super::{EventStore, SectionId, Store, StoredEvent};

/// How long a statement waits for the database to be free again.
const BUSY_TIMEOUT: Duration = Duration::from_secs(5);

/// The columns of an event, in the order they are written in.
const COLUMNS: &str = "service_id, event_id, original_network_id, stream_id, table_id, \
                       section_number, start_time, duration_seconds, language_code, name, text, \
                       description";

/// Reading them back goes by name, so this needs to name them all rather than
/// keep the order above.
const SELECT_EVENTS: &str = "SELECT service_id, event_id, start_time, duration_seconds, \
                             language_code, name, text, description FROM events";

/// The state chibitv keeps in a SQLite database.
///
/// Its schema lives in `migrations/sqlite`, one migration per thing kept.
pub struct SqliteStore {
    pool: SqlitePool,
}

impl Store for SqliteStore {}

impl SqliteStore {
    /// Opens the database the URL points at, creating it when it is not there
    /// yet and bringing its schema up to date.
    pub async fn open(url: &str) -> anyhow::Result<Self> {
        let options = SqliteConnectOptions::from_str(url)?
            .create_if_missing(true)
            // The schedule is written while the API reads it, and losing the
            // last few sections to a crash only costs one more crawl.
            .journal_mode(SqliteJournalMode::Wal)
            .synchronous(SqliteSynchronous::Normal)
            .busy_timeout(BUSY_TIMEOUT);

        // SQLite takes one writer at a time and this store has one writer, so
        // a single connection is enough. It also keeps a database held in
        // memory, which every connection would otherwise get its own of, whole.
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await?;

        sqlx::migrate!("./migrations/sqlite").run(&pool).await?;

        Ok(Self { pool })
    }
}

#[async_trait]
impl EventStore for SqliteStore {
    async fn load_events(&self) -> anyhow::Result<Vec<StoredEvent>> {
        let rows = sqlx::query(SELECT_EVENTS).fetch_all(&self.pool).await?;

        rows.iter().map(read_event).collect()
    }

    async fn replace_section(
        &self,
        section: SectionId,
        events: &[StoredEvent],
    ) -> anyhow::Result<()> {
        let mut transaction = self.pool.begin().await?;

        // The section is written as a whole, so whatever it listed before and
        // does not list any more goes. An event that moved to another section
        // has been claimed by that one already, and is not deleted here.
        sqlx::query(
            "DELETE FROM events \
             WHERE original_network_id = ? AND stream_id = ? AND service_id = ? \
               AND table_id = ? AND section_number = ?",
        )
        .bind(i64::from(section.original_network_id))
        .bind(i64::from(section.stream_id))
        .bind(i64::from(section.service_id))
        .bind(i64::from(section.table_id))
        .bind(i64::from(section.section_number))
        .execute(&mut *transaction)
        .await?;

        if !events.is_empty() {
            let updated_at = Utc::now().timestamp();
            let mut insert = QueryBuilder::<Sqlite>::new(format!(
                "INSERT OR REPLACE INTO events ({COLUMNS}, updated_at) "
            ));

            insert.push_values(events, |mut row, event| {
                row.push_bind(i64::from(event.service_id))
                    .push_bind(i64::from(event.event_id))
                    .push_bind(i64::from(section.original_network_id))
                    .push_bind(i64::from(section.stream_id))
                    .push_bind(i64::from(section.table_id))
                    .push_bind(i64::from(section.section_number))
                    .push_bind(event.start_time.map(to_timestamp))
                    .push_bind(event.duration.map(|duration| duration.num_seconds()))
                    .push_bind(event.language_code.clone())
                    .push_bind(event.name.clone())
                    .push_bind(event.text.clone())
                    .push_bind(encode_description(&event.description))
                    .push_bind(updated_at);
            });

            insert.build().execute(&mut *transaction).await?;
        }

        transaction.commit().await?;

        Ok(())
    }

    async fn prune_events_before(&self, at: NaiveDateTime) -> anyhow::Result<u64> {
        let result = sqlx::query("DELETE FROM events WHERE start_time < ?")
            .bind(to_timestamp(at))
            .execute(&self.pool)
            .await?;

        Ok(result.rows_affected())
    }
}

/// The wall clock the SI carries, as the seconds a database column holds.
///
/// Which zone it is read in never changes, so it round trips whatever the
/// server runs on.
fn to_timestamp(value: NaiveDateTime) -> i64 {
    value.and_utc().timestamp()
}

fn from_timestamp(value: i64) -> Option<NaiveDateTime> {
    DateTime::from_timestamp(value, 0).map(|value| value.naive_utc())
}

fn encode_description(description: &[Vec<(String, String)>]) -> String {
    serde_json::to_string(description).unwrap_or_else(|_| "[]".to_string())
}

fn read_event(row: &sqlx::sqlite::SqliteRow) -> anyhow::Result<StoredEvent> {
    let description: String = row.try_get("description")?;

    Ok(StoredEvent {
        service_id: row.try_get::<i64, _>("service_id")?.try_into()?,
        event_id: row.try_get::<i64, _>("event_id")?.try_into()?,
        start_time: row
            .try_get::<Option<i64>, _>("start_time")?
            .and_then(from_timestamp),
        duration: row
            .try_get::<Option<i64>, _>("duration_seconds")?
            .map(TimeDelta::seconds),
        language_code: row.try_get("language_code")?,
        name: row.try_get("name")?,
        text: row.try_get("text")?,
        description: serde_json::from_str(&description)?,
    })
}

#[cfg(test)]
mod tests {
    use chrono::NaiveDate;

    use super::*;

    const SECTION: SectionId = SectionId {
        original_network_id: 4,
        stream_id: 0x1234,
        service_id: 0x0400,
        table_id: 0x50,
        section_number: 0,
    };

    async fn store() -> SqliteStore {
        SqliteStore::open("sqlite::memory:").await.unwrap()
    }

    fn event(event_id: u16, name: &str, hour: u32) -> StoredEvent {
        StoredEvent {
            service_id: SECTION.service_id,
            event_id,
            start_time: NaiveDate::from_ymd_opt(2026, 7, 11)
                .unwrap()
                .and_hms_opt(hour, 0, 0),
            duration: Some(TimeDelta::minutes(30)),
            language_code: Some("jpn".to_string()),
            name: Some(name.to_string()),
            text: Some("Summary".to_string()),
            description: vec![vec![("Cast".to_string(), "Someone".to_string())]],
        }
    }

    #[tokio::test]
    async fn reads_back_every_event_of_a_section() {
        let store = store().await;
        let events = [event(0x0001, "Programme", 12), event(0x0002, "Next", 13)];

        store.replace_section(SECTION, &events).await.unwrap();

        let mut loaded = store.load_events().await.unwrap();
        loaded.sort_by_key(|event| event.event_id);

        assert_eq!(loaded, events);
    }

    #[tokio::test]
    async fn drops_the_events_a_section_stops_listing() {
        let store = store().await;
        store
            .replace_section(
                SECTION,
                &[
                    event(0x0001, "Programme", 12),
                    event(0x0002, "Cancelled", 13),
                ],
            )
            .await
            .unwrap();

        // The broadcaster revised the section, which now describes one longer
        // programme instead.
        store
            .replace_section(SECTION, &[event(0x0001, "Extended", 12)])
            .await
            .unwrap();

        let loaded = store.load_events().await.unwrap();

        assert_eq!(loaded, [event(0x0001, "Extended", 12)]);
    }

    #[tokio::test]
    async fn keeps_the_events_of_the_other_sections() {
        let store = store().await;
        let other = SectionId {
            section_number: 1,
            ..SECTION
        };
        store
            .replace_section(SECTION, &[event(0x0001, "Programme", 12)])
            .await
            .unwrap();
        store
            .replace_section(other, &[event(0x0002, "Later", 15)])
            .await
            .unwrap();

        store.replace_section(SECTION, &[]).await.unwrap();

        let loaded = store.load_events().await.unwrap();

        assert_eq!(loaded, [event(0x0002, "Later", 15)]);
    }

    #[tokio::test]
    async fn keeps_the_schedule_across_reopening() {
        let directory = tempfile::tempdir().unwrap();
        let url = format!("sqlite://{}", directory.path().join("chibitv.db").display());

        {
            let store = SqliteStore::open(&url).await.unwrap();
            store
                .replace_section(SECTION, &[event(0x0001, "Programme", 12)])
                .await
                .unwrap();
        }

        // Opening it again migrates a schema that is already there, and finds
        // what the previous run wrote.
        let store = SqliteStore::open(&url).await.unwrap();

        assert_eq!(
            store.load_events().await.unwrap(),
            [event(0x0001, "Programme", 12)]
        );
    }

    #[tokio::test]
    async fn prunes_what_has_been_broadcast_already() {
        let store = store().await;
        store
            .replace_section(
                SECTION,
                &[event(0x0001, "Over", 12), event(0x0002, "Upcoming", 15)],
            )
            .await
            .unwrap();

        let pruned = store
            .prune_events_before(
                NaiveDate::from_ymd_opt(2026, 7, 11)
                    .unwrap()
                    .and_hms_opt(14, 0, 0)
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(pruned, 1);
        assert_eq!(
            store.load_events().await.unwrap(),
            [event(0x0002, "Upcoming", 15)]
        );
    }
}
