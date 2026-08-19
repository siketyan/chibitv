# AGENTS.md

This file provides guidance to coding agents (Claude Code and others) when working with code in this repository.

chibitv is an experimental implementation of the ARIB broadcasting standards: it tunes Japanese ISDB-S/ISDB-T
broadcasts, descrambles them, and remuxes them to MPEG-2 TS / MP4 / fragmented MP4, with an HTTP streaming server and
a React GUI on top. See README.md for the CLI subcommands (`live`, `record`, `remux`, `scan`, `status`, `serve`) and
runtime setup (tuner devices, PC/SC, `config.toml`).

## Setup

- `[patch.crates-io]` in the workspace `Cargo.toml` replaces some crates.io dependencies (`cros-codecs`, `dvbv5-sys`, `mpeg2ts`, `shiguredo_mp4`) with forks pinned to a Git revision. To try a local change to one of them, add a `[patch]` override to `.cargo/config.toml` instead of editing the manifest.
- System libraries for the default `dvb` feature and PC/SC: `libdvbv5-dev` and `libpcsclite-dev`.
- JS tooling: Node 24 with pnpm (via corepack); run `pnpm install` at the repo root.

## Commands

Rust (workspace of `crates/*`, edition 2024):

- Build: `cargo build`
- Test all: `cargo test --all-targets` (CI runs exactly this)
- Test one crate: `cargo test -p chibitv_b60`
- Test one test: `cargo test -p chibitv_b60 <test_name>`
- Lint: `cargo clippy --all-targets -- -D warnings` (warnings fail CI)
- Format: `cargo fmt --all` (checked in CI with `--check`)

JS/TS (pnpm workspace: `gui`, `packages/*`):

- Check everything: `pnpm check` at the root (biome + `tsc` for `gui`, `tsc` for `packages/mediabunny-mpeg2`)
- Auto-fix GUI lint/format: `pnpm --filter chibitv fix`
- GUI dev server: `pnpm --filter chibitv dev` (proxies `/api` to the backend at `[::1]:3001`; run `cargo run -- serve` alongside)
- Build: `pnpm build` at the root

Protobuf (`proto/chibitv/v1/chibitv.proto` is the single source for the RPC API):

- After editing the proto, regenerate the TypeScript client: `pnpm --filter chibitv generate` (outputs to `gui/src/gen/`). The Rust side regenerates automatically via `crates/chibitv/build.rs` (connectrpc-build with a vendored protoc).

## Architecture

### Rust crates: one crate per ARIB standard

The library crates map directly to ARIB standard documents and hold the parsing/crypto logic; the `chibitv` binary crate composes them into pipelines:

- `chibitv_b10` — SI tables/descriptors for MPEG-2 TS (ISDB-T) broadcast metadata (STD-B10).
- `chibitv_b24` — character encoding (ARIB extended 8-bit chars / additional symbols) (STD-B24).
- `chibitv_b25` — ISDB-T conditional access: MULTI2 descrambling and the classic CAS card protocol (STD-B25).
- `chibitv_b60` — MMT/TLV container parsing for ISDB-S 4K: TLV packets, compressed IP, MMTP, messages/tables/descriptors, MFU (STD-B60).
- `chibitv_b61` — ISDB-S conditional access: AES-CTR descrambling and the ACAS card protocol; needs the externally provided _Kd_ master key (STD-B61).

### The `chibitv` binary

`crates/chibitv/src/main.rs` dispatches clap subcommands in `src/command/`.

The shared data flow is a pipeline:

1. A tuner source (`tuner/dvb.rs` behind the default `dvb` feature, or `tuner/stdin.rs` / file input) produces a raw stream
2. Demux (`demux.rs`, `mmt.rs` for MMT/TLV, `m2ts.rs` for MPEG-2 TS)
3. CAS descrambling (`cas.rs`, backed by the b25/b61 crates over PC/SC)
4. Remux (`remux.rs`, `mp4.rs`, codec helpers `aac.rs`/`hevc.rs`/`mp2.rs`)
5. Output

`serve` (`server.rs`, `rpc.rs`) runs an axum server exposing the ConnectRPC `ChibitvService` plus the live stream;
`registry.rs`/`stream.rs`/`event_crawler.rs` manage shared tuner/channel state and EPG events.
Configuration is loaded from `./config.toml` in the working directory (`config.rs`; template in `config.toml.example`).

Cargo features on `chibitv`:

- `dvb` (default, Linux DVB tuner support)
- `gui` (embeds the built `gui/dist` into the binary via rust-embed — used only by the Docker image; development keeps GUI and server separate).

### GUI and JS packages

`gui/` is a React 19 + rsbuild + Tailwind/HeroUI app.
It talks to the server with ConnectRPC clients generated into `gui/src/gen/`.
Playback happens client-side in `gui/src/player/`: a worker (`transcoder.worker.ts`) consumes the server stream (`protocol.ts`) and remuxes/decodes it with mediabunny.
The GUI draws its own transport controls (`components/PlayerControls.tsx`), so the `<video>` element carries no `controls` attribute; `player/presentation.ts` wraps Picture-in-Picture and fullscreen including the WebKit variants Safari needs.
`player/mediaSession.ts` publishes the programme on air to the platform controls, deliberately leaving service selection to the GUI.
`packages/mediabunny-mpeg2` supplies the MPEG-2 video decoder as a minimal FFmpeg WebAssembly build (`lib/build.sh`; prebuilt in Docker).
Note that `mediabunny` is patched via `patches/` in the pnpm workspace.
The app is installable as a PWA: `gui/public/` holds the manifest, the icons and the Service Worker (`sw.js` is plain JavaScript because it is served verbatim, without passing through the bundler).
`gui/src/pwa.ts` registers the worker in production builds only.

### Docker

The `Dockerfile` is a multi-stage build: FFmpeg-WASM build → GUI build → Rust build with the `gui` feature → distroless runtime image serving API and GUI from one binary.
CI (`.github/workflows/docker.yml`) pushes to `ghcr.io/siketyan/chibitv` and deploys `main` to a host over Tailscale SSH.

## Git workflow

- Use the Conventional Commits style in Git commits and GitHub PR title. 
- Avoid writing long description in Git commits.
- Create multiple commits when the change is large.
- Use English in commit messages and PR description.
