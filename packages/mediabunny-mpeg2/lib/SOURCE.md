# FFmpeg source and build provenance

- FFmpeg version: 9.0.1
- Emscripten version: 6.0.7
- Source archive: `vendor/ffmpeg-9.0.1.tar.xz`
- Source SHA-256: `cf38e0e28c7e5605942c4a77755349b0145804a397af37eb1fb4c77cb237f635`
- Generated WASM SHA-256: `64489dc2070367c3a3f2626c8739a7969fce0883bf5b5a0079009277110ac83c`
- Generated JS SHA-256: `3ce78ed12b2d7b49f7fad946645ebedb53f8a6532488c4a75bfeaf8130d8dde2`

The source archive is an unmodified copy downloaded from:

<https://ffmpeg.org/releases/ffmpeg-9.0.1.tar.xz>

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
