# FFmpeg source and build provenance

- FFmpeg version: 7.1.1
- Emscripten version: 4.0.15
- Source archive: `vendor/ffmpeg-7.1.1.tar.xz`
- Source SHA-256: `733984395e0dbbe5c046abda2dc49a5544e7e0e1e2366bba849222ae9e3a03b1`
- Generated WASM SHA-256: `b34ca6743fcb9714de8afe40dab9ef754f91680fd471f915583229e375ba6c6d`
- Generated JS SHA-256: `5ae5b1bcae45d06e37156e7b35ee3cddc78facc2756bb11fdaf1156504b85c2d`

The source archive is an unmodified copy downloaded from:

<https://ffmpeg.org/releases/ffmpeg-7.1.1.tar.xz>

The integration adapter is `adapter.c`. Run `build.sh` to reproduce the browser/worker WebAssembly module. The configure
summary must report:

```text
Libraries:
avcodec avfilter avutil

Enabled decoders:
mpeg2video

Enabled encoders:

Enabled parsers:
mpegvideo

Enabled filters:
bwdif

License: LGPL version 2.1 or later
```

The build must also have `CONFIG_GPL=0`, `CONFIG_GPLV3=0`, and `CONFIG_NONFREE=0`.
