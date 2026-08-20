-- The broadcast schedule, one row per programme.
--
-- Events are identified the way the registry identifies them, by the service
-- carrying them and their event id. The section columns record which EIT
-- section delivered a row, so that the whole section can be replaced when the
-- broadcaster revises it.
CREATE TABLE events (
    service_id INTEGER NOT NULL,
    event_id INTEGER NOT NULL,
    original_network_id INTEGER NOT NULL,
    stream_id INTEGER NOT NULL,
    table_id INTEGER NOT NULL,
    section_number INTEGER NOT NULL,
    -- Seconds of the wall clock the SI carries, which is JST.
    start_time INTEGER,
    duration_seconds INTEGER,
    language_code TEXT,
    name TEXT,
    text TEXT,
    -- The detailed description, as the JSON encoding of the extended event
    -- descriptors it was assembled from.
    description TEXT NOT NULL,
    updated_at INTEGER NOT NULL,
    PRIMARY KEY (service_id, event_id)
) WITHOUT ROWID;

-- Reading a programme guide asks for one service over a range of time.
CREATE INDEX events_by_service_and_start_time ON events (service_id, start_time);

-- Replacing a section and pruning what has been broadcast already both delete
-- a whole range of rows at once.
CREATE INDEX events_by_section ON events (
    original_network_id,
    stream_id,
    service_id,
    table_id,
    section_number
);
CREATE INDEX events_by_start_time ON events (start_time);
