# Getting started

> [!WARNING]
> This software is intended for experimental purposes to understand the
> standards and is not recommended for any other use.

## Prerequisites

- A tuner shared by tunelithd, the daemon of
  [Tunelith](https://github.com/siketyan/tunelith): PLEX, Digibest and
  e-better tuners over USB, the PT4K, or any tuner with a Linux DVB driver.
  Its [README](https://github.com/siketyan/tunelith#readme) covers installing
  and running tunelithd, together with the firmware and udev rules the devices
  need
- A PC/SC compatible interface to the CAS module
- The value of _Kd_ defined in Section 1.4 of the ARIB STD-B61 standard

### Device permissions on Linux

chibitv needs access to both the PC/SC daemon and the socket of tunelithd. The
following setup allows the application to run without `sudo` on distributions
using pcsc-lite, polkit, and udev, such as Ubuntu and Debian.

Create a dedicated group for PC/SC access, then add the current user to both
that group and the `video` group, which tunelithd grants access to its
socket by default:

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
ls -l /run/tunelith/tunelithd.sock
```

The output of `id -nG` should include both `pcsc` and `video`, and `pcsc_scan`
should detect the card reader without `sudo`.

## Installation

Every [release](https://github.com/siketyan/chibitv/releases/latest) comes
with prebuilt binaries, which embed the GUI so that
[`serve`](../reference/cli#serve) hosts it beside the API. Download the one for
your system from the assets of the release, or run the
[Docker image](./docker) instead.

The Linux binaries are built on Ubuntu 24.04, so they need glibc 2.39 or
newer: Ubuntu 24.04, Debian 13 and Fedora 40 or later.

### Debian and Ubuntu

```shell
sudo apt install ./chibitv_<VERSION>_amd64.deb
```

### Fedora and other RPM based distributions

```shell
sudo dnf install ./chibitv-<VERSION>-1.x86_64.rpm
```

### Arch Linux

```shell
sudo pacman -U chibitv-<VERSION>-1-x86_64.pkg.tar.zst
```

The packages above are built for arm64 as well, named with `arm64` or
`aarch64` in place of `amd64` or `x86_64`. They install the binary as
`/usr/bin/chibitv` and pull in libpcsclite.

### Other Linux distributions

The `.tar.gz` archives hold the binary alone. Install libpcsclite
from your distribution, together with the PC/SC daemon, then extract the
binary:

```shell
tar --extract --gzip --file chibitv-<VERSION>-x86_64-unknown-linux-gnu.tar.gz
```

### Windows

The `.zip` archive holds `chibitv.exe`, which tunes through tunelithd over its
named pipe and reaches the CAS module through the smart card service of Windows.

### macOS

The `.tar.gz` archive for `aarch64-apple-darwin` holds the binary alone, for
Apple silicon. It reaches the CAS module through the PC/SC framework built into
macOS, so nothing else needs installing. The binary is not signed, so clear the
quarantine Gatekeeper puts on it after extracting:

```shell
tar --extract --gzip --file chibitv-<VERSION>-aarch64-apple-darwin.tar.gz
xattr -d com.apple.quarantine chibitv
```

### Building from source

Install the Rust toolchain and, on Linux, the development package of
libpcsclite, then install the binary from a clone of the repository:

```shell
sudo apt install libpcsclite-dev
cargo install --locked --path crates/chibitv
```

The binary built this way leaves the GUI out, which the rsbuild development
server hosts instead, as [starting the server](#starting-the-server) describes.

## Configuring chibitv

Every subcommand loads `./config.toml` from the current directory. Copy the
example, which the packages install as
`/usr/share/doc/chibitv/config.toml.example` and the repository holds as
`config.toml.example`, and configure the CAS master key before running
chibitv; the channels are not part of the file — they are kept in the database, which
[scanning](#scanning-channels) writes:

```shell
cp config.toml.example config.toml
```

Every key of the file is described in the
[configuration reference](../reference/configuration).

## Running a subcommand

Run a subcommand with `chibitv <COMMAND>`. The channel arguments used by
`live`, `record`, and `status` are the identifiers the database gave the
channels, which [`channels`](../reference/cli#channels) lists. Tuner commands
ask tunelithd for any free tuner receiving the channel's broadcast.
Place the global `--verbose` option before the subcommand to enable trace
logging:

```shell
chibitv channels
chibitv --verbose live --channel 1
```

Every subcommand and option is described in the [CLI reference](../reference/cli).

## Setting the time zone

ARIB SI carries wall-clock time in JST, so chibitv reads the schedule against
the clock of the machine it runs on. Run it on JST, or set `TZ` for it, or
else it cannot tell which programme is on air:

```shell
TZ=JST-9 chibitv serve
```

## Scanning channels

[`scan`](../reference/cli#scan) is what puts the channels on air into the
database, so nothing can be watched until one has run:

```shell
# The terrestrial UHF channels.
chibitv scan

# The BS and CS110 transponders.
chibitv scan --delivery-system ISDB-S

# The 4K broadcasting on the BS transponders.
chibitv scan --delivery-system ISDB-S3
```

Each of these replaces the channels kept for the broadcast it walked and leaves
the others alone, so a dish and an aerial are scanned one after another. The
service catalog comes along, which the server needs so that the services of
every physical channel are known before tuning. Scanning from the GUI saves the
same way, without restarting the server.

2K and 4K share the transponders but not the signalling, so a dish carrying
both is scanned twice. The 4K scan reaches BS only for now.

A satellite network describes itself in full, so `--fast` writes the same
entries out of the signalling on one transponder per network instead of tuning
to every stream. It waits for every stream to be described, so give it a longer
`--timeout`:

```shell
chibitv scan --delivery-system ISDB-S --fast --timeout 30 > scanned-satellite.toml
```

## Starting the server

[`serve`](../reference/cli#serve) runs the HTTP API, the live stream and
the GUI:

```shell
chibitv serve
```

Open `http://localhost:3001/` in your browser and enjoy!

A binary [built from source](#building-from-source) serves the API alone. The
rsbuild development server hosts the GUI instead, at `http://localhost:3000/`,
and proxies its RPC requests to the backend:

```shell
pnpm install
pnpm --filter chibitv dev
```

The GUI is a Progressive Web App, so a browser loading a built GUI
(a prebuilt binary, `pnpm build`, or the Docker image) offers to install it as a standalone app.
Installing requires a secure context, so serve it over HTTPS or from
`localhost`. Its Service Worker caches the application shell and the bundles,
so that an installed app still opens while the server is unreachable; the RPC
API and the live stream are never cached. It is registered in production
builds only, which leaves the rsbuild development server above unaffected.

The channel pane shows each station's main service and the current programme
when its schedule is available. Use the arrow beside a station to show its
subchannels; a selected subchannel stays visible when the group is collapsed.
Separate stations sharing a satellite multiplex remain individually selectable.

Station logos are collected from broadcast SI while watching or refreshing the
programme guide and saved in the database. Reception can take several minutes,
depending on the broadcaster's transmission cycle. The GUI updates automatically
when a logo arrives. Supported carriers are CDT, MH-CDT (including fragmented
logos), and the named BS/CS logo modules in DSM-CC common receiver data.

## Next steps

- The [configuration reference](../reference/configuration) for every key of
  `config.toml`.
- The [CLI reference](../reference/cli) for every subcommand and option.
- [Docker](./docker) for running the published image instead of
  building from source.
