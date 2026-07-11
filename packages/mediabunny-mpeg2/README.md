# @chibitv/mediabunny-mpeg2

MPEG-2 video decoder for Mediabunny backed by a minimal FFmpeg WebAssembly build. It decodes 8-bit 4:2:0 MPEG-2,
deinterlaces with BWDIF in `send_frame` mode, and emits progressive Mediabunny `VideoSample` instances. The MPEG-2
sample aspect ratio is preserved through each `VideoFrame`'s display dimensions.

## Usage

Register the decoder in the same global scope in which Mediabunny runs:

```ts
import { registerMpeg2Decoder } from "@chibitv/mediabunny-mpeg2";

registerMpeg2Decoder();
```

The generated FFmpeg module and its WebAssembly binary are loaded from inside the package. Applications do not need
to copy the assets or configure their URLs.

For lower-level integration, `Mpeg2Decoder` is also exported.

## Building

From the repository root:

```bash
pnpm --filter @chibitv/mediabunny-mpeg2 build
pnpm --filter @chibitv/mediabunny-mpeg2 lib:build
```

The TypeScript build writes package entry points and declarations to `dist/`. The FFmpeg build writes
`ffmpeg-mpeg2.js` and `ffmpeg-mpeg2.wasm` to `lib/dist/`. Both generated `dist/` directories and the downloaded
`lib/vendor/` sources are ignored by Git.

The included Mediabunny 1.50.8 patch adds MPEG-2 Visual recognition and packet classification needed by the current
PoC. The repository root applies it through `pnpm-workspace.yaml`; it remains necessary until equivalent MPEG-2
support is available upstream.

## Licensing

The TypeScript integration and C wrapper are MIT-licensed as part of chibiTV. FFmpeg is LGPL-2.1-or-later. See
[`THIRD_PARTY_NOTICES.md`](./THIRD_PARTY_NOTICES.md), the license files generated in `lib/dist/`, and the reproducible
source and build information in [`lib/SOURCE.md`](./lib/SOURCE.md).
