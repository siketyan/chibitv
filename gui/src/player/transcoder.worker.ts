import { createTranscoder, type TranscodedFragment } from "@chibitv/mpeg2toh264";
import { ALL_FORMATS, EncodedPacketSink, Input, type InputAudioTrack, ReadableStreamSource } from "mediabunny";

import type { TranscoderRequest, TranscoderResponse } from "./protocol";

const PTS_TIMESCALE = 90_000;

const scope = globalThis as unknown as {
  postMessage(message: TranscoderResponse, transfer?: Transferable[]): void;
  addEventListener(type: "message", listener: (event: MessageEvent<TranscoderRequest>) => void): void;
};

let inputController: ReadableStreamDefaultController<Uint8Array> | undefined;
let running = false;
let cancelled = false;
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

async function audioSpecificConfig(track: InputAudioTrack): Promise<Uint8Array> {
  const description = (await track.getDecoderConfig())?.description;
  if (!description) {
    throw new Error("The AAC track does not carry an AudioSpecificConfig");
  }
  if (description instanceof Uint8Array) {
    return description;
  }
  if (ArrayBuffer.isView(description)) {
    return new Uint8Array(description.buffer, description.byteOffset, description.byteLength);
  }
  return new Uint8Array(description);
}

async function run(): Promise<void> {
  if (running) {
    throw new Error("An MPEG-2 transcoder is already running");
  }
  running = true;

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
  const transcoder = await createTranscoder();

  try {
    const videoTrack = await input.getPrimaryVideoTrack();
    if (!videoTrack) {
      throw new Error("The stream does not contain a video track");
    }
    const codec = await videoTrack.getCodec();
    if (codec !== "mpeg2") {
      throw new Error(`Expected MPEG-2 video, but detected ${codec ?? "an unknown codec"}`);
    }

    const audioTrack = await input.getPrimaryAudioTrack();
    if (audioTrack) {
      const audioCodec = await audioTrack.getCodec();
      if (audioCodec !== "aac") {
        throw new Error(`Expected AAC audio, but detected ${audioCodec ?? "an unknown codec"}`);
      }
      transcoder.setAudioConfig(await audioSpecificConfig(audioTrack));
    }

    let mimeTypeReady = false;
    let sendChain = Promise.resolve();
    const emit = (fragments: TranscodedFragment[]): void => {
      for (const fragment of fragments) {
        if (!mimeTypeReady) {
          const mimeType = fragment.mimeCodec;
          if (!mimeType) {
            throw new Error("The first fragment did not declare a MIME type");
          }
          post({ type: "ready", mimeType });
          mimeTypeReady = true;
        }
        const initSegment = fragment.initSegment;
        const mediaSegment = fragment.mediaSegment;
        fragment.free();
        if (initSegment) {
          sendChain = sendChain.then(() => sendChunk(initSegment));
        }
        sendChain = sendChain.then(() => sendChunk(mediaSegment));
      }
    };

    // Both tracks are pushed in presentation order so the audio a fragment
    // carries is always queued before the video unit that closes it.
    const videoSink = new EncodedPacketSink(videoTrack);
    const audioSink = audioTrack ? new EncodedPacketSink(audioTrack) : undefined;
    let video = await videoSink.getFirstPacket();
    let audio = audioSink ? await audioSink.getFirstPacket() : null;
    while (!cancelled && (video || audio)) {
      if (audio && (!video || audio.timestamp <= video.timestamp)) {
        transcoder.pushAudio(audio.data, Math.round(audio.timestamp * PTS_TIMESCALE));
        audio = audioSink ? await audioSink.getNextPacket(audio) : null;
      } else if (video) {
        const fragments = transcoder.pushVideo(video.data, Math.round(video.timestamp * PTS_TIMESCALE));
        if (fragments.length > 0) {
          emit(fragments);
          // Fragments are acknowledged as the main thread appends them, so
          // waiting here keeps the conversion from running ahead of playback.
          await sendChain;
        }
        video = await videoSink.getNextPacket(video);
      }
    }

    if (!cancelled) {
      emit(transcoder.finish());
    }
    await sendChain;
    post({ type: "complete" });
  } finally {
    inputController = undefined;
    transcoder.free();
    input.dispose();
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
    cancelled = true;
    inputController?.close();
    inputController = undefined;
    return;
  }

  void run().catch((error: unknown) => {
    post({
      type: "error",
      error: error instanceof Error ? (error.stack ?? error.message) : String(error),
    });
  });
});
