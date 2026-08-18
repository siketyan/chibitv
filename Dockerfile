# syntax=docker/dockerfile:1

# Builds the minimal FFmpeg WebAssembly module the GUI decodes MPEG-2 video
# with. The base image must match EMSDK_VERSION in the build script below.
FROM emscripten/emsdk:4.0.15 AS wasm-builder
WORKDIR /work
COPY packages/mediabunny-mpeg2/lib/ ./
RUN FFMPEG_VERSION="$(sed -n 's/^FFMPEG_VERSION="\(.*\)"$/\1/p' build.sh)" \
    && mkdir -p vendor dist \
    && curl --fail --location \
    --output "vendor/ffmpeg-${FFMPEG_VERSION}.tar.xz" \
    "https://ffmpeg.org/releases/ffmpeg-${FFMPEG_VERSION}.tar.xz" \
    && bash ./build-in-container.sh "${FFMPEG_VERSION}"

FROM node:24-trixie-slim AS gui-builder
ENV COREPACK_ENABLE_DOWNLOAD_PROMPT=0
WORKDIR /app
RUN corepack enable
COPY package.json pnpm-lock.yaml pnpm-workspace.yaml ./
COPY patches/ patches/
COPY gui/package.json gui/
COPY packages/mediabunny-mpeg2/package.json packages/mediabunny-mpeg2/
RUN pnpm install --frozen-lockfile
COPY gui/ gui/
COPY packages/ packages/
COPY --from=wasm-builder /work/dist/ packages/mediabunny-mpeg2/lib/dist/
RUN pnpm build

FROM rust:1.96-trixie AS server-builder
WORKDIR /app
RUN apt-get update \
    && apt-get install --no-install-recommends -y libdvbv5-dev libpcsclite-dev libudev-dev \
    && rm -rf /var/lib/apt/lists/*
COPY Cargo.toml Cargo.lock ./
COPY crates/ crates/
COPY proto/ proto/
COPY third_party/ third_party/
COPY --from=gui-builder /app/gui/dist/ gui/dist/
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/usr/local/cargo/git \
    --mount=type=cache,target=/app/target \
    cargo build --release --locked --features gui \
    && cp target/release/chibitv /usr/local/bin/chibitv

FROM gcr.io/distroless/cc-debian13:latest AS runtime
# Neither libdvbv5 (tuner access) nor libpcsclite (CAS module access) is part
# of the distroless base image, and libdvbv5 pulls in libudev and libcap.
COPY --from=server-builder \
    /usr/lib/x86_64-linux-gnu/libdvbv5.so.0* \
    /usr/lib/x86_64-linux-gnu/libudev.so.1* \
    /usr/lib/x86_64-linux-gnu/libcap.so.2* \
    /usr/lib/x86_64-linux-gnu/libpcsclite.so.1* \
    /usr/lib/x86_64-linux-gnu/
COPY --from=server-builder /usr/local/bin/chibitv /usr/local/bin/chibitv
# Every subcommand reads ./config.toml, so mount the configuration here.
WORKDIR /app
EXPOSE 3001
ENTRYPOINT ["/usr/local/bin/chibitv"]
CMD ["serve"]
