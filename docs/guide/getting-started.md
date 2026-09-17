# Getting started

> [!WARNING]
> This software is intended for experimental purposes to understand the
> standards and is not recommended for any other use.

## Prerequisites

- A DVB compatible tuner to produce raw MMT/TLV stream
- A PC/SC compatible interface to the CAS module
- The value of _Kd_ defined in Section 1.4 of the ARIB STD-B61 standard

### Device permissions on Linux

chibitv needs access to both the PC/SC daemon and DVB devices. The following
setup allows the application to run without `sudo` on distributions using
pcsc-lite, polkit, and udev, such as Ubuntu and Debian.

Create a dedicated group for PC/SC access, then add the current user to both
that group and the `video` group used by DVB devices:

```shell
sudo groupadd --force --system pcsc
sudo usermod --append --groups pcsc,video "$USER"
```

Allow members of the `pcsc` group to connect to the PC/SC daemon and access
smart cards by creating `/etc/polkit-1/rules.d/60-pcsc-lite.rules`:

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

Log out and back in after changing the group membership. Applications launched
from an IDE or a terminal opened before this change must also be restarted.
Verify the setup with:

```shell
id -nG
pcsc_scan
ls -l /dev/dvb
```

The output of `id -nG` should include both `pcsc` and `video`, and `pcsc_scan`
should detect the card reader without `sudo`.

## Configuring chibitv

Every subcommand loads `./config.toml` from the current directory. Copy the
example and configure the CAS master key, tuners, and channels before running
chibitv:

```shell
cp config.toml.example config.toml
```

Every key of the file is described in the
[configuration reference](../reference/configuration).

## Running a subcommand

Run a subcommand with `cargo run -- <COMMAND>`. The channel arguments used by
`live`, `record`, and `status` are zero-based indices into the `[[channels]]`
entries in `config.toml`. Tuner commands currently use the first entry in
`[[tuners]]`. Place the global `--verbose` option before the subcommand to
enable trace logging:

```shell
cargo run -- --verbose live --channel 0
```

Every subcommand and option is described in the [CLI reference](../reference/cli).

## Setting the time zone

ARIB SI carries wall-clock time in JST, so chibitv reads the schedule against
the clock of the machine it runs on. Run it on JST, or set `TZ` for it, or
else it cannot tell which programme is on air:

```shell
TZ=JST-9 cargo run -- serve
```

## Scanning channels

Let [`scan`](../reference/cli#scan) discover the channels on air rather than
writing them by hand. It prints `[[channels]]` entries, and their inline
`services` catalog, as TOML:

```shell
# The terrestrial UHF channels.
cargo run -- scan > scanned-channels.toml

# The BS and CS110 transponders.
cargo run -- scan --delivery-system ISDB-S > scanned-satellite.toml

# The 4K broadcasting on the BS transponders.
cargo run -- scan --delivery-system ISDB-S3 > scanned-4k.toml
```

Review the generated file and merge its `[[channels]]` entries into
`config.toml`. The server needs this catalog so that every configured physical
channel's services are available before tuning.

2K and 4K share the transponders but not the signalling, so a dish carrying
both is scanned twice. The 4K scan reaches BS only for now.

A satellite network describes itself in full, so `--fast` writes the same
entries out of the signalling on one transponder per network instead of tuning
to every stream. It waits for every stream to be described, so give it a longer
`--timeout`:

```shell
cargo run -- scan --delivery-system ISDB-S --fast --timeout 30 > scanned-satellite.toml
```

## Starting the server

[`serve`](../reference/cli#serve) runs the HTTP API and the live stream; the
GUI is served separately during development by the rsbuild development server,
which proxies its RPC requests to the backend:

```shell
# Terminal 1: start the backend.
cargo run -- serve

# Terminal 2: start the GUI development server.
pnpm install
pnpm --filter chibitv dev
```

Open `http://localhost:3000/` in your browser and enjoy!

The GUI is a Progressive Web App, so a browser loading a built GUI
(`pnpm build`, or the Docker image) offers to install it as a standalone app.
Installing requires a secure context, so serve it over HTTPS or from
`localhost`. Its Service Worker caches the application shell and the bundles,
so that an installed app still opens while the server is unreachable; the RPC
API and the live stream are never cached. It is registered in production
builds only, which leaves the rsbuild development server above unaffected.

## Next steps

- The [configuration reference](../reference/configuration) for every key of
  `config.toml`.
- The [CLI reference](../reference/cli) for every subcommand and option.
- [Docker](../deployment/docker) for running the published image instead of
  building from source.
