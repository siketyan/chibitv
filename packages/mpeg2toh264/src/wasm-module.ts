/**
 * Shapes of the wasm-bindgen module that build.sh generates into wasm/. The
 * artifacts are not part of the TypeScript source tree, so the module is
 * described here and imported dynamically.
 */

export interface TranscodedFragment {
  /**
   * The initialization segment, present on the fragment that describes the
   * stream; append it ahead of this fragment's media. Each read copies the
   * bytes out of WebAssembly memory, so read it once.
   */
  readonly initSegment: Uint8Array | undefined;
  /**
   * The MIME type to open the `SourceBuffer` with, present alongside the
   * initialization segment.
   */
  readonly mimeCodec: string | undefined;
  /** Each read copies the bytes out of WebAssembly memory, so read it once. */
  readonly mediaSegment: Uint8Array;
  /** Where this fragment starts on the presentation timeline, in seconds. */
  readonly startSeconds: number;
  free(): void;
}

export interface WasmTranscoder {
  /** Declare the AAC track by its AudioSpecificConfig before pushing audio. */
  setAudioConfig(audioSpecificConfig: Uint8Array): void;
  /**
   * Feed one MPEG-2 video access unit -- a picture with whatever sequence and
   * GOP headers precede it -- with its 90 kHz presentation timestamp, in
   * decode order.
   */
  pushVideo(accessUnit: Uint8Array, pts: number): TranscodedFragment[];
  /**
   * Feed one AAC access unit (raw, without ADTS framing) with its 90 kHz
   * presentation timestamp.
   */
  pushAudio(accessUnit: Uint8Array, pts: number): void;
  /** Flush the unit still being collected when the stream ends. */
  finish(): TranscodedFragment[];
  /** Units dropped because they would not parse or convert. */
  readonly unitsSkipped: number;
  free(): void;
}

export interface WasmModule {
  default(options: { module_or_path: URL | string }): Promise<unknown>;
  Transcoder: new (oversample: number, recoveryInterval: number) => WasmTranscoder;
}
