#!/usr/bin/env bash
set -euo pipefail

FFMPEG_VERSION="${1:?FFmpeg version is required}"
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SOURCE_DIR="/tmp/ffmpeg-$FFMPEG_VERSION"
BUILD_DIR="/tmp/ffmpeg-build"

rm -rf "$SOURCE_DIR" "$BUILD_DIR"
tar -C /tmp -xf "$SCRIPT_DIR/vendor/ffmpeg-$FFMPEG_VERSION.tar.xz"
mkdir -p "$BUILD_DIR"
cd "$BUILD_DIR"

emconfigure "$SOURCE_DIR/configure" \
  --cc=emcc \
  --cxx=em++ \
  --ar=emar \
  --ranlib=emranlib \
  --nm=emnm \
  --target-os=none \
  --arch=wasm32 \
  --enable-cross-compile \
  --disable-everything \
  --disable-autodetect \
  --disable-programs \
  --disable-doc \
  --disable-debug \
  --disable-network \
  --disable-pthreads \
  --disable-avdevice \
  --disable-avformat \
  --disable-swresample \
  --disable-swscale \
  --disable-iconv \
  --enable-small \
  --enable-avcodec \
  --enable-decoder=mpeg2video \
  --enable-parser=mpegvideo \
  --enable-avfilter \
  --enable-filter=buffer,buffersink,bwdif \
  --extra-cflags="-O3 -flto -msimd128" \
  --extra-ldflags="-O3 -flto -msimd128"

emmake make -j"$(nproc)"

emcc "$SCRIPT_DIR/adapter.c" \
  -I"$BUILD_DIR" \
  -I"$SOURCE_DIR" \
  -L"$BUILD_DIR/libavfilter" \
  -L"$BUILD_DIR/libavcodec" \
  -L"$BUILD_DIR/libavutil" \
  -lavfilter -lavcodec -lavutil \
  -O3 -flto -msimd128 \
  -s MODULARIZE=1 \
  -s EXPORT_ES6=1 \
  -s ENVIRONMENT=web,worker \
  -s FILESYSTEM=0 \
  -s ALLOW_MEMORY_GROWTH=1 \
  -s INITIAL_MEMORY=33554432 \
  -s STACK_SIZE=1048576 \
  -s ASSERTIONS=0 \
  -s EXPORTED_FUNCTIONS='["_malloc","_free","_mpeg2_decoder_init","_mpeg2_decoder_send","_mpeg2_decoder_flush","_mpeg2_decoder_receive","_mpeg2_frame_width","_mpeg2_frame_height","_mpeg2_frame_plane_pointer","_mpeg2_frame_plane_stride","_mpeg2_frame_pts","_mpeg2_frame_duration","_mpeg2_frame_sar_num","_mpeg2_frame_sar_den","_mpeg2_decoder_error","_mpeg2_decoder_close"]' \
  -s EXPORTED_RUNTIME_METHODS='["UTF8ToString","HEAPU8"]' \
  -o "$SCRIPT_DIR/dist/ffmpeg-mpeg2.js"

cp "$SOURCE_DIR/COPYING.LGPLv2.1" "$SCRIPT_DIR/dist/COPYING.LGPLv2.1"
cp "$SOURCE_DIR/COPYING.LGPLv3" "$SCRIPT_DIR/dist/COPYING.LGPLv3"
