-- Events are identified by the service carrying them, which is a stream and a
-- service of it rather than a service alone: BS 2K and BS 4K number their
-- services alike, so one would otherwise replace the other.
--
-- The rows already record the stream that delivered them, so the schedule is
-- carried over rather than crawled again.
ALTER TABLE events RENAME TO events_by_service_id;

CREATE TABLE events (
    stream_id INTEGER NOT NULL,
    service_id INTEGER NOT NULL,
    event_id INTEGER NOT NULL,
    original_network_id INTEGER NOT NULL,
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
    PRIMARY KEY (stream_id, service_id, event_id)
) WITHOUT ROWID;

INSERT INTO events (
    stream_id,
    service_id,
    event_id,
    original_network_id,
    table_id,
    section_number,
    start_time,
    duration_seconds,
    language_code,
    name,
    text,
    description,
    updated_at
)
SELECT
    stream_id,
    service_id,
    event_id,
    original_network_id,
    table_id,
    section_number,
    start_time,
    duration_seconds,
    language_code,
    name,
    text,
    description,
    updated_at
FROM events_by_service_id;

-- The indexes of the old table go with it, which is what frees their names.
DROP TABLE events_by_service_id;

-- Reading a programme guide asks for one service over a range of time.
CREATE INDEX events_by_service_and_start_time ON events (stream_id, service_id, start_time);

-- Replacing a section deletes every row it delivered at once.
CREATE INDEX events_by_section ON events (
    original_network_id,
    stream_id,
    service_id,
    table_id,
    section_number
);
