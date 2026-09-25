# Configuration

Every subcommand loads `./config.toml` from the working directory it is started
in. There is no option to point it elsewhere, so run chibitv from the directory
holding the file. Copy the template to start from:

```shell
cp config.toml.example config.toml
```

The file is read into the types in `crates/chibitv/src/config.rs`. Only
[`[cas]`](#cas) is required; every other table has a default, although
[tunelithd](#tunelith) has to be running before anything can be tuned.

The [channels](#channels) are not part of it: they are kept in the
[`[database]`](#database), which a [scan](./cli#scan) writes.

## `[cas]`

The conditional access module.

| Key          | Type   | Default    | Description                                          |
| ------------ | ------ | ---------- | ---------------------------------------------------- |
| `master_key` | string | _required_ | _Kd_, as 64 hexadecimal digits (32 bytes), unquoted. |

_Kd_ is the master key defined in Section 1.4 of ARIB STD-B61. It is not
distributed with chibitv and has to be supplied. It is decoded with `hex`, so
the string has to be exactly 32 bytes' worth of hexadecimal digits; anything
else fails to load the configuration.

```toml
[cas]
master_key = "0123456789ABCDEF0123456789ABCDEF0123456789ABCDEF0123456789ABCDEF"
```

The key is read by the ISDB-S3 (B61) descrambler only. ISDB-T and ISDB-S
descrambling (B25) derives its keys from the card alone, so a setup without a
4K channel still needs the key to be present, but never uses its value.

## `[tunelith]`

tunelithd, the daemon of [Tunelith](https://github.com/siketyan/tunelith),
which holds the tuners and shares them among programs: PLEX, Digibest and
e-better tuners over USB, the PT4K, and any tuner with a Linux DVB driver. Its
[README](https://github.com/siketyan/tunelith#readme) covers installing and
running it.

| Key      | Type    | Default     | Description                                                               |
| -------- | ------- | ----------- | ------------------------------------------------------------------------- |
| `socket` | string  | _see below_ | Path to the socket of tunelithd.                                          |
| `lnb`    | boolean | `false`     | Whether to power the dish's converter while a satellite channel is tuned. |

Without `socket`, chibitv connects to the socket of the user's tunelithd,
`$XDG_RUNTIME_DIR/tunelith/tunelithd.sock`, if there is one, and to that of
the system's, `/run/tunelith/tunelithd.sock`, otherwise. On Windows it is the
named pipe `\\.\pipe\tunelith`. tunelithd grants the `video` group access to
its socket by default, as
[Device permissions on Linux](../guide/getting-started#device-permissions-on-linux)
sets up.

chibitv does not name the tuners itself. Each time a channel is tuned, it asks
tunelithd for any free tuner receiving the channel's broadcast, which tunelithd
picks, or shares with a program already receiving the same, and gets back once
the stream is over. How many channels can be received at once is therefore how
many tuners tunelithd has free. A `[[tuners]]` table left over from an older
configuration is ignored.

```toml
[tunelith]
socket = "/run/tunelith/tunelithd.sock"
lnb = true
```

## Channels

Not a table of this file: the channels are kept in the
[`[database]`](#database), where a [scan](./cli#scan) writes them.
[`channels`](./cli#channels) lists what it keeps, with the identifiers the
`--channel` option of `live`, `record` and `status` names.

There is no import path from an older configuration: a `[[channels]]` section
left in the file is ignored, and a scan is what fills a database that has never
seen one. `serve` warns when it starts with no channel stored.

## `[server]`

Read by [`serve`](./cli#serve) only.

| Key       | Type   | Default      | Description                                   |
| --------- | ------ | ------------ | --------------------------------------------- |
| `address` | string | `"[::1]:3001"` | Socket address the HTTP server listens on. |

The default listens on the loopback interface, which the rsbuild development
server proxies `/api` to. Inside a container it has to listen on more than
loopback for the published port to reach it:

```toml
[server]
address = "[::]:3001"
```

## `[database]`

Where chibitv keeps what has to survive a restart: the channels being served,
and the broadcast schedule so far. A scan writes the channels, and the
programme guide is restored before anything is crawled again.

| Key   | Type   | Default                 | Description                                        |
| ----- | ------ | ----------------------- | -------------------------------------------------- |
| `url` | string | `"sqlite://chibitv.db"` | The database to keep it in, as a URL whose scheme picks the backend. |

Every command reads the channels from it, so `live`, `record`, `scan`, `status`
and `serve` all want it pointed at the same database. It is created when it is
not there yet.

SQLite is the only backend implemented, and it is bundled, so no database
server is needed. The path is relative to the working directory; in the Docker
image that directory is not writable, so point the URL at a mounted volume:

```toml
[database]
url = "sqlite://data/chibitv.db"
```

## `[storage]`

Where recordings are written, one object per recording. `type` is the tag a
remote store, such as an S3 bucket, would be picked with; only a directory of
the file system is stored into for now.

| Key    | Type   | Default         | Description                              |
| ------ | ------ | --------------- | ---------------------------------------- |
| `type` | string | `"directory"`   | The kind of store. Only `directory` exists. |
| `path` | string | `"./recordings"` | Directory recordings are written into.  |

```toml
[storage]
type = "directory"
path = "/srv/recordings"
```

## Time zone

Not a key of the file, but a requirement alongside it: ARIB SI carries
wall-clock time in JST, so chibitv reads the schedule against the clock of the
machine it runs on. Run it on JST, or set `TZ` for it, or else it cannot tell
which programme is on air. `serve` warns when the clock disagrees.

```shell
TZ=JST-9 chibitv serve
```

The Docker image sets `TZ=JST-9` itself.
