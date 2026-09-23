-- The services on air, which a scan and the SDT describe.
--
-- A service is kept under the stream carrying it rather than under a channel,
-- so that what the SDT says of it can be kept without working out which
-- channel that is first. The channel it belongs to is the one carrying its
-- stream, which is for the server to work out out of the channels.
CREATE TABLE services (
    -- The TLV stream id on ISDB-S3, the transport stream id on ISDB-T and
    -- ISDB-S.
    stream_id INTEGER NOT NULL,
    service_id INTEGER NOT NULL,
    name TEXT NOT NULL,
    provider_name TEXT NOT NULL,
    PRIMARY KEY (stream_id, service_id)
) WITHOUT ROWID;

-- The catalogs a scan wrote are carried over, under the stream of the channel
-- they were written for.
INSERT OR REPLACE INTO services (stream_id, service_id, name, provider_name)
SELECT
    COALESCE(channels.transport_stream_id, channels.stream_id),
    channel_services.service_id,
    channel_services.name,
    channel_services.provider_name
FROM channel_services
JOIN channels ON channels.id = channel_services.channel_id
WHERE COALESCE(channels.transport_stream_id, channels.stream_id) IS NOT NULL;

DROP TABLE channel_services;

