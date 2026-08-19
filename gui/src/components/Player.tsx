import { type JSX, useEffect, useState } from "react";

import { useStream } from "../api/stream";
import { bindMediaSession, publishNowPlaying } from "../player/mediaSession";
import { startPlayback } from "../player/playback";
import { PlayerControls } from "./PlayerControls";

export function Player(): JSX.Element {
  // The controls act on the element, so hold it in state rather than a ref to
  // render them once it is mounted.
  const [video, setVideo] = useState<HTMLVideoElement | null>(null);
  const [error, setError] = useState<string>();
  const { state, serviceId, hasServices, subscribeFmp4, playbackGeneration } = useStream();
  const serviceName = state?.service?.name;
  const providerName = state?.service?.providerName ?? "";
  const eventTitle = state?.event?.title;

  // biome-ignore lint/correctness/useExhaustiveDependencies: the generation deliberately restarts playback without remounting the video element.
  useEffect(() => {
    if (!video) return;

    setError(undefined);
    return startPlayback(video, subscribeFmp4, {
      onError(playbackError) {
        console.error("Playback failed", playbackError);
        setError(playbackError.message);
      },
    });
  }, [video, subscribeFmp4, playbackGeneration]);

  // The element outlives every playback, so the platform keeps one session for
  // as long as the player is mounted.
  useEffect(() => {
    if (!video) return;

    return bindMediaSession(video);
  }, [video]);

  useEffect(() => {
    publishNowPlaying(
      serviceName ? { title: eventTitle || serviceName, service: serviceName, provider: providerName } : undefined,
    );
  }, [eventTitle, serviceName, providerName]);

  return (
    <div className="relative grid h-full min-h-0 min-w-0 place-items-center overflow-hidden bg-black">
      {/* Firefox does not reliably detect the MPEG-2 display aspect ratio, so keep the player explicitly at 16:9. */}
      <video
        ref={setVideo}
        muted
        autoPlay
        playsInline
        className="aspect-video h-auto max-h-full w-full max-w-full object-fill"
      />
      {video && <PlayerControls video={video} />}
      {serviceId === undefined && !hasServices && (
        <p className="absolute z-10 text-sm text-white/70">No channels are available.</p>
      )}
      {error && (
        <div className="absolute inset-x-4 bottom-20 z-30 rounded-lg bg-danger/90 p-3 text-sm text-white shadow-lg">
          {error}
        </div>
      )}
    </div>
  );
}
