-- The channels chibitv serves, which `config.toml` used to name.
--
-- A row says how the channel is tuned to and what it delivers once tuned:
-- `delivery_system` is the broadcast it is carried on, which decides how the
-- stream is demultiplexed, and `tuning` says whether the tuning parameters are
-- here or held by a BonDriver, which enumerates its channels itself. The
-- columns of the tuning the row does not use are left null.
--
-- The identifier is never reused, so one a client remembers cannot come back
-- naming another channel once a scan has replaced it.
CREATE TABLE channels (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    name TEXT NOT NULL,
    -- `ISDB-T`, `ISDB-S` or `ISDB-S3`.
    delivery_system TEXT NOT NULL,
    -- `parameters` for a channel tuned by the parameters below, `bondriver`
    -- for one named by the numbers a BonDriver enumerates.
    tuning TEXT NOT NULL,
    -- Hz on ISDB-T, kHz on the satellite systems.
    frequency INTEGER,
    bandwidth_hz INTEGER,
    -- The transport stream id on ISDB-S, the TLV stream id on ISDB-S3, which
    -- is what picks the stream out of its transponder.
    stream_id INTEGER,
    -- The tuning space and channel numbers of a BonDriver channel.
    space INTEGER,
    channel_number INTEGER,
    -- The stream the services of the channel are named under, as a scan wrote
    -- it.
    transport_stream_id INTEGER
);

-- A scan replaces the channels of the broadcast it walked.
CREATE INDEX channels_by_delivery_system ON channels (delivery_system);

-- The service catalog of a channel, which the registry is seeded with while
-- starting up so that the services are known before anything is tuned.
CREATE TABLE channel_services (
    channel_id INTEGER NOT NULL REFERENCES channels (id) ON DELETE CASCADE,
    service_id INTEGER NOT NULL,
    name TEXT NOT NULL,
    provider_name TEXT NOT NULL,
    PRIMARY KEY (channel_id, service_id)
) WITHOUT ROWID;
