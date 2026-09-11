#!/usr/bin/env bash
#
# Builds the minimal FFmpeg that chibitv_ffmpeg links, with nothing but what
# the backends named on the command line need, and installs it under
# `vendor/prefix` next to this script (or `$CHIBITV_FFMPEG_VENDOR_DIR`), where
# rusty_ffmpeg finds it through the `FFMPEG_PKG_CONFIG_PATH` set in the
# workspace's `.cargo/config.toml`. Run it before `cargo build` with any of the
# crate's backend features on; cargo cannot do it, since the bindings are made
# by a dependency's build script that needs FFmpeg before this crate's own
# build script gets to run.
#
# Usage: build.sh [backend...]
#
# Backends: software (x264, x265 and svt-av1), x264, x265, svt-av1,
# videotoolbox (macOS), nvidia (Linux, Windows), qsv (Linux, Windows), vaapi
# (Linux), amf (Windows, Linux), and mpeg2-encoder, which the tests need to
# make their input. A backend named on a platform it does not exist on is
# skipped with a warning, so that one list serves every platform. With no
# argument, software is built, plus videotoolbox on macOS.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
FFMPEG_VERSION="8.1.2"
VENDOR_DIR="${CHIBITV_FFMPEG_VENDOR_DIR:-$SCRIPT_DIR/vendor}"
SOURCE_DIR="${CHIBITV_FFMPEG_SOURCE:-$VENDOR_DIR/ffmpeg-$FFMPEG_VERSION}"
ARCHIVE="$VENDOR_DIR/ffmpeg-$FFMPEG_VERSION.tar.xz"
BUILD_DIR="$VENDOR_DIR/build"
PREFIX="$VENDOR_DIR/prefix"

case "$(uname -s)" in
  Darwin) OS=macos ;;
  Linux) OS=linux ;;
  MINGW* | MSYS* | CYGWIN*) OS=windows ;;
  *) OS=other ;;
esac

if [[ $# -eq 0 ]]; then
  set -- software
  [[ $OS == macos ]] && set -- "$@" videotoolbox
fi

# Takes a backend when the platform has it; the list is what ends up built.
backends=()
want() {
  local backend="$1"
  shift
  if [[ $# -gt 0 ]] && [[ " $* " != *" $OS "* ]]; then
    echo "warning: $backend does not exist on $OS, skipping" >&2
    return
  fi
  backends+=("$backend")
}
for backend in "$@"; do
  case "$backend" in
    software) want x264; want x265; want svt-av1 ;;
    x264 | x265 | svt-av1 | mpeg2-encoder) want "$backend" ;;
    videotoolbox) want videotoolbox macos ;;
    nvidia) want nvidia linux windows ;;
    qsv) want qsv linux windows ;;
    vaapi) want vaapi linux ;;
    amf) want amf windows linux ;;
    *)
      echo "error: unknown backend $backend" >&2
      exit 2
      ;;
  esac
done
has() { [[ " ${backends[*]} " == *" $1 "* ]]; }

# rusty_ffmpeg binds and links every library of FFmpeg, so the ones nothing
# here uses are built too, emptied of everything by --disable-everything.
configure=(
  --prefix="$PREFIX"
  --disable-everything
  --disable-autodetect
  --disable-programs
  --disable-doc
  --disable-debug
  --disable-network
  --disable-iconv
  --disable-shared
  --enable-static
  --enable-pic
  # Converts between pixel formats between the decoder and the encoder.
  --enable-swscale
  # The software decoders, and the input side of every hardware acceleration
  # that assists them.
  --enable-decoder=mpeg2video,hevc
  # Deinterlacing and pixel format conversion in software, and moving
  # pictures between system and device memory.
  --enable-filter=buffer,buffersink,bwdif,format,scale,hwupload,hwdownload
)
if [[ $OS == windows ]]; then
  configure+=(--toolchain=msvc)
fi
# FFmpeg's assembly needs nasm on x86; without it the build is slow but works.
case "$(uname -m)" in
  x86_64 | amd64 | i?86)
    if ! command -v nasm >/dev/null; then
      echo "warning: nasm was not found, so FFmpeg is built without its x86 assembly" >&2
      configure+=(--disable-x86asm)
    fi
    ;;
