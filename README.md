<h1 align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="docs/public/logo-dark.svg">
    <img src="docs/public/logo.svg" alt="chibitv" height="96">
  </picture>
</h1>

<h3 align="center">The most easy and lightweight way to watch Japanese TV streams in your home.</h3>
<br />
<img width="4494" height="2362" alt="Screenshot of chibitv" src="https://github.com/user-attachments/assets/da0d5f6c-ca74-4005-a095-27b0b1988ded" />
<br />
<br />

> [!WARNING]
> This software is intended for experimental purposes to understand the standards and is not recommended for any other use.

## Documentation

The documentation is published at https://chibitv.p.s6n.jp/:

- [Getting started](https://chibitv.p.s6n.jp/guide/getting-started) covers the prerequisites, the device
  permissions needed on Linux, configuring `config.toml`, and running the server alongside the GUI.
- The [configuration reference](https://chibitv.p.s6n.jp/reference/configuration) documents every key of
  `config.toml`.
- The [CLI reference](https://chibitv.p.s6n.jp/reference/cli) documents every subcommand and option.
- [Docker](https://chibitv.p.s6n.jp/guide/docker) covers running the published image.

## Development

The instructions below are for building chibitv from a checkout. Running it
also needs a tuner shared by
[tunelithd](https://github.com/siketyan/tunelith), a CAS card and a
`config.toml`, which the
[getting started guide](https://chibitv.p.s6n.jp/guide/getting-started)
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

The CAS code talks to a card over PC/SC, which is the only system library
chibitv links against. On Debian and Ubuntu:

```shell
sudo apt-get install --no-install-recommends libpcsclite-dev
```

On Windows nothing is needed: PC/SC is the system `winscard`.

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
