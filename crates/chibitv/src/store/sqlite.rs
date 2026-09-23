use std::str::FromStr;
use std::time::Duration;

use anyhow::bail;
use async_trait::async_trait;
use chrono::{DateTime, NaiveDateTime, TimeDelta, Utc};
use sqlx::sqlite::{
    SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteRow, SqliteSynchronous,
};
use sqlx::{QueryBuilder, Row, Sqlite, SqlitePool};

use crate::channel::{ChannelInner, DeliverySystem};
use crate::event::Event;
use crate::service::{Service, ServiceKey, StoredService};

use super::{
    ChannelRepository, EventRepository, NewChannel, SectionId, ServiceRepository, Store,
    StoredChannel,
};

/// How long a statement waits for the database to be free again.
const BUSY_TIMEOUT: Duration = Duration::from_secs(5);

/// The columns of an event, in the order they are written in.
const COLUMNS: &str = "stream_id, service_id, event_id, original_network_id, table_id, \
                       section_number, start_time, duration_seconds, language_code, name, text, \
                       description";

/// Reading them back goes by name, so this needs to name them all rather than
/// keep the order above.
macro_rules! select_events {
    () => {
        "SELECT stream_id, service_id, event_id, start_time, duration_seconds, language_code, \
         name, text, description FROM events"
    };
}

/// The channels in the order they are served in, which is the order they were
/// stored in.
const SELECT_CHANNELS: &str = "SELECT id, name, delivery_system, tuning, frequency, bandwidth_hz, \
                               stream_id, space, channel_number, transport_stream_id FROM \
                               channels ORDER BY id";

/// The service catalogs of every channel, read alongside the channels.
const SELECT_CHANNEL_SERVICES: &str = "SELECT channels.id AS channel_id, services.service_id, \
                                       services.name, services.provider_name FROM channels JOIN \
                                       services ON services.stream_id = \
                                       COALESCE(channels.transport_stream_id, channels.stream_id) \
                                       ORDER BY channels.id, services.service_id";

/// The services of the channels being served.
macro_rules! select_services {
    () => {
        "SELECT stream_id, service_id, name, provider_name, channel_id FROM served_services"
    };
}

/// A channel tuned by the parameters the row carries.
const TUNING_PARAMETERS: &str = "parameters";

/// A channel named by the numbers a BonDriver enumerates, which holds the
/// tuning parameters itself.
const TUNING_BONDRIVER: &str = "bondriver";

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
            // The service catalog of a channel goes with the channel, which is
            // what the cascade of its foreign key does.
            .foreign_keys(true)
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
impl EventRepository for SqliteStore {
    async fn find_events(&self, key: ServiceKey) -> anyhow::Result<Vec<Event>> {
        let rows = sqlx::query(concat!(
            select_events!(),
            " WHERE stream_id = ? AND service_id = ? ORDER BY start_time, event_id"
        ))
        .bind(i64::from(key.stream_id))
        .bind(i64::from(key.service_id))
        .fetch_all(&self.pool)
        .await?;

        rows.iter().map(read_event).collect()
    }

    async fn find_event(&self, key: ServiceKey, event_id: u16) -> anyhow::Result<Option<Event>> {
        sqlx::query(concat!(
            select_events!(),
            " WHERE stream_id = ? AND service_id = ? AND event_id = ?"
        ))
        .bind(i64::from(key.stream_id))
        .bind(i64::from(key.service_id))
        .bind(i64::from(event_id))
        .fetch_optional(&self.pool)
        .await?
        .as_ref()
        .map(read_event)
        .transpose()
    }

    async fn replace_section(&self, section: SectionId, events: &[Event]) -> anyhow::Result<()> {
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

        // What the section lists now is claimed by it, even when another one
        // delivered it before.
        if let Some(mut insert) = insert_events("INSERT OR REPLACE", section, events, "") {
            insert.build().execute(&mut *transaction).await?;
        }

        transaction.commit().await?;

        Ok(())
    }

