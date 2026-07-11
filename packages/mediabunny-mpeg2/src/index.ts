import { registerDecoder } from "mediabunny";

import { Mpeg2Decoder } from "./mpeg2-decoder.js";

export { Mpeg2Decoder } from "./mpeg2-decoder.js";

let registered = false;

/** Registers the MPEG-2 decoder with Mediabunny. */
export function registerMpeg2Decoder(): void {
  if (!registered) {
    registerDecoder(Mpeg2Decoder);
    registered = true;
  }
}
