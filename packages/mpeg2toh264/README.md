# @chibitv/mpeg2toh264

MPEG-2 映像をブラウザ内で H.264 へ変換するブリッジです。[mpeg2toh264](https://github.com/otya128/mpeg2toh264) のビットストリーム変換 (量子化係数・動きベクトル・参照関係を再利用し、ピクセルを再構成しない) を WebAssembly として利用します。

サーバーが配信する fMP4 から Mediabunny で取り出した MPEG-2 映像のアクセスユニットと AAC のアクセスユニットを PTS 付きで渡すと、`SourceBuffer` にそのまま append できる fMP4 フラグメント (映像 + 音声) が返ります。GOP 分割・変換・2 トラックのタイムライン整列はすべて Rust 側 (`rust/src/lib.rs`) で行います。

```ts
import { createTranscoder } from "@chibitv/mpeg2toh264";

const transcoder = await createTranscoder();
transcoder.setAudioConfig(audioSpecificConfig);
for (const fragment of transcoder.pushVideo(accessUnit, pts90k)) {
  // fragment.mimeCodec / fragment.initSegment / fragment.mediaSegment
}
```

## WebAssembly のビルド

`wasm/` 以下の生成物はリポジトリに含まれません。次でビルドします:

```console
$ rustup target add wasm32-unknown-unknown
$ cargo install wasm-bindgen-cli
$ pnpm wasm:build
$ pnpm build
```

## ライセンス

このパッケージおよび mpeg2toh264 は MIT ライセンスです。
