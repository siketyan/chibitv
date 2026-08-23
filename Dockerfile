# syntax=docker/dockerfile:1

# Builds the minimal FFmpeg WebAssembly module the GUI decodes MPEG-2 video
# with. The base image must match EMSDK_VERSION in the build script below.
FROM emscripten/emsdk:6.0.8 AS wasm-builder
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

FROM rust:1.98-trixie AS chef
WORKDIR /app
RUN apt-get update \
    && apt-get install --no-install-recommends -y libdvbv5-dev libpcsclite-dev libudev-dev \
    && rm -rf /var/lib/apt/lists/*
RUN cargo install cargo-chef --locked

FROM chef AS planner
COPY Cargo.toml Cargo.lock ./
COPY crates/ crates/
RUN cargo chef prepare --recipe-path recipe.json

FROM chef AS server-builder
# Dependencies are built from the recipe alone, so this layer is reused until
# one of them changes. Nothing is cache mounted, so that the layer is part of
# the image and can be restored from the registry.
COPY --from=planner /app/recipe.json recipe.json
RUN cargo chef cook --release --locked --features gui --recipe-path recipe.json
COPY Cargo.toml Cargo.lock ./
COPY crates/ crates/
COPY proto/ proto/
COPY --from=gui-builder /app/gui/dist/ gui/dist/
RUN cargo build --release --locked --features gui \
    && cp target/release/chibitv /usr/local/bin/chibitv

FROM gcr.io/distroless/cc-debian13:nonroot AS runtime
# Neither libdvbv5 (tuner access) nor libpcsclite (CAS module access) is part
# of the distroless base image. libdvbv5 pulls in libudev and libcap, and
# libpcsclite is a stub that loads the real library at runtime.
COPY --from=server-builder \
    /usr/lib/x86_64-linux-gnu/libdvbv5.so.0* \
    /usr/lib/x86_64-linux-gnu/libudev.so.1* \
    /usr/lib/x86_64-linux-gnu/libcap.so.2* \
    /usr/lib/x86_64-linux-gnu/libpcsclite.so.1* \
    /usr/lib/x86_64-linux-gnu/libpcsclite_real.so.1* \
    /usr/lib/x86_64-linux-gnu/
COPY --from=server-builder /usr/local/bin/chibitv /usr/local/bin/chibitv
# ARIB SI carries wall-clock time in JST, so the broadcast schedule only lines
# up with the clock of a server running on that zone. This is the POSIX form of
# the offset, which needs no time zone database.
ENV TZ=JST-9
# Every subcommand reads ./config.toml, so mount the configuration here.
# Nothing is written back, so the working directory stays owned by root while
# the image runs as the unprivileged user of the base image.
WORKDIR /app
EXPOSE 3001
ENTRYPOINT ["/usr/local/bin/chibitv"]
CMD ["serve"]