esac

if has x264 || has x265; then
  configure+=(--enable-gpl)
fi
has x264 && configure+=(--enable-libx264 --enable-encoder=libx264)
has x265 && configure+=(--enable-libx265 --enable-encoder=libx265)
has svt-av1 && configure+=(--enable-libsvtav1 --enable-encoder=libsvtav1)
has mpeg2-encoder && configure+=(--enable-encoder=mpeg2video)
if has videotoolbox; then
  configure+=(
    --enable-videotoolbox
    --enable-hwaccel=mpeg2_videotoolbox,hevc_videotoolbox
    --enable-encoder=h264_videotoolbox,hevc_videotoolbox
    --enable-filter=yadif_videotoolbox,scale_vt
  )
fi
if has nvidia; then
  # The CUVID decoders deinterlace on the GPU themselves, which the NVDEC
  # hardware acceleration of the software decoders does not.
  configure+=(
    --enable-ffnvcodec
    --enable-cuvid
    --enable-nvenc
    --enable-decoder=mpeg2_cuvid,hevc_cuvid
    --enable-encoder=h264_nvenc,hevc_nvenc,av1_nvenc
  )
fi
if has vaapi || { has qsv && [[ $OS == linux ]]; }; then
  configure+=(--enable-vaapi)
fi
if has vaapi; then
  configure+=(
    --enable-hwaccel=mpeg2_vaapi,hevc_vaapi
    --enable-encoder=h264_vaapi,hevc_vaapi,av1_vaapi
    --enable-filter=deinterlace_vaapi,scale_vaapi
  )
fi
if has qsv; then
  configure+=(
    --enable-libvpl
    --enable-decoder=mpeg2_qsv,hevc_qsv
    --enable-encoder=h264_qsv,hevc_qsv,av1_qsv
    --enable-filter=deinterlace_qsv,scale_qsv
  )
  # The device libvpl sits on top of on Windows.
  [[ $OS == windows ]] && configure+=(--enable-d3d11va)
fi
if has amf; then
  configure+=(--enable-amf --enable-encoder=h264_amf,hevc_amf,av1_amf)
  if [[ -n ${CHIBITV_FFMPEG_AMF_INCLUDE:-} ]]; then
    configure+=(--extra-cflags="-I$CHIBITV_FFMPEG_AMF_INCLUDE")
  fi
fi

mkdir -p "$VENDOR_DIR"
if [[ ! -f "$SOURCE_DIR/configure" ]]; then
  if [[ ! -f "$ARCHIVE" ]]; then
    curl --fail --location --output "$ARCHIVE.part" \
      "https://ffmpeg.org/releases/ffmpeg-$FFMPEG_VERSION.tar.xz"
    mv "$ARCHIVE.part" "$ARCHIVE"
  fi
  tar -C "$VENDOR_DIR" -xf "$ARCHIVE"
fi

# The same build is not done twice: the stamp records what it was made of.
stamp="ffmpeg $FFMPEG_VERSION for $OS"$'\n'"$(printf '%s\n' "${configure[@]}")"
if [[ -f "$PREFIX/chibitv-stamp" ]] && [[ "$(cat "$PREFIX/chibitv-stamp")" == "$stamp" ]]; then
  echo "FFmpeg $FFMPEG_VERSION with ${backends[*]:-nothing} is already built in $PREFIX"
  exit 0
fi

rm -rf "$BUILD_DIR" "$PREFIX"
mkdir -p "$BUILD_DIR"
cd "$BUILD_DIR"
sh "$SOURCE_DIR/configure" "${configure[@]}"
jobs="$(nproc 2>/dev/null || sysctl -n hw.ncpu 2>/dev/null || echo 1)"
make -j"$jobs"
make install

# What the crate's build script reads to tell what this FFmpeg has.
printf '%s\n' "${backends[@]}" > "$PREFIX/chibitv-backends"
printf '%s' "$stamp" > "$PREFIX/chibitv-stamp"
echo "FFmpeg $FFMPEG_VERSION with ${backends[*]:-nothing} is built in $PREFIX"
