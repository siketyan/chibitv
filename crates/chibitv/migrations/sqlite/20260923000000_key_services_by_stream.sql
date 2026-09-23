-- The services on air, which a scan and the SDT describe.
--
-- A service is kept under the stream carrying it rather than under a channel,
-- so that what the SDT says of it can be kept without working out which
-- channel that is first. The channel it belongs to is the one carrying its
-- stream, which `served_services` works out when it is read.
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

-- The services of the channels being served, each under the channel carrying
-- its stream.
--
-- The stream a channel carries is the one a scan recorded, or failing that the
-- one its tuning parameters pick out of a transponder. Two channels carrying
-- the same stream — a relay station on another frequency — share its services,
-- which go with the first of them.
CREATE VIEW served_services AS
SELECT
    services.stream_id,
    services.service_id,
    services.name,
    services.provider_name,
    MIN(channels.id) AS channel_id
FROM services
JOIN channels ON COALESCE(channels.transport_stream_id, channels.stream_id) = services.stream_id
GROUP BY services.stream_id, services.service_id;
