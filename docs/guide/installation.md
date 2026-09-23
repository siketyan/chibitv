# Installation

Every [release](https://github.com/siketyan/chibitv/releases/latest) comes
with prebuilt binaries, which embed the GUI so that
[`serve`](../reference/cli#serve) hosts it beside the API. Download the one for
your system from the assets of the release, or run the
[Docker image](./docker) instead.

The Linux binaries are built on Ubuntu 24.04, so they need glibc 2.39 or
newer: Ubuntu 24.04, Debian 13 and Fedora 40 or later.

## Debian and Ubuntu

```shell
sudo apt install ./chibitv_<VERSION>_amd64.deb
```

## Fedora and other RPM based distributions

```shell
sudo dnf install ./chibitv-<VERSION>-1.x86_64.rpm
```

## Arch Linux

```shell
sudo pacman -U chibitv-<VERSION>-1-x86_64.pkg.tar.zst
```

The packages above are built for arm64 as well, named with `arm64` or
`aarch64` in place of `amd64` or `x86_64`. They install the binary as
`/usr/bin/chibitv` and the example configuration as
`/usr/share/doc/chibitv/config.toml.example`, and pull in libdvbv5 and
libpcsclite. Copy the example into the directory chibitv is run from to
[configure it](./getting-started#configuring-chibitv):

```shell
cp /usr/share/doc/chibitv/config.toml.example config.toml
```

## Other Linux distributions

The `.tar.gz` archives hold the binary alone. Install libdvbv5 and libpcsclite
from your distribution, together with the PC/SC daemon, then extract the
binary:

```shell
tar --extract --gzip --file chibitv-v<VERSION>-x86_64-unknown-linux-gnu.tar.gz
```

## Windows

The `.zip` archive holds `chibitv.exe`, which tunes through a BonDriver and
reaches the CAS module through the smart card service of Windows.

## Building from source

Clone the repository and run chibitv through Cargo, which the
[getting started guide](./getting-started) follows. This needs the Rust
toolchain and, on Linux, the development packages of the libraries:

```shell
sudo apt install libdvbv5-dev libpcsclite-dev
cargo run -- <COMMAND>
```
