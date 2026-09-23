# Configuration

Every subcommand loads `./config.toml` from the working directory it is started
in. There is no option to point it elsewhere, so run chibitv from the directory
holding the file. Copy the template to start from:

```shell
cp config.toml.example config.toml
```

The file is read into the types in `crates/chibitv/src/config.rs`. Only
[`[cas]`](#cas) is required; every other table has a default, although a
[tuner](#tuners) is needed before anything can be tuned.

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

## `[[tuners]]`

An array of tables, one per tuner. `type` picks the implementation, and the
remaining keys belong to that variant, apart from `delivery_systems`, which
every variant takes.

| Key                | Type     | Default | Description                                                                                       |
| ------------------ | -------- | ------- | ------------------------------------------------------------------------------------------------- |
| `delivery_systems` | string[] | all     | The broadcasts the tuner receives, out of `"ISDB-T"`, `"ISDB-S"` and `"ISDB-S3"` (BS/CS 4K). |

A tuner is picked for the broadcast the channel is on: the first free one in
the file that receives it. Unless `delivery_systems` is given, a tuner is taken
to receive every broadcast, so a setup where every tuner does can leave it out.
Where a tuner receives only some, naming them keeps a channel it cannot reach
from being tuned on it, and keeps the tuner free for what it can, such as a 4K
satellite tuner beside a terrestrial one:

```toml
[[tuners]]
type = "dvb"
adapter_num = 0
frontend_num = 0
delivery_systems = ["ISDB-S", "ISDB-S3"]

[[tuners]]
type = "px4"
path = "/dev/pxmlt5video0"
delivery_systems = ["ISDB-T", "ISDB-S"]
```

The `live`, `record`, `scan` and `status` subcommands take the first free
tuner receiving the channel or the broadcast scanned; `serve` manages every
configured tuner in its registry the same way.

### `type = "dvb"`

A Linux DVB device. Available on Linux with the default `dvb` Cargo feature.

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

### `type = "px4"`

A tuner driven by [px4_drv](https://github.com/tsukumijima/px4_drv), the
Linux driver of the PLEX and Digibest tuners (PX-W3U4, PX-MLT5PE, PX-M1UR,
DTV02A-1T1S-U and the like). Available on Linux with the default `px4` Cargo
feature.

| Key           | Type    | Default    | Description                                                                          |
| ------------- | ------- | ---------- | ------------------------------------------------------------------------------------ |
| `path`        | string  | _required_ | Path to the device file the driver makes, such as `/dev/pxmlt5video0`.               |
| `lnb_voltage` | integer | `0`        | Voltage fed to the dish's converter while a satellite channel is tuned: 0, 11 or 15. |

The driver takes the channel numbers of the PT1/PT3 drivers rather than a
frequency, and chibitv works them out from the channel it is given, so the
channels a [scan](./cli#scan) finds with any tuner can be tuned with this one.
Only ISDB-T and ISDB-S are received: none of the tuners the driver supports
takes ISDB-S3.

The device is opened while the tuner is in use and closed once it is released,
so other programs, `recpt1` for instance, can use it in between. A device the
driver made for one broadcast only, such as the `px4video0` and `px4video1`
of a PX-W3U4 which receive ISDB-S alone, refuses a channel of the other.

Device files of the driver belong to the `video` group, as the
[DVB ones](../guide/getting-started#device-permissions-on-linux) do.

```toml
[[tuners]]
type = "px4"
path = "/dev/pxmlt5video0"
lnb_voltage = 15
```

### `type = "bon"`

A BonDriver DLL, which is how tuners are driven on Windows. Available on
Windows with the default `bon` Cargo feature.

| Key    | Type   | Default    | Description                  |
| ------ | ------ | ---------- | ---------------------------- |
| `path` | string | _required_ | Path to the BonDriver DLL.   |

The DLL reads its own tuning parameters from the `.ini` file sitting next to
it, which is why a [BonDriver channel](./cli#channels) names a channel number
rather than a frequency.

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
