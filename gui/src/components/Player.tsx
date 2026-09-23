import { Button, Spinner } from "@heroui/react";
import { type JSX, useEffect, useState } from "react";

import { useServices } from "../api/services";
import { type StreamFailure, useStream } from "../api/stream";
import { StreamErrorKind } from "../gen/chibitv/v1/chibitv_pb";
import { bindMediaSession, publishNowPlaying } from "../player/mediaSession";
import { startPlayback } from "../player/playback";
import { useIsWaitingForMedia } from "../player/readiness";
import { useServiceKey } from "../router";
import { PlayerControls } from "./PlayerControls";

/**
 * What to tell the viewer about the error that stopped the stream.
 *
 * The message the server sends explains it the way the standard numbers it,
 * which says nothing to whoever is only trying to watch television, so the
 * kinds worth knowing about are put in their own words and the message is kept
 * for the ones that are not.
 */
function describeStreamError(error: StreamFailure): string {
  switch (error.kind) {
    case StreamErrorKind.NOT_CONTRACTED:
      return "This programme cannot be descrambled: the card holds no contract for it.";
    case StreamErrorKind.DESCRAMBLING_REFUSED:
      return "The card handed over no key to descramble this programme with.";
    default:
      return error.message || "The stream stopped.";
  }
}

export function Player(): JSX.Element {
  // The controls act on the element, so hold it in state rather than a ref to
  // render them once it is mounted.
  const [video, setVideo] = useState<HTMLVideoElement | null>(null);
  const [error, setError] = useState<string>();
  const { state, error: streamError, stopped, subscribeFmp4, playbackGeneration, reconnect, retry } = useStream();
  const service = useServiceKey();
  const { data: services = [] } = useServices();
  const isWaitingForMedia = useIsWaitingForMedia(video);
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
        // The pipeline cannot be resumed where it broke: it has to be built
        // again from an init segment, which only a new connection sends.
        reconnect();
      },
    });
  }, [video, subscribeFmp4, playbackGeneration, reconnect]);

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
      {/* Firefox does not reliably detect the MPEG-2 display aspect ratio, so keep the player explicitly at 16:9
          and letterbox it inside the frame, which a viewport wider than 16:9 would otherwise overflow. */}
      <div className="player-frame grid h-full w-full place-items-center">
        <video ref={setVideo} muted autoPlay playsInline className="player-picture object-fill" />
      </div>
      {video && <PlayerControls video={video} />}
      {/* Tuning, descrambling and transcoding all happen before the first frame
          arrives, and the picture stays black until then, so say it is coming. */}
      {service !== undefined && !error && !streamError && isWaitingForMedia && (
        // The colour lives on the wrapper because the spinner inherits it.
        <div className="pointer-events-none absolute z-10 text-white/70">
          <Spinner aria-label="Loading the picture" color="current" size="lg" />
        </div>
      )}
      {service === undefined && services.length === 0 && (
        <p className="absolute z-10 text-sm text-white/70">No channels are available.</p>
      )}
      {/* The stream stopping is what the viewer is owed an explanation for,
          so it is said over whatever the player itself made of the silence. */}
      {streamError ? (
        <div className="absolute inset-x-4 bottom-20 z-30 flex items-center gap-3 rounded-lg bg-danger/90 p-3 text-sm text-white shadow-lg">
          <p className="min-w-0 flex-1">{describeStreamError(streamError)}</p>
          {/* A stream that is still reconnecting takes itself back up, so
              there is only something to ask for once it has given up. */}
          {stopped && (
            <Button className="shrink-0" size="sm" variant="secondary" onPress={retry}>
              Try again
            </Button>
          )}
        </div>
      ) : (
        error && (
          <div className="absolute inset-x-4 bottom-20 z-30 rounded-lg bg-danger/90 p-3 text-sm text-white shadow-lg">
            {error}
          </div>
        )
      )}
    </div>
  );
}
