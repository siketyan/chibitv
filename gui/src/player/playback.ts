import type { TranscoderRequest, TranscoderResponse } from "./protocol";

const AVC_BITRATE = 8_000_000;
const MIN_START_BUFFER_SECONDS = 2;
const MAX_BUFFER_AHEAD_SECONDS = 30;
const RETAIN_BUFFER_BEHIND_SECONDS = 30;
/** How far behind the newest media playback may drift before it is pulled back up to it. */
const MAX_LIVE_DELAY_SECONDS = 5;
/** How far short of the newest media it resumes, so that the decoder is not left with nothing. */
const LIVE_EDGE_MARGIN_SECONDS = 1;
/** What may have left playback behind the newest media the buffer holds. */
const LIVE_DRIFT_EVENTS = ["play", "seeked"];

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

// Safari for iOS exposes MSE only as ManagedMediaSource, and it is also preferable on the other
// Safari platforms because the media stack then manages the buffer under memory pressure.
function resolveMediaSourceConstructor(): MediaSourceConstructor {
  const managed = window.ManagedMediaSource;
  if (managed) return managed;
  if (typeof MediaSource === "undefined") {
    throw new Error("This browser does not support Media Source Extensions");
  }

  return MediaSource;
}

function isManagedMediaSource(mediaSource: MediaSource): mediaSource is ManagedMediaSource {
  const managed = window.ManagedMediaSource;
  return managed !== undefined && mediaSource instanceof managed;
}

class MediaSourcePlayback {
  private readonly mediaSourceConstructor = resolveMediaSourceConstructor();
  private readonly mediaSource = new this.mediaSourceConstructor();
  private readonly objectUrl: string | undefined;
  private readonly sourceOpen: Promise<void>;
  private sourceBuffer: SourceBuffer | undefined;
  private stopped = false;
  private playbackStarted = false;
  private streaming = true;
  private readonly catchUp = () => this.catchUpToLive();

  constructor(private readonly video: HTMLVideoElement) {
    // Nothing in the GUI seeks, but the platform controls, a stray key or a
    // pause taken over the picture all leave playback behind what is on air.
    for (const event of LIVE_DRIFT_EVENTS) {
      video.addEventListener(event, this.catchUp);
    }

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

    if (isManagedMediaSource(this.mediaSource)) {
      // A ManagedMediaSource may only be attached to an element that opts out of remote playback,
      // and it tells us through these events when it wants to be fed.
      video.disableRemotePlayback = true;
      this.mediaSource.addEventListener("startstreaming", () => {
        this.streaming = true;
      });
      this.mediaSource.addEventListener("endstreaming", () => {
        this.streaming = false;
      });
      video.srcObject = this.mediaSource;
    } else {
      this.objectUrl = URL.createObjectURL(this.mediaSource);
      video.src = this.objectUrl;
    }
    video.load();
  }

  async initialize(mimeType: string): Promise<void> {
    if (!this.mediaSourceConstructor.isTypeSupported(mimeType)) {
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
    this.catchUpToLive();
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
    for (const event of LIVE_DRIFT_EVENTS) {
      this.video.removeEventListener(event, this.catchUp);
    }
    try {
      this.sourceBuffer?.abort();
    } catch {
      // The SourceBuffer may already be detached.
    }
    if (this.objectUrl) {
      this.video.removeAttribute("src");
      URL.revokeObjectURL(this.objectUrl);
    } else {
      this.video.srcObject = null;
    }
    this.video.load();
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

  /**
   * Pulls playback back up to the newest media the buffer holds.
   *
   * What is being watched is live, so the buffer only runs ahead of playback
   * while the picture is standing still: a pause, a seek backwards, a tab the
   * browser throttled. Whatever left it behind, the viewer wants what is on air
   * now rather than to sit through the delay, so playback jumps over it.
   *
   * A pause is left alone until it ends, because playback resuming raises one
   * of the events above and comes back through here.
   */
  private catchUpToLive(): void {
    // The SourceBuffer is detached once stopped, when reading it would throw.
    if (this.stopped || !this.playbackStarted || this.video.paused) return;

    const buffered = this.sourceBuffer?.buffered;
    if (!buffered || buffered.length === 0) return;

    const bufferedEnd = buffered.end(buffered.length - 1);
    if (bufferedEnd - this.video.currentTime <= MAX_LIVE_DELAY_SECONDS) return;

    // The newest range alone: an older one holds media playback has already
    // passed, and the gap between them has nothing to play at all.
    const newestStart = buffered.start(buffered.length - 1);
    this.video.currentTime = Math.max(newestStart, bufferedEnd - LIVE_EDGE_MARGIN_SECONDS);
  }

  private async trimOldBuffer(): Promise<void> {
    const sourceBuffer = this.sourceBuffer;
    if (!sourceBuffer || sourceBuffer.buffered.length === 0) return;

    const removeEnd = this.video.currentTime - RETAIN_BUFFER_BEHIND_SECONDS;
    const bufferedStart = sourceBuffer.buffered.start(0);
    if (removeEnd <= bufferedStart + 1) return;

    await waitForSourceBuffer(sourceBuffer, () => sourceBuffer.remove(0, removeEnd));
  }

  private wantsMoreData(): boolean {
    const bufferedAhead = this.getBufferedEnd() - this.video.currentTime;
    if (bufferedAhead > MAX_BUFFER_AHEAD_SECONDS) return false;

    // A ManagedMediaSource asks us to stop feeding it while it is satisfied, but keep the initial
    // buffer flowing so that playback can start even if it never asks for data.
    return this.streaming || bufferedAhead <= MIN_START_BUFFER_SECONDS;
  }

  private async waitForBufferRoom(): Promise<void> {
    while (!this.stopped && this.mediaSource.readyState === "open" && !this.wantsMoreData()) {
      await new Promise((resolve) => window.setTimeout(resolve, 250));
    }
  }
}

export function startPlayback(
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

  worker.addEventListener("error", (event) => fail(new Error(event.message || "Transcoder Worker failed")));
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
