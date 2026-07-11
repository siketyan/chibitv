#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
FFMPEG_VERSION="7.1.1"
EMSDK_VERSION="4.0.15"
EMSDK_IMAGE="emscripten/emsdk:$EMSDK_VERSION"
EMSDK_DIR="$SCRIPT_DIR/vendor/emsdk"
ARCHIVE="$SCRIPT_DIR/vendor/ffmpeg-$FFMPEG_VERSION.tar.xz"

mkdir -p "$SCRIPT_DIR/vendor" "$SCRIPT_DIR/dist"

if [[ ! -f "$ARCHIVE" ]]; then
  curl --fail --location --output "$ARCHIVE" \
    "https://ffmpeg.org/releases/ffmpeg-$FFMPEG_VERSION.tar.xz"
fi

if docker info >/dev/null 2>&1; then
  docker run --rm \
    --user "$(id -u):$(id -g)" \
    --volume "$SCRIPT_DIR:/work" \
    --workdir /work \
    "$EMSDK_IMAGE" \
    bash ./build-in-container.sh "$FFMPEG_VERSION"
else
  if [[ ! -d "$EMSDK_DIR" ]]; then
    git clone --depth 1 --branch "$EMSDK_VERSION" https://github.com/emscripten-core/emsdk.git "$EMSDK_DIR"
  fi
  "$EMSDK_DIR/emsdk" install "$EMSDK_VERSION"
  "$EMSDK_DIR/emsdk" activate "$EMSDK_VERSION"
  # shellcheck disable=SC1091
  source "$EMSDK_DIR/emsdk_env.sh"
  bash "$SCRIPT_DIR/build-in-container.sh" "$FFMPEG_VERSION"
fi
