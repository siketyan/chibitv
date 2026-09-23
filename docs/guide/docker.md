# Docker

The image built from the `Dockerfile` bundles the GUI into the server binary,
so a single container serves both the RPC API and the web interface.
Development is unaffected: `cargo run -- serve` still serves the API alone
while the rsbuild dev server hosts the GUI and proxies the RPC requests to it.

Prebuilt images are published to `ghcr.io/siketyan/chibitv`: `main` follows
the main branch, and each release is tagged with its version, such as `0.2.0`,
and its minor version, such as `0.2`. To build one locally instead:

```shell
docker build --tag chibitv .
```

## Running the image

The image sets `TZ=JST-9` so that the container reads the broadcast schedule
on the clock the SI is expressed in. The container reads `/app/config.toml`
and needs access to the tuner devices and the PC/SC daemon of the host.

Its working directory is not writable, so the
[database](../reference/configuration#database), which keeps the channels and
the programme guide, takes a volume to write into and a `[database]` URL
pointing at it, for example `url = "sqlite://data/chibitv.db"`. Note that
[`server.address`](../reference/configuration#server) has to listen on more
than the loopback interface of the container, for example
`address = "[::]:3001"`:

```shell
docker run --rm \
  --publish 3001:3001 \
  --user "$(id -u):$(id -g)" \
  --group-add "$(getent group video | cut -d: -f3)" \
  --volume "$PWD/config.toml:/app/config.toml:ro" \
  --volume "$PWD/data:/app/data" \
  --volume /run/pcscd/pcscd.comm:/run/pcscd/pcscd.comm \
  --device /dev/dvb/adapter0/frontend0 \
  --device /dev/dvb/adapter0/demux0 \
  --device /dev/dvb/adapter0/dvr0 \
  ghcr.io/siketyan/chibitv:main
```

A tuner px4_drv drives is passed in the same way, with the device file the
driver made for it in place of the three DVB ones, for example
`--device /dev/pxmlt5video0`.

Open `http://localhost:3001/` in your browser and enjoy!

## Permissions

The image never runs as `root`: it defaults to the unprivileged user of the
distroless base image, and chibitv itself needs no privileges beyond reaching
the devices and the daemon. Both of those are still checked against the host,
which is what the two options above are for:

- DVB and px4_drv device nodes belong to the `video` group, so the container
  process has to be a member of it.
- pcsc-lite authorizes card access with polkit, which resolves the user of the
  connecting process on the host. Running the container as the host user set
  up in
  [Device permissions on Linux](./getting-started#device-permissions-on-linux)
  therefore keeps the same rule working. Without `--user`, the polkit rule has
  to accept the user of the image (uid 65532) instead.

## pcsc-lite version mismatch

The client library of pcsc-lite also has to agree with the daemon it connects
to. The image ships the 2.3 series of Debian 13, which reports
`SCARD_E_NO_SERVICE` against an older `pcscd`, such as the 2.0 series of
Ubuntu 24.04. Mount the library of the host over the one of the image if
upgrading the daemon is not an option:

```shell
docker run --rm \
  --volume /usr/lib/x86_64-linux-gnu/libpcsclite.so.1.0.0:/usr/lib/x86_64-linux-gnu/libpcsclite.so.1:ro \
  ...
```
