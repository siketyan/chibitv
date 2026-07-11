# Third-party notices

## FFmpeg

This package uses code from FFmpeg 7.1.1, licensed under the GNU Lesser General Public License version 2.1 or later.

- Upstream source: <https://ffmpeg.org/releases/ffmpeg-7.1.1.tar.xz>
- Source and build provenance: [`lib/SOURCE.md`](./lib/SOURCE.md)
- License: `lib/dist/COPYING.LGPLv2.1` in generated and published packages
- Build recipe: [`lib/build.sh`](./lib/build.sh) and
  [`lib/build-in-container.sh`](./lib/build-in-container.sh)
- Integration adapter source: [`lib/adapter.c`](./lib/adapter.c)

The configured build reports `LGPL version 2.1 or later`, with `CONFIG_GPL=0`, `CONFIG_GPLV3=0`, and
`CONFIG_NONFREE=0`. It contains the MPEG-2 video decoder, MPEG video parser, and BWDIF filter. It does not contain an
H.264 encoder; H.264 encoding is provided by the browser's WebCodecs implementation.

The build recipe downloads the exact unmodified FFmpeg source archive into the Git-ignored `lib/vendor/` directory.
Its version, URL, and SHA-256 are recorded in `lib/SOURCE.md` so recipients can rebuild and replace the WebAssembly
binary.

## Mediabunny

Mediabunny 1.50.8 is licensed under the Mozilla Public License 2.0. The pnpm patch in the workspace root's `patches/`
directory adds the minimum
MPEG-2 input recognition required by the package's current integration. The modified MPL-covered files remain
available in patch form.

- Upstream: <https://github.com/Vanilagy/mediabunny>
- License: <https://www.mozilla.org/MPL/2.0/>
