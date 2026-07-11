# chibitv - Yet another implementation for ARIB standards

> [!WARNING]
> This software is intended for experimental purposes to understand the standards and is not recommended for any other use.

## Prerequisites

- A DVB compatible tuner to produce raw MMT/TLV stream
- A PC/SC compatible interface to the CAS module
- The value of _Kd_ defined in Section 1.4 of the ARIB STD-B61 standard

### Device permissions on Linux

chibitv needs access to both the PC/SC daemon and DVB devices. The following setup allows the application to run
without `sudo` on distributions using pcsc-lite, polkit, and udev, such as Ubuntu and Debian.

Create a dedicated group for PC/SC access, then add the current user to both that group and the `video` group used by
DVB devices:

```shell
sudo groupadd --force --system pcsc
sudo usermod --append --groups pcsc,video "$USER"
```

Allow members of the `pcsc` group to connect to the PC/SC daemon and access smart cards by creating
`/etc/polkit-1/rules.d/60-pcsc-lite.rules`:

```shell
sudo tee /etc/polkit-1/rules.d/60-pcsc-lite.rules >/dev/null <<'EOF'
polkit.addRule(function(action, subject) {
    if (
        (
            action.id == "org.debian.pcsc-lite.access_pcsc" ||
            action.id == "org.debian.pcsc-lite.access_card"
        ) &&
        subject.isInGroup("pcsc")
    ) {
        return polkit.Result.YES;
    }
});
EOF

sudo chmod 0644 /etc/polkit-1/rules.d/60-pcsc-lite.rules
```

Log out and back in after changing the group membership. Applications launched from an IDE or a terminal opened
before this change must also be restarted. Verify the setup with:

```shell
id -nG
pcsc_scan
ls -l /dev/dvb
```

The output of `id -nG` should include both `pcsc` and `video`, and `pcsc_scan` should detect the card reader without
`sudo`.

## Usage

Every subcommand loads `./config.toml` from the current directory. Copy the example and configure the CAS master key,
tuners, and channels before running chibitv:

```shell
cp config.toml.example config.toml
```

Run a subcommand with `cargo run -- <COMMAND>`. The channel arguments used by `live`, `record`, and `status` are
zero-based indices into the `[[channels]]` entries in `config.toml`. Tuner commands currently use the first entry in
`[[tuners]]`. Place the global `--verbose` option before the subcommand to enable trace logging:

```shell
cargo run -- --verbose live --channel 0
```

### `live`

Tune to a configured channel, descramble it, remux it to MPEG-2 Transport Stream, and write the result to stdout. The
stream continues until interrupted with <kbd>Ctrl</kbd>+<kbd>C</kbd>.

```shell
# Watch the first configured channel with a player that accepts stdin.
cargo run -- live --channel 0 | mpv -

# Alternatively, save the remuxed stream.
cargo run -- live --channel 0 > live.m2ts
```

Both ISDB-S channels using MMT/TLV and ISDB-T channels using MPEG-2 TS are supported.

### `record`

Tune to a configured channel and copy the raw tuner stream without descrambling or remuxing it. `--output` defaults
to stdout; pass a path to write directly to a file.

```shell
cargo run -- record --channel 0 --output capture.mmts

# The explicit output value `-` also means stdout.
cargo run -- record --channel 0 --output - > capture.mmts
```

### `remux`

Descramble and remux an existing stream. The input defaults to stdin and MMT/TLV (`mmts`), while the output defaults
to stdout and MPEG-2 TS (`m2ts`). Pass `-` as the input or output path to select standard input or output explicitly.

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

Supported input formats are `mmts` and `m2ts`; supported output format names are `m2ts`, `mp4`, and `fmp4`. A regular
MP4 requires an output path. MP4 and fragmented MP4 output from an `m2ts` input are not currently supported.

### `scan`

Scan terrestrial UHF physical channels and print discovered ISDB-T `[[channels]]` entries and their inline
`services` catalog as TOML. The default range is channels 13 through 52, with a maximum wait of 12 seconds per
channel.

```shell
cargo run -- scan > scanned-channels.toml

# Scan a smaller range and wait up to 5 seconds per channel.
cargo run -- scan --start-channel 20 --end-channel 30 --timeout 5 > scanned-channels.toml
```

Review the generated file and merge its `[[channels]]` entries into `config.toml`.

### `status`

Tune to a configured ISDB-T channel and print its network, services, and current events from the B10 SI tables. The
command waits up to 3 seconds by default for the required tables.

```shell
cargo run -- status --channel 1
cargo run -- status --channel 1 --timeout 10
```

This command currently supports ISDB-T channels only.

### `serve`

Start the HTTP API and live-streaming server at `server.address` from `config.toml`. The default address is
`[::1]:3001`, and the first configured channel is selected when the server starts.

```shell
# Terminal 1: start the backend.
cargo run -- serve

# Terminal 2: start the GUI development server.
pnpm install
pnpm --filter chibitv dev
```

Open http://localhost:3000/ in your browser and enjoy!

The server supports ISDB-S and ISDB-T channels and requires at least one configured tuner and channel. For ISDB-T,
generate the service catalog with `scan` first so that every configured physical channel's services are available
before tuning.

## References

- ARIB STD-B32: https://www.arib.or.jp/english/html/overview/doc/6-STD-B32v3_11-3p3-E1.pdf
- ARIB STD-B60: https://www.arib.or.jp/english/html/overview/doc/6-STD-B60_v1_14-E1.pdf
- ARIB STD-B61: https://www.arib.or.jp/english/html/overview/doc/6-STD-B61v1_4-E1.pdf
