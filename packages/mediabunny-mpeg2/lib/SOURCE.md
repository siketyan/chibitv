# FFmpeg source and build provenance

- FFmpeg version: 7.1.1
- Emscripten version: 6.0.7
- Source archive: `vendor/ffmpeg-7.1.1.tar.xz`
- Source SHA-256: `733984395e0dbbe5c046abda2dc49a5544e7e0e1e2366bba849222ae9e3a03b1`
- Generated WASM SHA-256: `6439c0101d4690b44d79b90b56f51ca1276466f314579c4b8a1b65560a757ee8`
- Generated JS SHA-256: `3ce78ed12b2d7b49f7fad946645ebedb53f8a6532488c4a75bfeaf8130d8dde2`

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
