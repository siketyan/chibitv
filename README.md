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
- [Docker](https://siketyan.github.io/chibitv/guide/docker) covers running the published image.

## Development

The instructions below are for building chibitv from a checkout. Running it
also needs a tuner, a CAS card and a `config.toml`, which the
[getting started guide](https://siketyan.github.io/chibitv/guide/getting-started)
covers.

### Toolchains

- **Rust** — the toolchain is pinned in `rust-toolchain.toml`, so
  [rustup](https://rustup.rs/) installs the right version, with `clippy` and
  `rustfmt`, on the first `cargo` invocation.
- **Node.js 24 and pnpm** — pnpm comes from corepack, which ships with Node.js:

  ```shell
  corepack enable
  pnpm install
  ```

`protoc` is vendored by the build, so nothing has to be installed to
regenerate the RPC code.

### System libraries

The default `dvb` feature builds against libdvbv5, and the CAS code talks to a
card over PC/SC. On Debian and Ubuntu:

```shell
sudo apt-get install --no-install-recommends libdvbv5-dev libpcsclite-dev
```

The Docker builder image also installs `libudev-dev`, which libdvbv5 links
against; a desktop distribution normally has it already.

`cargo build --no-default-features` drops libdvbv5, leaving the file and
stdin inputs; PC/SC is not optional and is linked either way.

On Windows neither package is needed: PC/SC is the system `winscard`, and the
`bon` feature is a hand-written binding to BonDriver's vtable, so it needs no
SDK.

### Building the FFmpeg WebAssembly module

`packages/mediabunny-mpeg2` decodes MPEG-2 video with a minimal FFmpeg
WebAssembly build that is not checked in. Build it once before building the
GUI:

```shell
bash packages/mediabunny-mpeg2/lib/build.sh
```

The script builds inside the `emscripten/emsdk` image when Docker is
available, and otherwise clones and activates the emsdk itself. It downloads
the FFmpeg sources into `packages/mediabunny-mpeg2/lib/vendor/` and takes a
while, but the result is cached in `lib/dist/` and only has to be rebuilt when
the build scripts change.

### Commands

Rust:

```shell
cargo build
cargo test --all-targets            # what CI runs, on Linux and Windows
cargo clippy --all-targets -- -D warnings
cargo fmt --all
```

JS/TS:

```shell
pnpm check                          # biome + tsc across the workspace
pnpm build
pnpm --filter chibitv fix           # auto-fix GUI lint and formatting
pnpm --filter @chibitv/docs docs:dev  # preview the documentation site
```

Run the server and the GUI development server side by side; the latter proxies
`/api` to the backend:

```shell
# Terminal 1
cargo run -- serve

# Terminal 2
pnpm --filter chibitv dev
```

`proto/chibitv/v1/chibitv.proto` is the single source for the RPC API. The
Rust side regenerates on build; the TypeScript client is regenerated with
`pnpm --filter chibitv generate`.

## References

- ARIB STD-B32: https://www.arib.or.jp/english/html/overview/doc/6-STD-B32v3_11-3p3-E1.pdf
- ARIB STD-B60: https://www.arib.or.jp/english/html/overview/doc/6-STD-B60_v1_14-E1.pdf
- ARIB STD-B61: https://www.arib.or.jp/english/html/overview/doc/6-STD-B61v1_4-E1.pdf
