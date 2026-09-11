# chibitv_ffmpeg

Video transcoding for chibitv on a minimal FFmpeg the crate builds itself.

MPEG-2 or HEVC goes in, H.264, HEVC or AV1 comes out, deinterlaced on the way
where broadcast video needs it. A stream already in the codec asked for passes
through untouched. Nothing of FFmpeg shows in the API: callers name codecs,
accelerations and access units, and the timestamps are seconds like elsewhere
in chibitv.

```rust
use chibitv_ffmpeg::{Deinterlace, Transcoder, TranscodeOptions, VideoCodec};

let mut options = TranscodeOptions::new(VideoCodec::H264);
options.deinterlace = Deinterlace::Frame;
let mut transcoder = Transcoder::new(VideoCodec::Mpeg2, options)?;

for packet in access_units {
    transcoder.push(packet)?;
    while let Some(output) = transcoder.pull() {
        // An H.264 access unit in Annex B, parameter sets at every keyframe.
    }
}
transcoder.finish()?;
while let Some(output) = transcoder.pull() {
    // What the encoder held back.
}
```

## Features

Every feature adds something to the FFmpeg build; with none the crate has no
FFmpeg at all and can only pass streams through, so the rest of the workspace
builds without any of the dependencies below.

| Feature        | What it adds                                        | Platforms       | Build dependency                                           |
| -------------- | --------------------------------------------------- | --------------- | ---------------------------------------------------------- |
| `videotoolbox` | Apple VideoToolbox decode, deinterlace and encode   | macOS           | none                                                       |
| `nvidia`       | NVIDIA CUVID decode with deinterlace, NVENC encode  | Linux, Windows  | `ffnvcodec` headers (`nv-codec-headers`)                   |
| `qsv`          | Intel Quick Sync decode, deinterlace and encode     | Linux, Windows  | `libvpl-dev` (and `libva-dev` on Linux)                    |
| `vaapi`        | VA-API decode, deinterlace and encode               | Linux           | `libva-dev`                                                |
| `amf`          | AMD AMF encode after a software decode              | Windows, Linux  | AMF SDK headers, pointed at with `CHIBITV_FFMPEG_AMF_INCLUDE` |
| `x264`         | H.264 software encode (GPL)                         | all             | `libx264-dev`                                              |
| `x265`         | HEVC software encode (GPL)                          | all             | `libx265-dev`                                              |
| `svt-av1`      | AV1 software encode                                 | all             | `libsvtav1enc-dev`                                         |
| `software`     | `x264`, `x265` and `svt-av1`                        |                 |                                                            |

A hardware feature only takes effect on the platforms in its row and is
ignored with a warning elsewhere, so one feature list serves every platform.
`Acceleration::available()` reports what a build ended up with; whether the
machine has the hardware is found out when a `Transcoder` opens, which tries
the accelerations in order and falls back to the next when one does not work.

## Building FFmpeg

`build.rs` fetches the FFmpeg release archive, configures it with nothing but
what the features need, builds it as static libraries and compiles the C shim
in `csrc/` against it. That needs `make`, a C compiler, `nasm` (on x86; the
build works without it but is slow), `curl`, `tar` and `pkg-config`, plus the
development package of each library above.

The archive, sources and builds live in `vendor/` next to this file, which Git
and Docker ignore, so that a build serves every Cargo profile. Environment
variables:

- `CHIBITV_FFMPEG_VENDOR_DIR`: put `vendor/` elsewhere, for instance in a cache.
- `CHIBITV_FFMPEG_SOURCE`: an FFmpeg source tree to build instead of the release archive.
- `CHIBITV_FFMPEG_AMF_INCLUDE`: the directory holding `AMF/` for the `amf` feature.

Only Linux and macOS builds have been exercised. Windows needs the MSYS2
`sh` and `make` on the `PATH` in an MSVC developer shell, since FFmpeg's
`configure` is a shell script.

## Layout

- `csrc/chibitv_ffmpeg.{h,c}`: the only code that includes FFmpeg's headers; a flat C API over decoding, filtering and encoding in the 90 kHz clock.
- `src/sys.rs`: the bindings to that API.
- `src/av.rs`: safe owners of what the shim hands out.
- `src/backend.rs`: what each acceleration decodes, filters and encodes with, and the options it takes. Pure, and tested on its own.
- `src/pipeline.rs`: the decode, filter, encode chain, and the search for an acceleration that works here.
- `src/lib.rs`: the public API.
