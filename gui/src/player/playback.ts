import type { TranscoderRequest, TranscoderResponse } from "./protocol";

const AVC_BITRATE = 8_000_000;
const MIN_START_BUFFER_SECONDS = 2;
const MAX_BUFFER_AHEAD_SECONDS = 30;
const RETAIN_BUFFER_BEHIND_SECONDS = 30;

type SubscribeFmp4 = (listener: (data: Uint8Array) => void) => () => void;

export type PlaybackOptions = {
  onError?: (error: Error) => void;
};

function waitForSourceBuffer(sourceBuffer: SourceBuffer, operation: () => void): Promise<void> {
  return new Promise((resolve, reject) => {
    const cleanup = () => {
      sourceBuffer.removeEventListener("updateend", onUpdateEnd);
      sourceBuffer.removeEventListener("error", onError);
      sourceBuffer.removeEventListener("abort", onAbort);
    };
    const onUpdateEnd = () => {
      cleanup();
      resolve();
    };
    const onError = () => {
      cleanup();
      reject(new Error("SourceBuffer failed while appending transcoded media"));
    };
    const onAbort = () => {
      cleanup();
      reject(new Error("SourceBuffer operation was aborted"));
    };

    sourceBuffer.addEventListener("updateend", onUpdateEnd, { once: true });
    sourceBuffer.addEventListener("error", onError, { once: true });
    sourceBuffer.addEventListener("abort", onAbort, { once: true });
    try {
      operation();
    } catch (error) {
      cleanup();
      reject(error);
    }
  });
}

class MediaSourcePlayback {
  private readonly mediaSource = new MediaSource();
  private readonly objectUrl = URL.createObjectURL(this.mediaSource);
  private readonly sourceOpen: Promise<void>;
  private sourceBuffer: SourceBuffer | undefined;
  private stopped = false;
  private playbackStarted = false;

  constructor(private readonly video: HTMLVideoElement) {
    this.sourceOpen = new Promise((resolve, reject) => {
      this.mediaSource.addEventListener("sourceopen", () => resolve(), { once: true });
      this.mediaSource.addEventListener(
        "sourceclose",
        () => {
          if (!this.stopped) {
            reject(new Error("MediaSource closed before initialization completed"));
          }
        },
        { once: true },
      );
    });

    video.src = this.objectUrl;
    video.load();
  }

  async initialize(mimeType: string): Promise<void> {
    if (!MediaSource.isTypeSupported(mimeType)) {
      throw new Error(`MSE does not support the transcoder output: ${mimeType}`);
    }

    await this.sourceOpen;
    if (this.stopped || this.mediaSource.readyState !== "open") {
      throw new Error("MediaSource is no longer open");
    }

    this.sourceBuffer = this.mediaSource.addSourceBuffer(mimeType);
    this.sourceBuffer.mode = "segments";
    this.mediaSource.duration = Number.POSITIVE_INFINITY;
  }

  async append(data: ArrayBuffer): Promise<void> {
    const sourceBuffer = this.sourceBuffer;
    if (!sourceBuffer) {
      throw new Error("SourceBuffer has not been initialized");
    }
    if (this.stopped || this.mediaSource.readyState !== "open") {
      throw new Error("Cannot append to a closed MediaSource");
    }

    await waitForSourceBuffer(sourceBuffer, () => sourceBuffer.appendBuffer(data));
    await this.startPlaybackWhenReady();
    await this.trimOldBuffer();
    await this.waitForBufferRoom();
  }

  finish(): void {
    if (!this.stopped && this.mediaSource.readyState === "open") {
      this.mediaSource.endOfStream();
    }
  }

  stop(): void {
    if (this.stopped) return;
    this.stopped = true;
    try {
      this.sourceBuffer?.abort();
    } catch {
      // The SourceBuffer may already be detached.
    }
    this.video.removeAttribute("src");
    this.video.load();
    URL.revokeObjectURL(this.objectUrl);
  }

  private getBufferedEnd(): number {
    const buffered = this.sourceBuffer?.buffered;
    return buffered && buffered.length > 0 ? buffered.end(buffered.length - 1) : 0;
  }

  private async startPlaybackWhenReady(): Promise<void> {
    const sourceBuffer = this.sourceBuffer;
    if (this.playbackStarted || !sourceBuffer || sourceBuffer.buffered.length === 0) return;

    const bufferedStart = sourceBuffer.buffered.start(0);
    const bufferedEnd = this.getBufferedEnd();
    if (bufferedEnd - bufferedStart < MIN_START_BUFFER_SECONDS) return;

    this.playbackStarted = true;
    this.video.currentTime = bufferedStart;
    await this.video.play().catch(() => {});
  }

  private async trimOldBuffer(): Promise<void> {
    const sourceBuffer = this.sourceBuffer;
    if (!sourceBuffer || sourceBuffer.buffered.length === 0) return;

    const removeEnd = this.video.currentTime - RETAIN_BUFFER_BEHIND_SECONDS;
    const bufferedStart = sourceBuffer.buffered.start(0);
    if (removeEnd <= bufferedStart + 1) return;

    await waitForSourceBuffer(sourceBuffer, () => sourceBuffer.remove(0, removeEnd));
  }

  private async waitForBufferRoom(): Promise<void> {
    while (
      !this.stopped &&
      this.mediaSource.readyState === "open" &&
      this.getBufferedEnd() - this.video.currentTime > MAX_BUFFER_AHEAD_SECONDS
    ) {
      await new Promise((resolve) => window.setTimeout(resolve, 250));
    }
  }
}

export function startMpeg2Playback(
  video: HTMLVideoElement,
  subscribeFmp4: SubscribeFmp4,
  options: PlaybackOptions = {},
): () => void {
  const worker = new Worker(new URL("./transcoder.worker.ts", import.meta.url), { type: "module" });
  const playback = new MediaSourcePlayback(video);
  let stopped = false;
  let messageChain = Promise.resolve();

  const stop = () => {
    if (stopped) return;
    stopped = true;
    unsubscribe();
    const request: TranscoderRequest = { type: "cancel" };
    worker.postMessage(request);
    worker.terminate();
    playback.stop();
  };

  const fail = (error: unknown) => {
    if (stopped) return;
    const normalized = error instanceof Error ? error : new Error(String(error));
    options.onError?.(normalized);
    stop();
  };

  worker.addEventListener("error", (event) => fail(new Error(event.message || "MPEG-2 transcoder Worker failed")));
  worker.addEventListener("message", (event: MessageEvent<TranscoderResponse>) => {
    if (stopped) return;
    const message = event.data;

    if (message.type === "error") {
      fail(new Error(message.error));
      return;
    }
    if (message.type === "ready") {
      messageChain = messageChain.then(() => playback.initialize(message.mimeType));
    } else if (message.type === "chunk") {
      messageChain = messageChain.then(async () => {
        await playback.append(message.data);
        const acknowledgement: TranscoderRequest = { type: "ack", chunkId: message.chunkId };
        worker.postMessage(acknowledgement);
      });
    } else {
      messageChain = messageChain.then(() => playback.finish());
    }
    messageChain.catch(fail);
  });

  const startRequest: TranscoderRequest = { type: "start", bitrate: AVC_BITRATE };
  worker.postMessage(startRequest);
  const unsubscribe = subscribeFmp4((data) => {
    if (stopped) return;
    const buffer = data.slice().buffer;
    const request: TranscoderRequest = { type: "data", data: buffer };
    worker.postMessage(request, [buffer]);
  });

  return stop;
}
