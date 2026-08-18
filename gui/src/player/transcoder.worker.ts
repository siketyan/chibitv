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
// While probing, the incoming data is retained so that a passthrough stream can be replayed from
// the beginning once the codec is known.
let mode: "probe" | "transcode" | "passthrough" = "probe";
const probedData: Uint8Array[] = [];
let nextChunkId = 1;
const chunkAcknowledgements = new Map<number, () => void>();
let sendChain = Promise.resolve();

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

function enqueueChunk(data: Uint8Array): Promise<void> {
  sendChain = sendChain.then(() => sendChunk(data));
  return sendChain;
}

async function transcode(input: Input, bitrate: number): Promise<void> {
  if (typeof VideoEncoder === "undefined") {
    throw new Error("VideoEncoder is not available in this Dedicated Worker");
  }

  let mimeTypeReady = false;
  const pendingHeaderChunks: Uint8Array[] = [];

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
}

async function passthrough(input: Input): Promise<void> {
  const mimeType = await input.getMimeType();

  mode = "passthrough";
  post({ type: "ready", mimeType });
  for (const chunk of probedData.splice(0)) {
    void enqueueChunk(chunk);
  }
}

async function run(bitrate: number): Promise<void> {
  if (running) {
    throw new Error("A transcoder is already running");
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

  try {
    const videoTrack = await input.getPrimaryVideoTrack();
    if (!videoTrack) {
      throw new Error("The stream does not contain a video track");
    }

    // ISDB-S streams already carry HEVC that the browser can decode, so only MPEG-2 (ISDB-T)
    // is transcoded; anything else is passed through to MSE as-is.
    if ((await videoTrack.getCodec()) === "mpeg2") {
      mode = "transcode";
      probedData.length = 0;
      await transcode(input, bitrate);
    } else {
      await passthrough(input);
    }
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
    const data = new Uint8Array(request.data);
    if (mode === "passthrough") {
      void enqueueChunk(data);
      return;
    }
    if (mode === "probe") {
      probedData.push(data);
    }
    inputController?.enqueue(data);
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
