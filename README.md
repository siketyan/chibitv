# chibitv - Yet another implementation for ARIB standards

> [!WARNING]
> This software is intended for experimental purposes to understand the standards and is not recommended for any other use.

## Documentation

The documentation is published at https://siketyan.github.io/chibitv/:

- [Getting started](https://siketyan.github.io/chibitv/guide/getting-started) covers the prerequisites, the device
  permissions needed on Linux, configuring `config.toml`, and running the server alongside the GUI.
- The [configuration reference](https://siketyan.github.io/chibitv/reference/configuration) documents every key of
  `config.toml`.
- The [CLI reference](https://siketyan.github.io/chibitv/reference/cli) documents every subcommand and option.

## Docker

The image built from the `Dockerfile` bundles the GUI into the server binary, so a single container serves both the
RPC API and the web interface. Development is unaffected: `cargo run -- serve` still serves the API alone while the
rsbuild dev server hosts the GUI and proxies the RPC requests to it.

Prebuilt images are published to `ghcr.io/siketyan/chibitv`. To build one locally instead:

```shell
docker build --tag chibitv .
```

The image sets `TZ=JST-9` so that the container reads the broadcast schedule on the clock the SI is expressed in.
The container reads `/app/config.toml` and needs access to the tuner devices and the PC/SC daemon of the host.
Its working directory is not writable, so the database takes a volume to write into and a `[database]` URL pointing
at it, for example `url = "sqlite://data/chibitv.db"`.
Note that `server.address` has to listen on more than the loopback interface of the container, for example
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

Open http://localhost:3001/ in your browser and enjoy!

The image never runs as `root`: it defaults to the unprivileged user of the distroless base image, and chibitv
itself needs no privileges beyond reaching the devices and the daemon. Both of those are still checked against the
host, which is what the two options above are for:

- DVB device nodes belong to the `video` group, so the container process has to be a member of it.
- pcsc-lite authorizes card access with polkit, which resolves the user of the connecting process on the host.
  Running the container as the host user set up in [Device permissions on Linux](https://siketyan.github.io/chibitv/guide/getting-started#device-permissions-on-linux)
  therefore keeps the same rule working. Without `--user`, the polkit rule has to accept the user of the image
  (uid 65532) instead.

The client library of pcsc-lite also has to agree with the daemon it connects to. The image ships the 2.3 series of
Debian 13, which reports `SCARD_E_NO_SERVICE` against an older `pcscd`, such as the 2.0 series of Ubuntu 24.04. Mount
the library of the host over the one of the image if upgrading the daemon is not an option:

```shell
docker run --rm \
  --volume /usr/lib/x86_64-linux-gnu/libpcsclite.so.1.0.0:/usr/lib/x86_64-linux-gnu/libpcsclite.so.1:ro \
  ...
```

## References

- ARIB STD-B32: https://www.arib.or.jp/english/html/overview/doc/6-STD-B32v3_11-3p3-E1.pdf
- ARIB STD-B60: https://www.arib.or.jp/english/html/overview/doc/6-STD-B60_v1_14-E1.pdf
- ARIB STD-B61: https://www.arib.or.jp/english/html/overview/doc/6-STD-B61v1_4-E1.pdf