    async fn save_events(&self, section: SectionId, events: &[Event]) -> anyhow::Result<()> {
        // An event already kept stays claimed by the section that delivered
        // it, which is what replacing that section goes by.
        let Some(mut insert) = insert_events(
            "INSERT",
            section,
            events,
            " ON CONFLICT (stream_id, service_id, event_id) DO UPDATE SET \
             start_time = excluded.start_time, duration_seconds = excluded.duration_seconds, \
             language_code = excluded.language_code, name = excluded.name, \
             text = excluded.text, description = excluded.description, \
             updated_at = excluded.updated_at",
        ) else {
            return Ok(());
        };

        insert.build().execute(&self.pool).await?;

        Ok(())
    }
}

/// Writes the events a section delivered with the statement `insert` names,
/// doing what `on_conflict` says with one kept already, or nothing when there
/// are none.
fn insert_events(
    insert: &str,
    section: SectionId,
    events: &[Event],
    on_conflict: &str,
) -> Option<QueryBuilder<Sqlite>> {
    if events.is_empty() {
        return None;
    }

    let updated_at = Utc::now().timestamp();
    let mut insert =
        QueryBuilder::<Sqlite>::new(format!("{insert} INTO events ({COLUMNS}, updated_at) "));

    insert.push_values(events, |mut row, event| {
        row.push_bind(i64::from(event.key.stream_id))
            .push_bind(i64::from(event.key.service_id))
            .push_bind(i64::from(event.id))
            .push_bind(i64::from(section.original_network_id))
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
    insert.push(on_conflict);

    Some(insert)
}

#[async_trait]
impl ServiceRepository for SqliteStore {
    async fn find_services(&self) -> anyhow::Result<Vec<Service>> {
        let rows = sqlx::query(concat!(
            select_services!(),
            " ORDER BY stream_id, service_id"
        ))
        .fetch_all(&self.pool)
        .await?;

        rows.iter().map(read_service).collect()
    }

    async fn find_service(&self, key: ServiceKey) -> anyhow::Result<Option<Service>> {
        sqlx::query(concat!(
            select_services!(),
            " WHERE stream_id = ? AND service_id = ?"
        ))
        .bind(i64::from(key.stream_id))
        .bind(i64::from(key.service_id))
        .fetch_optional(&self.pool)
        .await?
        .as_ref()
        .map(read_service)
        .transpose()
    }

    async fn save_services(
        &self,
        stream_id: u16,
        services: &[StoredService],
    ) -> anyhow::Result<()> {
        let mut transaction = self.pool.begin().await?;
        insert_services(&mut transaction, stream_id, services).await?;
        transaction.commit().await?;

        Ok(())
    }
}

#[async_trait]
impl ChannelRepository for SqliteStore {
    async fn load_channels(&self) -> anyhow::Result<Vec<StoredChannel>> {
        let mut channels = sqlx::query(SELECT_CHANNELS)
            .fetch_all(&self.pool)
            .await?
            .iter()
            .map(read_channel)
            .collect::<anyhow::Result<Vec<_>>>()?;

        // The catalogs are read in one statement rather than one per channel,
        // and handed to the channel each row names.
        for row in sqlx::query(SELECT_CHANNEL_SERVICES)
            .fetch_all(&self.pool)
            .await?
        {
            let channel_id = usize::try_from(row.try_get::<i64, _>("channel_id")?)?;
            let Some(channel) = channels.iter_mut().find(|channel| channel.id == channel_id) else {
                continue;
            };

            channel.services.push(StoredService {
                id: row.try_get::<i64, _>("service_id")?.try_into()?,
                name: row.try_get("name")?,
                provider_name: row.try_get("provider_name")?,
            });
        }

        Ok(channels)
    }

    async fn create_channels(&self, channels: &[NewChannel]) -> anyhow::Result<()> {
        let mut transaction = self.pool.begin().await?;

        // A channel is told apart by the tuning it is reached with, so keeping
        // one that is already kept writes over it rather than beside it.
        let kept = sqlx::query(SELECT_CHANNELS)
            .fetch_all(&mut *transaction)
            .await?
            .iter()
            .map(read_channel)
            .collect::<anyhow::Result<Vec<_>>>()?;

        for channel in channels {
            match kept.iter().find(|kept| kept.inner == channel.inner) {
                Some(kept) => {
                    let id = i64::try_from(kept.id)?;
                    sqlx::query(
                        "UPDATE channels SET name = ?, transport_stream_id = ? WHERE id = ?",
                    )
                    .bind(channel.name.as_str())
                    .bind(channel.transport_stream_id.map(i64::from))
                    .bind(id)
                    .execute(&mut *transaction)
                    .await?;
                }
                None => {
                    insert_channel(&mut transaction, channel).await?;
                }
            }

            replace_catalog(&mut transaction, channel).await?;
        }

        transaction.commit().await?;

        Ok(())
    }

    async fn replace_channels(
        &self,
        delivery_system: DeliverySystem,
        channels: &[NewChannel],
    ) -> anyhow::Result<()> {
        let mut transaction = self.pool.begin().await?;

        sqlx::query("DELETE FROM channels WHERE delivery_system = ?")
            .bind(delivery_system.as_str())
            .execute(&mut *transaction)
            .await?;

        for channel in channels {
            insert_channel(&mut transaction, channel).await?;
            replace_catalog(&mut transaction, channel).await?;
        }

        transaction.commit().await?;

        Ok(())
    }
}

/// Writes a channel that is not kept yet.
async fn insert_channel(
    transaction: &mut sqlx::Transaction<'_, Sqlite>,
    channel: &NewChannel,
) -> anyhow::Result<()> {
    let tuning = Tuning::of(&channel.inner);
    sqlx::query(
        "INSERT INTO channels (name, delivery_system, tuning, frequency, bandwidth_hz, \
         stream_id, space, channel_number, transport_stream_id) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(channel.name.as_str())
    .bind(tuning.delivery_system.as_str())
    .bind(tuning.tuning)
    .bind(tuning.frequency)
    .bind(tuning.bandwidth_hz)
    .bind(tuning.stream_id)
    .bind(tuning.space)
    .bind(tuning.channel_number)
    .bind(channel.transport_stream_id.map(i64::from))
    .execute(&mut **transaction)
    .await?;

    Ok(())
}

/// Writes the service catalog a scan found on the channel, under the stream it
/// carries.
///
/// The catalog is written as a whole, so a service the stream no longer
/// carries goes. A channel whose stream is not known has nowhere to keep one.
async fn replace_catalog(
    transaction: &mut sqlx::Transaction<'_, Sqlite>,
    channel: &NewChannel,
) -> anyhow::Result<()> {
    let Some(stream_id) = channel.stream_id() else {
        return Ok(());
    };

    sqlx::query("DELETE FROM services WHERE stream_id = ?")
        .bind(i64::from(stream_id))
        .execute(&mut **transaction)
        .await?;

    insert_services(transaction, stream_id, &channel.services).await
}

async fn insert_services(
    transaction: &mut sqlx::Transaction<'_, Sqlite>,
    stream_id: u16,
    services: &[StoredService],
) -> anyhow::Result<()> {
    for service in services {
        sqlx::query(
            "INSERT OR REPLACE INTO services (stream_id, service_id, name, provider_name) \
             VALUES (?, ?, ?, ?)",
        )
        .bind(i64::from(stream_id))
        .bind(i64::from(service.id))
        .bind(service.name.as_str())
        .bind(service.provider_name.as_str())
        .execute(&mut **transaction)
        .await?;
    }

    Ok(())
}

/// The tuning of a channel, as the columns of its row.
///
/// Which columns are used is what `tuning` says: a channel a BonDriver tunes
/// carries the numbers it enumerates instead of tuning parameters, so the
/// columns of the other tuning are left null.
struct Tuning {
    delivery_system: DeliverySystem,
    tuning: &'static str,
    frequency: Option<i64>,
    bandwidth_hz: Option<i64>,
    stream_id: Option<i64>,
    space: Option<i64>,
    channel_number: Option<i64>,
}

impl Tuning {
    fn of(inner: &ChannelInner) -> Self {
        let mut tuning = Self {
            delivery_system: inner.delivery_system(),
            tuning: TUNING_PARAMETERS,
            frequency: None,
            bandwidth_hz: None,
            stream_id: None,
            space: None,
            channel_number: None,
        };

        match *inner {
            ChannelInner::IsdbT {
                frequency,
                bandwidth_hz,
            } => {
                tuning.frequency = Some(i64::from(frequency));
                tuning.bandwidth_hz = Some(i64::from(bandwidth_hz));
            }
            ChannelInner::IsdbS {
                frequency,
                stream_id,
            }
            | ChannelInner::IsdbS3 {
                frequency,
                stream_id,
            } => {
                tuning.frequency = Some(i64::from(frequency));
                tuning.stream_id = Some(i64::from(stream_id));
            }
            ChannelInner::BonIsdbT { space, channel }
            | ChannelInner::BonIsdbS { space, channel }
            | ChannelInner::BonIsdbS3 { space, channel } => {
                tuning.tuning = TUNING_BONDRIVER;
                tuning.space = Some(i64::from(space));
                tuning.channel_number = Some(i64::from(channel));
            }
        }

        tuning
    }
}

fn read_channel(row: &SqliteRow) -> anyhow::Result<StoredChannel> {
    let delivery_system = DeliverySystem::parse(row.try_get("delivery_system")?)?;
    let tuning: String = row.try_get("tuning")?;
    let inner = match (tuning.as_str(), delivery_system) {
        (TUNING_PARAMETERS, DeliverySystem::IsdbT) => ChannelInner::IsdbT {
            frequency: tuning_column(row, "frequency")?,
            bandwidth_hz: tuning_column(row, "bandwidth_hz")?,
        },
        (TUNING_PARAMETERS, DeliverySystem::IsdbS) => ChannelInner::IsdbS {
            frequency: tuning_column(row, "frequency")?,
            stream_id: tuning_column(row, "stream_id")?,
        },
        (TUNING_PARAMETERS, DeliverySystem::IsdbS3) => ChannelInner::IsdbS3 {
            frequency: tuning_column(row, "frequency")?,
            stream_id: tuning_column(row, "stream_id")?,
        },
        (TUNING_BONDRIVER, DeliverySystem::IsdbT) => ChannelInner::BonIsdbT {
            space: tuning_column(row, "space")?,
            channel: tuning_column(row, "channel_number")?,
        },
        (TUNING_BONDRIVER, DeliverySystem::IsdbS) => ChannelInner::BonIsdbS {
            space: tuning_column(row, "space")?,
            channel: tuning_column(row, "channel_number")?,
        },
        (TUNING_BONDRIVER, DeliverySystem::IsdbS3) => ChannelInner::BonIsdbS3 {
            space: tuning_column(row, "space")?,
            channel: tuning_column(row, "channel_number")?,
        },
        (tuning, _) => bail!("`{tuning}` is not a way of tuning chibitv knows"),
    };

    Ok(StoredChannel {
        id: usize::try_from(row.try_get::<i64, _>("id")?)?,
        name: row.try_get("name")?,
        inner,
        transport_stream_id: row
            .try_get::<Option<i64>, _>("transport_stream_id")?
            .map(u16::try_from)
            .transpose()?,
        services: vec![],
    })
}

/// One of the tuning columns, which the tuning the row names has to carry.
fn tuning_column(row: &SqliteRow, column: &str) -> anyhow::Result<u32> {
    let Some(value) = row.try_get::<Option<i64>, _>(column)? else {
        bail!("the channel is stored without its `{column}`");
    };

    Ok(value.try_into()?)
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

fn read_key(row: &SqliteRow) -> anyhow::Result<ServiceKey> {
    Ok(ServiceKey {
        stream_id: row.try_get::<i64, _>("stream_id")?.try_into()?,
        service_id: row.try_get::<i64, _>("service_id")?.try_into()?,
    })
}

fn read_service(row: &SqliteRow) -> anyhow::Result<Service> {
    Ok(Service {
        key: read_key(row)?,
        name: row.try_get("name")?,
        provider_name: row.try_get("provider_name")?,
        channel_id: usize::try_from(row.try_get::<i64, _>("channel_id")?)?,
    })
}

fn read_event(row: &SqliteRow) -> anyhow::Result<Event> {
    let description: String = row.try_get("description")?;

    Ok(Event {
        key: read_key(row)?,
        id: row.try_get::<i64, _>("event_id")?.try_into()?,
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

    fn event(event_id: u16, name: &str, hour: u32) -> Event {
        Event {
            key: SECTION.service(),
            id: event_id,
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

    fn new_channel(name: &str, inner: ChannelInner, stream_id: Option<u16>) -> NewChannel {
        NewChannel {
            name: name.to_string(),
            inner,
            transport_stream_id: stream_id,
            services: vec![StoredService {
                id: 0x0400,
                name: "Service".to_string(),
                provider_name: "Provider".to_string(),
            }],
        }
    }

    #[tokio::test]
    async fn reads_back_every_channel_with_its_tuning_and_services() {
        let store = store().await;
        let channels = [
            new_channel(
                "UHF 20",
                ChannelInner::IsdbT {
                    frequency: 515_142_857,
                    bandwidth_hz: 6_000_000,
                },
                Some(0x1234),
            ),
            new_channel(
                "BS 4K",
                ChannelInner::IsdbS3 {
                    frequency: 1_318_000,
                    stream_id: 0x40F1,
                },
                Some(0x40F1),
            ),
            new_channel(
                "BonDriver BS",
                ChannelInner::BonIsdbS {
                    space: 1,
                    channel: 2,
                },
                None,
            ),
        ];

        for channel in &channels {
            store
                .replace_channels(
                    channel.inner.delivery_system(),
                    std::slice::from_ref(channel),
                )
                .await
                .unwrap();
        }
        let stored = store.load_channels().await.unwrap();

        assert_eq!(
            stored
                .iter()
                .map(|channel| (channel.name.as_str(), channel.inner.clone()))
                .collect::<Vec<_>>(),
            channels
                .iter()
                .map(|channel| (channel.name.as_str(), channel.inner.clone()))
                .collect::<Vec<_>>(),
        );
        assert_eq!(stored[0].transport_stream_id, Some(0x1234));
        assert_eq!(stored[2].transport_stream_id, None);
        assert_eq!(stored[1].services, channels[1].services);
    }

    #[tokio::test]
    async fn keeps_a_channel_tuned_the_same_way_once() {
        let store = store().await;
        let tuning = ChannelInner::IsdbT {
            frequency: 515_142_857,
            bandwidth_hz: 6_000_000,
        };
        store
            .create_channels(&[new_channel("UHF 20", tuning.clone(), Some(0x1234))])
            .await
            .unwrap();
        let first = store.load_channels().await.unwrap();

        // The same channel under another name, as someone who edited what a
        // scan found would hand it back.
        let mut renamed = new_channel("TOKYO MX", tuning, Some(0x1234));
        renamed.services.clear();
        store.create_channels(&[renamed]).await.unwrap();

        let kept = store.load_channels().await.unwrap();

        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].id, first[0].id);
        assert_eq!(kept[0].name, "TOKYO MX");
        // The catalog is written as a whole, so a service that came with the
        // channel before and does not now is gone.
        assert!(kept[0].services.is_empty());
    }

    #[tokio::test]
    async fn keeps_a_channel_beside_the_ones_already_kept() {
        let store = store().await;
        store
            .create_channels(&[new_channel(
                "UHF 20",
                ChannelInner::IsdbT {
                    frequency: 515_142_857,
                    bandwidth_hz: 6_000_000,
                },
                Some(0x1234),
            )])
            .await
            .unwrap();

        store
            .create_channels(&[new_channel(
                "BS",
                ChannelInner::IsdbS {
                    frequency: 1_049_480,
                    stream_id: 0x4031,
                },
                Some(0x4031),
            )])
            .await
            .unwrap();

        assert_eq!(
            store
                .load_channels()
                .await
                .unwrap()
                .iter()
                .map(|channel| channel.name.clone())
                .collect::<Vec<_>>(),
            ["UHF 20", "BS"],
        );
    }

    #[tokio::test]
    async fn replaces_the_channels_of_one_broadcast_only() {
        let store = store().await;
        store
            .replace_channels(
                DeliverySystem::IsdbT,
                &[new_channel(
                    "UHF 20",
                    ChannelInner::IsdbT {
                        frequency: 515_142_857,
                        bandwidth_hz: 6_000_000,
                    },
                    Some(0x1234),
                )],
            )
            .await
            .unwrap();
        store
            .replace_channels(
                DeliverySystem::IsdbS,
                &[new_channel(
                    "BS",
                    ChannelInner::IsdbS {
                        frequency: 1_049_480,
                        stream_id: 0x4031,
                    },
                    Some(0x4031),
                )],
            )
            .await
            .unwrap();

        // A terrestrial scan says nothing about the satellite channels.
        store
            .replace_channels(
                DeliverySystem::IsdbT,
                &[new_channel(
                    "UHF 21",
                    ChannelInner::IsdbT {
                        frequency: 521_142_857,
                        bandwidth_hz: 6_000_000,
                    },
                    Some(0x5678),
                )],
            )
            .await
            .unwrap();

        let stored = store.load_channels().await.unwrap();

        assert_eq!(
            stored
                .iter()
                .map(|channel| channel.name.as_str())
                .collect::<Vec<_>>(),
            ["BS", "UHF 21"],
        );
        // The services of a channel that was replaced go with it, and the
        // identifier it had is not given to another channel.
        assert_eq!(stored[1].services.len(), 1);
        assert!(stored[1].id > stored[0].id);
    }

    #[tokio::test]
    async fn reads_back_every_event_of_a_section() {
        let store = store().await;
        let events = [event(0x0001, "Programme", 12), event(0x0002, "Next", 13)];

        store.replace_section(SECTION, &events).await.unwrap();

        assert_eq!(store.find_events(SECTION.service()).await.unwrap(), events);
        assert_eq!(
            store.find_event(SECTION.service(), 0x0002).await.unwrap(),
            Some(event(0x0002, "Next", 13))
        );
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

        let loaded = store.find_events(SECTION.service()).await.unwrap();

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

        let loaded = store.find_events(SECTION.service()).await.unwrap();

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
            store.find_events(SECTION.service()).await.unwrap(),
            [event(0x0001, "Programme", 12)]
        );
    }

    #[tokio::test]
    async fn keeps_what_was_on_air_once_the_next_programme_starts() {
        let store = store().await;
        let present = SectionId {
            table_id: 0x4E,
            ..SECTION
        };
        store
            .save_events(present, &[event(0x0001, "Programme", 12)])
            .await
            .unwrap();

        // The present event is the next one now, and the one that ended is
        // still part of the schedule.
        store
            .save_events(present, &[event(0x0002, "Next", 13)])
            .await
            .unwrap();

        assert_eq!(
            store.find_events(SECTION.service()).await.unwrap(),
            [event(0x0001, "Programme", 12), event(0x0002, "Next", 13)]
        );
    }

    #[tokio::test]
    async fn leaves_an_event_to_the_schedule_that_listed_it() {
        let store = store().await;
        store
            .replace_section(SECTION, &[event(0x0001, "Programme", 12)])
            .await
            .unwrap();

        // The present event is described again, which does not take it out of
        // the section of the schedule listing it.
        let present = SectionId {
            table_id: 0x4E,
            ..SECTION
        };
        store
            .save_events(present, &[event(0x0001, "On air", 12)])
            .await
            .unwrap();
        store.replace_section(SECTION, &[]).await.unwrap();

        assert!(
            store
                .find_events(SECTION.service())
                .await
                .unwrap()
                .is_empty()
        );
    }

    fn catalog(id: u16, name: &str) -> Vec<StoredService> {
        vec![StoredService {
            id,
            name: name.to_string(),
            provider_name: "Provider".to_string(),
        }]
    }

    fn terrestrial_channel(
        frequency: u32,
        stream_id: u16,
        services: Vec<StoredService>,
    ) -> NewChannel {
        NewChannel {
            name: format!("Channel {stream_id}"),
            inner: ChannelInner::IsdbT {
                frequency,
                bandwidth_hz: 6_000_000,
            },
            transport_stream_id: Some(stream_id),
            services,
        }
    }

    #[tokio::test]
    async fn serves_the_services_of_every_channel_kept() {
        let store = store().await;
        store
            .replace_channels(
                DeliverySystem::IsdbT,
                &[
                    terrestrial_channel(515_142_857, 100, catalog(101, "Service A")),
                    terrestrial_channel(521_142_857, 200, catalog(201, "Service B")),
                ],
            )
            .await
            .unwrap();
        let channels = store.load_channels().await.unwrap();

        assert_eq!(
            store
                .find_services()
                .await
                .unwrap()
                .iter()
                .map(|service| (service.key.service_id, service.channel_id))
                .collect::<Vec<_>>(),
            [(101, channels[0].id), (201, channels[1].id)],
        );
    }

    #[tokio::test]
    async fn forgets_the_services_of_a_channel_no_longer_served() {
        let store = store().await;
        store
            .replace_channels(
                DeliverySystem::IsdbT,
                &[
                    terrestrial_channel(515_142_857, 100, catalog(101, "Service A")),
                    terrestrial_channel(521_142_857, 200, catalog(201, "Service B")),
                ],
            )
            .await
            .unwrap();

        // A scan that no longer finds the second channel replaces the list
        // with the first one alone.
        store
            .replace_channels(
                DeliverySystem::IsdbT,
                &[terrestrial_channel(
                    515_142_857,
                    100,
                    catalog(101, "Renamed"),
                )],
            )
            .await
            .unwrap();

        assert_eq!(
            store
                .find_services()
                .await
                .unwrap()
                .iter()
                .map(|service| service.name.as_str())
                .collect::<Vec<_>>(),
            ["Renamed"],
        );
    }

    #[tokio::test]
    async fn keeps_a_service_under_the_channel_carrying_its_stream() {
        let store = store().await;
        store
            .replace_channels(
                DeliverySystem::IsdbT,
                &[
                    terrestrial_channel(515_142_857, 100, vec![]),
                    terrestrial_channel(521_142_857, 200, vec![]),
                ],
            )
            .await
            .unwrap();
        let channels = store.load_channels().await.unwrap();

        // The signalling of a network describes every stream of it, including
        // the ones no channel is served from.
        store
            .save_services(200, &catalog(202, "Another"))
            .await
            .unwrap();
        store
            .save_services(300, &catalog(301, "Elsewhere"))
            .await
            .unwrap();

        let key = |stream_id, service_id| ServiceKey {
            stream_id,
            service_id,
        };
        assert_eq!(
            store
                .find_service(key(200, 202))
                .await
                .unwrap()
                .map(|service| service.channel_id),
            Some(channels[1].id),
        );
        assert_eq!(store.find_service(key(300, 301)).await.unwrap(), None);
    }

    #[tokio::test]
    async fn tells_the_services_of_two_streams_apart() {
        // BS 2K and BS 4K both carry a service numbered 101, on streams of
        // their own.
        let store = store().await;
        store
            .create_channels(&[
                new_channel(
                    "BS",
                    ChannelInner::IsdbS {
                        frequency: 1_049_480,
                        stream_id: 0x40F1,
                    },
                    None,
                ),
                new_channel(
                    "BS 4K",
                    ChannelInner::IsdbS3 {
                        frequency: 1_318_000,
                        stream_id: 0xB071,
                    },
                    None,
                ),
            ])
            .await
            .unwrap();
        store
            .save_services(0x40F1, &catalog(101, "BS Channel"))
            .await
            .unwrap();
        store
            .save_services(0xB071, &catalog(101, "BS 4K Channel"))
            .await
            .unwrap();

        let name_of = async |stream_id| {
            store
                .find_service(ServiceKey {
                    stream_id,
                    service_id: 101,
                })
                .await
                .unwrap()
                .map(|service| service.name)
        };
        assert_eq!(name_of(0x40F1).await.as_deref(), Some("BS Channel"));
        assert_eq!(name_of(0xB071).await.as_deref(), Some("BS 4K Channel"));
    }
}
