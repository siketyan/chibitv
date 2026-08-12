import type { WasmModule, WasmTranscoder } from "./wasm-module";

export type { TranscodedFragment, WasmTranscoder as Mpeg2ToH264Transcoder } from "./wasm-module";

export interface Mpeg2ToH264Options {
  /**
   * H.264 quantisation granularity relative to the MPEG-2 step. Higher is
   * closer to the source at a higher bitrate; mpeg2toh264's default of 2
   * costs about 0.5 dB.
   */
  oversample?: number;
  /**
   * Emit a decoder restart point every this many GOPs, so MSE can evict
   * buffered media and a decoder joining mid-stream has somewhere to begin.
   */
  recoveryInterval?: number;
}

let modulePromise: Promise<WasmModule> | undefined;

function loadModule(): Promise<WasmModule> {
  modulePromise ??= (async () => {
    // @ts-ignore -- generated into wasm/ by build.sh, so it may be absent at check time.
    const module = (await import("../wasm/chibitv_mpeg2toh264.js")) as WasmModule;
    await module.default({
      module_or_path: new URL("../wasm/chibitv_mpeg2toh264_bg.wasm", import.meta.url),
    });
    return module;
  })();
  return modulePromise;
}

export async function createTranscoder(options: Mpeg2ToH264Options = {}): Promise<WasmTranscoder> {
  const module = await loadModule();
  return new module.Transcoder(options.oversample ?? 2, options.recoveryInterval ?? 24);
}
