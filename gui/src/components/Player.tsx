import { useQuery } from "@tanstack/react-query";
import { type JSX, useEffect, useRef } from "react";

import { chibitvClient, queryKeys } from "../api";

export function Player(): JSX.Element {
  const ref = useRef<HTMLVideoElement>(null);
  const { data: stream } = useQuery({
    queryKey: queryKeys.stream(0),
    queryFn: () => chibitvClient.getStream({ streamId: 0 }),
  });
  const serviceId = stream?.service?.id;

  useEffect(() => {
    const video = ref.current;
    if (!video || serviceId === undefined) {
      return;
    }

    const mimeTypes = [
      'video/mp4; codecs="hev1.2.4.L153.0, mp4a.40.2"',
      'video/mp4; codecs="hev1.2.4.L153.0, mp4a.40.5"',
    ];
    const mimeType = mimeTypes.find((type) => MediaSource.isTypeSupported(type));
    if (!mimeType) {
      console.error("MSE codecs are not supported", mimeTypes);
      return;
    }

    const abortController = new AbortController();
    const mediaSource = new MediaSource();
    const objectUrl = URL.createObjectURL(mediaSource);
    const queue: ArrayBuffer[] = [];
    let sourceBuffer: SourceBuffer | undefined;
    let initialSeekDone = false;

    const seekToBufferedStart = () => {
      if (initialSeekDone || !sourceBuffer || sourceBuffer.buffered.length === 0) {
        return;
      }

      video.currentTime = sourceBuffer.buffered.start(0);
      initialSeekDone = true;
      void video.play().catch(() => {});
    };

    const appendNext = () => {
      if (!sourceBuffer || sourceBuffer.updating || queue.length === 0 || mediaSource.readyState !== "open") {
        seekToBufferedStart();
        return;
      }

      const chunk = queue.shift();
      if (!chunk) {
        return;
      }

      try {
        sourceBuffer.appendBuffer(chunk);
      } catch (error) {
        console.error("appendBuffer failed", error);
        abortController.abort();
      }
    };

    const readStream = async () => {
      const stream = chibitvClient.streamFmp4({ streamId: 0 }, { signal: abortController.signal });
      for await (const { data } of stream) {
        queue.push(data.buffer.slice(data.byteOffset, data.byteOffset + data.byteLength) as ArrayBuffer);
        appendNext();
      }
    };

    const handleSourceOpen = () => {
      sourceBuffer = mediaSource.addSourceBuffer(mimeType);
      sourceBuffer.mode = "segments";
      sourceBuffer.addEventListener("updateend", appendNext);
      sourceBuffer.addEventListener("error", () => {
        console.error("SourceBuffer error", mediaSource.readyState);
        abortController.abort();
      });

      void readStream().catch((error) => {
        if (!abortController.signal.aborted) {
          console.error("Failed to read stream", error);
        }
      });
    };

    mediaSource.addEventListener("sourceopen", handleSourceOpen, { once: true });
    video.src = objectUrl;
    void video.play().catch(() => {});

    return () => {
      abortController.abort();
      mediaSource.removeEventListener("sourceopen", handleSourceOpen);
      sourceBuffer?.removeEventListener("updateend", appendNext);
      video.removeAttribute("src");
      video.load();
      URL.revokeObjectURL(objectUrl);
    };
  }, [serviceId]);

  return (
    <div className="min-h-0 min-w-0 overflow-hidden bg-black">
      <video ref={ref} controls muted autoPlay playsInline className="block h-full max-h-full w-full object-contain" />
    </div>
  );
}
