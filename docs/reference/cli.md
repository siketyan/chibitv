# CLI

```
chibitv [OPTIONS] <COMMAND>
```

Every subcommand loads [`./config.toml`](./configuration) from the working
directory before it runs. During development, run the binary through Cargo:

```shell
cargo run -- <COMMAND>
```

## Global options

| Option            | Description                                                  |
| ----------------- | ------------------------------------------------------------ |
| `-v`, `--verbose` | Perform verbose logging, raising the default level to `TRACE`. |
| `-h`, `--help`    | Print help.                                                  |

The global options go before the subcommand:

```shell
cargo run -- --verbose live --channel 0
```

Logs are written to stderr, which leaves stdout free for the stream a
subcommand writes there. The level defaults to `INFO` and is otherwise read
from `RUST_LOG`, which `--verbose` only changes the default of.

## Commands

| Command             | Description                                             |
| ------------------- | ------------------------------------------------------- |
| [`live`](#live)     | Watch a channel as a remuxed M2TS stream written to stdout. |
| [`record`](#record) | Record a MMT/TLV stream from a tuner.                   |
| [`remux`](#remux)   | Demux a MMT/TLV stream and mux a M2TS stream.           |
| [`scan`](#scan)     | Scan physical channels and print the channel config as TOML. |
| [`status`](#status) | Show current broadcast status from B10 SI tables.       |
| [`serve`](#serve)   | Run the chibitv server.                                 |

The `--channel` option of `live`, `record` and `status` is a zero-based index
into the [`[[channels]]`](./configuration#channels) entries of the
configuration. Tuner commands currently use the first entry in
[`[[tuners]]`](./configuration#tuners).

## `live`

Tune to a configured channel, descramble it, remux it to MPEG-2 Transport
Stream, and write the result to stdout. The stream continues until interrupted
with <kbd>Ctrl</kbd>+<kbd>C</kbd>.

| Option                    | Type    | Default    | Description                    |
| ------------------------- | ------- | ---------- | ------------------------------ |
| `-c`, `--channel <INDEX>` | integer | _required_ | Index of the channel to tune to. |

```shell
# Watch the first configured channel with a player that accepts stdin.
cargo run -- live --channel 0 | mpv -

# Alternatively, save the remuxed stream.
cargo run -- live --channel 0 > live.m2ts
```

Every delivery system is supported; the one the channel names picks the
descrambler and the demultiplexer: ISDB-S3 carries MMT/TLV and is descrambled
with B61, while ISDB-T and ISDB-S carry MPEG-2 TS and are descrambled with
B25.

## `record`

Tune to a configured channel and copy the raw tuner stream, without
descrambling or remuxing it. The stream continues until interrupted with
<kbd>Ctrl</kbd>+<kbd>C</kbd>.

| Option                    | Type    | Default    | Description                                       |
| ------------------------- | ------- | ---------- | ------------------------------------------------- |
| `-c`, `--channel <INDEX>` | integer | _required_ | Index of the channel to tune to.                  |
| `-o`, `--output <PATH>`   | string  | stdout     | Destination path of the output stream. `-` means stdout. |

```shell
cargo run -- record --channel 0 --output capture.mmts

# The explicit output value `-` also means stdout.
cargo run -- record --channel 0 --output - > capture.mmts
```

What is written is the scrambled stream as the tuner produced it, so
[`remux`](#remux) is what turns the file into something playable.

## `remux`

Descramble and remux an existing stream.

| Argument / Option                | Type   | Default | Description                                        |
| -------------------------------- | ------ | ------- | -------------------------------------------------- |
| `[INPUT]`                        | string | stdin   | Source path of the input stream. `-` means stdin.   |
| `-o`, `--output <PATH>`          | string | stdout  | Destination path of the output stream. `-` means stdout. |
| `--input-format <FORMAT>`        | enum   | `mmts`  | Format of the input stream: `mmts` or `m2ts`.       |
| `-f`, `--format <FORMAT>`        | enum   | `m2ts`  | Format of the output stream: `m2ts`, `mp4` or `fmp4`. |

`mmts` is a MMT/TLV stream, `m2ts` a MPEG-2 Transport Stream, `mp4` a regular
MPEG-4 / ISO BMFF file and `fmp4` a fragmented MP4.

```shell
# MMT/TLV to MPEG-2 TS.
cargo run -- remux capture.mmts --output program.m2ts

# MMT/TLV to a regular MP4 file.
cargo run -- remux capture.mmts --format mp4 --output program.mp4

# ISDB-T MPEG-2 TS descrambling/remuxing.
cargo run -- remux terrestrial.m2ts --input-format m2ts --format m2ts --output descrambled.m2ts

# MMT/TLV to fragmented MP4 on stdout.
cargo run -- remux capture.mmts --format fmp4 > program.fmp4
```

Two limits are worth knowing before picking a format:

- A regular MP4 has to seek back to write its index, so `--format mp4`
  requires an output path and cannot write to stdout. Fragmented MP4 can.
- MP4 and fragmented MP4 output from an `m2ts` input are not currently
  supported.

## `scan`

Scan the physical channels on air and print the discovered
[`[[channels]]`](./configuration#channels) entries, and their inline
`services` catalog, as TOML on stdout.

| Option                        | Type    | Default  | Description                                    |
| ----------------------------- | ------- | -------- | ---------------------------------------------- |
| `--delivery-system <SYSTEM>`  | string  | `ISDB-T` | `ISDB-T` for terrestrial UHF, `ISDB-S` for BS and CS110, `ISDB-S3` for the 4K broadcasting on BS. |
| `--start-channel <N>`         | integer | `13`     | First UHF physical channel to scan. ISDB-T only. |
| `--end-channel <N>`           | integer | `52`     | Last UHF physical channel to scan. ISDB-T only. |
| `--timeout <SECONDS>`         | integer | `12`     | Maximum time to wait on each channel.          |

A terrestrial scan walks the UHF physical channels in order. The range has to
lie within 13 to 52, and the start must not exceed the end; anything else is
rejected before tuning. Scanning the full range waits up to the timeout on
every channel that carries nothing, so a complete scan takes a while.

A satellite scan works the other way round, because a satellite stream is
picked by its id rather than by a channel number and which ids are on air
changes as broadcasters come and go. The built-in BS and CS110 transponder
frequencies are only a way in: the first transponder that answers hands over
its network's NIT, which names every stream of that network and the transponder
each one sits on, and the scan then tunes to the ones carrying television to
read their services. Reaching a network is therefore a couple of tunes, and the
length of the scan is set by how many streams it finds.

`ISDB-S3`, the 4K broadcasting, is scanned the same way over MMT/TLV: the
transmission control signal of a TLV stream carries the TLV-NIT that names the
network, and the services of a stream are named by its own MH-SDT. It needs the
[`master_key`](./configuration#cas) and an ACAS card, as watching 4K does. 2K
and 4K are separate scans because they share the transponders but not the
signalling, so a dish carrying both is scanned twice.

The 4K scan walks the BS transponders only. A stream id is built from the
network it belongs to, and BS numbers its 4K network apart from its 2K one;
which network the 4K on 110CS is numbered as is in ARIB TR-B39, so those
transponders are left alone until it is known.

```shell
cargo run -- scan > scanned-channels.toml

# Scan a smaller range and wait up to 5 seconds per channel.
cargo run -- scan --start-channel 20 --end-channel 30 --timeout 5 > scanned-channels.toml

# Scan the BS and CS110 transponders instead.
cargo run -- scan --delivery-system ISDB-S > scanned-channels.toml

# The 4K broadcasting on the BS transponders.
cargo run -- scan --delivery-system ISDB-S3 > scanned-4k-channels.toml
```

Review the generated file and merge its `[[channels]]` entries into
`config.toml`. This is also how a channel gets the service catalog that
[`serve`](#serve) needs.

## `status`

Tune to a configured ISDB-T channel and print its network, services, and
current events from the B10 SI tables.

| Option                    | Type    | Default    | Description                                        |
| ------------------------- | ------- | ---------- | -------------------------------------------------- |
| `-c`, `--channel <INDEX>` | integer | _required_ | Index of the channel to tune to.                    |
| `--timeout <SECONDS>`     | integer | `3`        | Maximum time to wait for SI tables before printing. |

```shell
cargo run -- status --channel 1
cargo run -- status --channel 1 --timeout 10
```

This command supports the channels carrying MPEG-2 TS, ISDB-T and ISDB-S; an
ISDB-S3 channel, which carries MMT/TLV, is rejected. The command prints as soon as the tables it needs have
arrived, so the timeout is an upper bound rather than how long it takes.

## `serve`

Start the HTTP API and live-streaming server at
[`server.address`](./configuration#server) from the configuration. The default
address is `[::1]:3001`, and the first configured channel is selected when the
server starts. The command takes no options.

```shell
# Terminal 1: start the backend.
cargo run -- serve

# Terminal 2: start the GUI development server.
pnpm install
pnpm --filter chibitv dev
```

Open `http://localhost:3000/` in your browser and enjoy!

The server supports every delivery system and requires at least one
configured tuner and channel. For the MPEG-2 TS ones, ISDB-T and ISDB-S,
the service catalog has to be in the configuration before tuning: generate it
with [`scan`](#scan) for ISDB-T, and write it by hand for ISDB-S, which
`scan` does not walk.

What has to survive a restart is kept in the
[database](./configuration#database): the broadcast schedule is restored
before anything is crawled again, so the programme guide is there as soon as
the server is up. Recordings are written to the configured
[storage](./configuration#storage).

Because ARIB SI carries wall-clock time in JST, the server warns at startup
when its clock is not on that offset, which leaves the programme on air
unrecognised. Set `TZ=JST-9` for it.
