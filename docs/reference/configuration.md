# Configuration

Every subcommand loads `./config.toml` from the working directory it is started
in. There is no option to point it elsewhere, so run chibitv from the directory
holding the file. Copy the template to start from:

```shell
cp config.toml.example config.toml
```

The file is read into the types in `crates/chibitv/src/config.rs`. Only
[`[cas]`](#cas) is required; every other table has a default, although a
[tuner](#tuners) and a [channel](#channels) are needed before anything can be
tuned.

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

## `[[tuners]]`

An array of tables, one per tuner. `type` picks the implementation, and the
remaining keys belong to that variant.

The `live`, `record`, `scan` and `status` subcommands currently acquire the
first entry; `serve` manages every configured tuner in its registry.

### `type = "dvb"`

A Linux DVB device. Available on Unix with the default `dvb` Cargo feature.

| Key            | Type    | Default    | Description                                                 |
| -------------- | ------- | ---------- | ----------------------------------------------------------- |
| `adapter_num`  | integer | _required_ | Adapter number, the _N_ of `/dev/dvb/adapterN`.              |
| `frontend_num` | integer | _required_ | Frontend number, the _N_ of `/dev/dvb/adapterX/frontendN`.   |

```toml
[[tuners]]
type = "dvb"
adapter_num = 0
frontend_num = 0
```

### `type = "bon"`

A BonDriver DLL, which is how tuners are driven on Windows. Available on
Windows with the default `bon` Cargo feature.

| Key    | Type   | Default    | Description                  |
| ------ | ------ | ---------- | ---------------------------- |
| `path` | string | _required_ | Path to the BonDriver DLL.   |

The DLL reads its own tuning parameters from the `.ini` file sitting next to
it, which is why a [BonDriver channel](#bondriver-channels) names a channel
number rather than a frequency.

```toml
[[tuners]]
type = "bon"
path = 'C:\BonDriver\BonDriver_BDA.dll'
```

### `type = "stdin"`

Reads the stream from standard input instead of a device, which is how a
captured file is fed through the same pipeline. Takes no further keys.

```toml
[[tuners]]
type = "stdin"
```

## `[[channels]]`

An array of tables, one per channel. The `--channel` option of `live`,
`record` and `status` is a zero-based index into this array, so the order of
the entries is the order the GUI and the CLI address them by.

These keys are common to every channel:

| Key                   | Type             | Default    | Description                                                             |
| --------------------- | ---------------- | ---------- | ----------------------------------------------------------------------- |
| `name`                | string           | _required_ | Display name of the channel.                                            |
| `delivery_system`     | string           | _required_ | One of `ISDB-T`, `ISDB-S`, `ISDB-S3`, `Bon-ISDB-T`, `Bon-ISDB-S`, `Bon-ISDB-S3`. |
| `transport_stream_id` | integer          | unset      | Transport stream ID, as written by [`scan`](./cli#scan).                |
| `services`            | array of tables  | empty      | The service catalog of the channel; see [`[[channels.services]]`](#channels-services). |

`delivery_system` also decides how the stream is demultiplexed and which
descrambler is used: `ISDB-T` and `ISDB-S`, the 2K satellite broadcasting,
carry MPEG-2 TS and are descrambled with B25, while `ISDB-S3`, the 4K one,
carries MMT/TLV and is descrambled with B61. The remaining keys depend on it.

### `delivery_system = "ISDB-T"`

| Key            | Type    | Default     | Description                   |
| -------------- | ------- | ----------- | ----------------------------- |
| `frequency`    | integer | _required_  | Centre frequency in Hz.       |
| `bandwidth_hz` | integer | `6000000`   | Channel bandwidth in Hz.      |

```toml
[[channels]]
name = "Terrestrial Example"
delivery_system = "ISDB-T"
frequency = 551142857
bandwidth_hz = 6000000
```

Rather than writing these by hand, let [`scan`](./cli#scan) discover the
physical channels on air and print the entries, including their `services`, as
TOML to merge into the file.

### `delivery_system = "ISDB-S"`

| Key         | Type    | Default    | Description                                  |
| ----------- | ------- | ---------- | -------------------------------------------- |
| `frequency` | integer | _required_ | Transponder frequency in kHz.                |
| `stream_id` | integer | _required_ | Transport stream ID of the stream to select. |

```toml
[[channels]]
name = "BS Example"
delivery_system = "ISDB-S"
frequency = 1049480
stream_id = 0x4031
```

`scan` only walks the terrestrial UHF band, so a satellite channel is written
by hand, along with its [`transport_stream_id`](#channels) and
[`services`](#channels-services) that `serve` needs before tuning.

### `delivery_system = "ISDB-S3"`

| Key         | Type    | Default    | Description                            |
| ----------- | ------- | ---------- | -------------------------------------- |
| `frequency` | integer | _required_ | Transponder frequency in kHz.          |
| `stream_id` | integer | _required_ | TLV stream ID of the stream to select. |

```toml
[[channels]]
name = "BS 4K Example"
delivery_system = "ISDB-S3"
frequency = 1318000
stream_id = 0x40F1
```

### BonDriver channels

A BonDriver holds the tuning parameters itself, so `Bon-ISDB-T`,
`Bon-ISDB-S` and `Bon-ISDB-S3` name the tuning space and channel numbers the
driver enumerates instead of a frequency. The delivery system still has to be
named, because it decides how the stream is demultiplexed.

| Key       | Type    | Default    | Description                     |
| --------- | ------- | ---------- | ------------------------------- |
| `space`   | integer | _required_ | Tuning space number.            |
| `channel` | integer | _required_ | Channel number within the space. |

```toml
[[channels]]
name = "BonDriver Terrestrial Example"
delivery_system = "Bon-ISDB-T"
space = 0
channel = 0

[[channels]]
name = "BonDriver BS Example"
delivery_system = "Bon-ISDB-S"
space = 1
channel = 0

[[channels]]
name = "BonDriver BS 4K Example"
delivery_system = "Bon-ISDB-S3"
space = 2
channel = 0
```

### `[[channels.services]]` {#channels-services}

The services carried on a channel. [`scan`](./cli#scan) writes this catalog,
and `serve` needs it for the MPEG-2 TS channels, `ISDB-T` and `ISDB-S`, so
that every configured physical channel's services are known before tuning.

| Key             | Type    | Default    | Description                            |
| --------------- | ------- | ---------- | -------------------------------------- |
| `id`            | integer | _required_ | Service ID.                            |
| `name`          | string  | _required_ | Service name.                          |
| `provider_name` | string  | empty      | Name of the broadcaster of the service. |

```toml
[[channels]]
name = "TOKYO MX"
delivery_system = "ISDB-T"
frequency = 515142857
transport_stream_id = 12345

[[channels.services]]
id = 23608
name = "TOKYO MX1"
provider_name = "TOKYO MX"
```

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

Where the server keeps what has to survive a restart, which is the broadcast
schedule so far: the programme guide is restored before anything is crawled
again.

| Key   | Type   | Default                 | Description                                        |
| ----- | ------ | ----------------------- | -------------------------------------------------- |
| `url` | string | `"sqlite://chibitv.db"` | The database to keep it in, as a URL whose scheme picks the backend. |

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
TZ=JST-9 cargo run -- serve
```

The Docker image sets `TZ=JST-9` itself.
