CREATE TABLE logos (
    stream_id INTEGER NOT NULL,
    service_id INTEGER NOT NULL,
    png BLOB NOT NULL,
    PRIMARY KEY (stream_id, service_id)
);
