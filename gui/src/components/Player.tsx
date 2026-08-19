import { type JSX, useEffect, useRef, useState } from "react";

import { useStream } from "../api/stream";
import { startPlayback } from "../player/playback";

export function Player(): JSX.Element {
  const ref = useRef<HTMLVideoElement>(null);
  const [error, setError] = useState<string>();
  const { serviceId, hasServices, subscribeFmp4, playbackGeneration } = useStream();

  // biome-ignore lint/correctness/useExhaustiveDependencies: the generation deliberately restarts playback without remounting the video element.
  useEffect(() => {
    const video = ref.current;
    if (!video) return;

    setError(undefined);
    return startPlayback(video, subscribeFmp4, {
      onError(playbackError) {
        console.error("Playback failed", playbackError);
        setError(playbackError.message);
      },
    });
  }, [subscribeFmp4, playbackGeneration]);

  return (
    <div className="relative grid h-full min-h-0 min-w-0 place-items-center overflow-hidden bg-black">
      {/* Firefox does not reliably detect the MPEG-2 display aspect ratio, so keep the player explicitly at 16:9. */}
      <video
        ref={ref}
        controls
        muted
        autoPlay
        playsInline
        className="aspect-video h-auto max-h-full w-full max-w-full object-fill"
      />
      {serviceId === undefined && !hasServices && (
        <p className="absolute z-10 text-sm text-white/70">No channels are available.</p>
      )}
      {error && (
        <div className="absolute inset-x-4 bottom-4 z-30 rounded-lg bg-danger/90 p-3 text-sm text-white shadow-lg">
          {error}
        </div>
      )}
    </div>
  );
}
