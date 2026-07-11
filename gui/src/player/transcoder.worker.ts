import { registerMpeg2Decoder } from "@chibitv/mediabunny-mpeg2";
import {
  ALL_FORMATS,
  AppendOnlyStreamTarget,
  Conversion,
  Input,
  Mp4OutputFormat,
  Output,
  ReadableStreamSource,
} from "mediabunny";

import type { TranscoderRequest, TranscoderResponse } from "./protocol";

const scope = globalThis as unknown as {
  postMessage(message: TranscoderResponse, transfer?: Transferable[]): void;
  addEventListener(type: "message", listener: (event: MessageEvent<TranscoderRequest>) => void): void;
};

let conversion: Conversion | undefined;
let inputController: ReadableStreamDefaultController<Uint8Array> | undefined;
let running = false;
let nextChunkId = 1;
const chunkAcknowledgements = new Map<number, () => void>();

function post(message: TranscoderResponse, transfer: Transferable[] = []): void {
  scope.postMessage(message, transfer);
}

function sendChunk(data: Uint8Array): Promise<void> {
  const chunkId = nextChunkId++;
  const buffer = data.slice().buffer;

  return new Promise((resolve) => {
    chunkAcknowledgements.set(chunkId, resolve);
    post({ type: "chunk", chunkId, data: buffer }, [buffer]);
  });
}

async function run(bitrate: number): Promise<void> {
  if (running) {
    throw new Error("An MPEG-2 transcoder is already running");
  }
  running = true;
  registerMpeg2Decoder();

  const stream = new ReadableStream<Uint8Array>({
    start(controller) {
      inputController = controller;
    },
    cancel() {
      inputController = undefined;
    },
  });
  const input = new Input({
    source: new ReadableStreamSource(stream),
    formats: ALL_FORMATS,
  });

  let mimeTypeReady = false;
  const pendingHeaderChunks: Uint8Array[] = [];
  let sendChain = Promise.resolve();
  const enqueueChunk = (data: Uint8Array): Promise<void> => {
    sendChain = sendChain.then(() => sendChunk(data));
    return sendChain;
  };

  const output = new Output({
    format: new Mp4OutputFormat({
      fastStart: "fragmented",
      minimumFragmentDuration: 0.5,
    }),
    target: new AppendOnlyStreamTarget(
      new WritableStream<Uint8Array>({
        write(data) {
          if (!mimeTypeReady) {
            pendingHeaderChunks.push(data.slice());
            return;
          }
          return enqueueChunk(data);
        },
      }),
    ),
  });

  try {
    if (typeof VideoEncoder === "undefined") {
      throw new Error("VideoEncoder is not available in this Dedicated Worker");
    }

    const videoTrack = await input.getPrimaryVideoTrack();
    if (!videoTrack) {
      throw new Error("The stream does not contain a video track");
    }
    const codec = await videoTrack.getCodec();
    if (codec !== "mpeg2") {
      throw new Error(`Expected MPEG-2 video, but detected ${codec ?? "an unknown codec"}`);
    }

    conversion = await Conversion.init({
      input,
      output,
      tracks: "primary",
      video: {
        codec: "avc",
        bitrate,
        keyFrameInterval: 0.5,
        hardwareAcceleration: "prefer-hardware",
        forceTranscode: true,
      },
      showWarnings: false,
    });

    if (!conversion.isValid) {
      const reasons = conversion.discardedTracks.map((track) => track.reason).join("\n");
      throw new Error(`Could not construct the MPEG-2 conversion pipeline.\n${reasons}`);
    }

    const mimeTypePromise = output.getMimeType().then((mimeType) => {
      post({ type: "ready", mimeType });
      mimeTypeReady = true;
      for (const chunk of pendingHeaderChunks) {
        void enqueueChunk(chunk);
      }
      pendingHeaderChunks.length = 0;
    });

    await conversion.execute();
    await mimeTypePromise;
    await sendChain;
    post({ type: "complete" });
  } finally {
    inputController = undefined;
    input.dispose();
    conversion = undefined;
    running = false;
  }
}

scope.addEventListener("message", (event) => {
  const request = event.data;

  if (request.type === "data") {
    inputController?.enqueue(new Uint8Array(request.data));
    return;
  }
  if (request.type === "ack") {
    const acknowledge = chunkAcknowledgements.get(request.chunkId);
    if (acknowledge) {
      chunkAcknowledgements.delete(request.chunkId);
      acknowledge();
    }
    return;
  }
  if (request.type === "cancel") {
    inputController?.close();
    inputController = undefined;
    void conversion?.cancel();
    return;
  }

  void run(request.bitrate).catch((error: unknown) => {
    post({
      type: "error",
      error: error instanceof Error ? (error.stack ?? error.message) : String(error),
    });
  });
});
