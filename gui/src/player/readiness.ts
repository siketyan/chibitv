import { useEffect, useState } from "react";

/**
 * The events after which the element may have run out of media, or found some.
 *
 * The readiness itself is read off the element rather than inferred from which
 * event arrived, so the order they arrive in does not matter.
 */
const READINESS_EVENTS = [
  "emptied",
  "loadstart",
  "loadeddata",
  "canplay",
  "canplaythrough",
  "playing",
  "waiting",
  "stalled",
  "seeking",
  "seeked",
  "progress",
  "ended",
  "error",
];

/**
 * Whether the element has nothing to play right now: the stream has not filled
 * the buffer yet, or playback ran it dry.
 *
 * A player the viewer paused is not waiting — it still holds the frame it
 * stopped on — so this only reports the times the picture is missing.
 */
export function useIsWaitingForMedia(video: HTMLVideoElement | null): boolean {
  const [isWaiting, setIsWaiting] = useState(true);

  useEffect(() => {
    if (!video) return;

    const sync = () => setIsWaiting(video.readyState < video.HAVE_FUTURE_DATA);
    for (const event of READINESS_EVENTS) {
      video.addEventListener(event, sync);
    }
    sync();

    return () => {
      for (const event of READINESS_EVENTS) {
        video.removeEventListener(event, sync);
      }
    };
  }, [video]);

  return isWaiting;
}
