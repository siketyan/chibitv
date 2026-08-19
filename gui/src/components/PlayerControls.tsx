import {
  ArrowsPointingInIcon,
  ArrowsPointingOutIcon,
  PauseIcon,
  PlayIcon,
  SpeakerWaveIcon,
  SpeakerXMarkIcon,
} from "@heroicons/react/24/outline";
import { Button } from "@heroui/react";
import { type JSX, useEffect, useState } from "react";

import {
  isFullscreen,
  isInPictureInPicture,
  observeFullscreen,
  observePictureInPicture,
  supportsFullscreen,
  supportsPictureInPicture,
  toggleFullscreen,
  togglePictureInPicture,
} from "../player/presentation";

/** The level unmuting restores when the viewer had turned the volume all the way down. */
const RESTORED_VOLUME = 1;

const BUTTON_CLASS = "pointer-events-auto shrink-0 text-white data-[hover=true]:bg-white/15";

/** Heroicons has no Picture-in-Picture glyph, so here is the usual one. */
function PictureInPictureIcon(): JSX.Element {
  return (
    <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth={1.5} aria-hidden="true">
      <rect x="3" y="5" width="18" height="14" rx="2" strokeLinejoin="round" />
      <rect x="12.5" y="11.5" width="7" height="6" rx="1.5" fill="currentColor" stroke="none" />
    </svg>
  );
}

interface PlayerControlsProps {
  video: HTMLVideoElement;
}

export function PlayerControls({ video }: PlayerControlsProps): JSX.Element {
  const [isPaused, setIsPaused] = useState(true);
  const [isMuted, setIsMuted] = useState(true);
  const [volume, setVolume] = useState(1);
  const [isPictureInPicture, setIsPictureInPicture] = useState(false);
  const [isFullscreenActive, setIsFullscreenActive] = useState(false);
  const [canSetVolume, setCanSetVolume] = useState(false);

  useEffect(() => {
    // Safari for iOS keeps `volume` read-only, where a slider would do nothing.
    // The element is muted until the viewer unmutes it, so probing it is silent.
    const original = video.volume;
    video.volume = original > 0 ? 0 : 1;
    setCanSetVolume(video.volume !== original);
    video.volume = original;
  }, [video]);

  useEffect(() => {
    const sync = () => {
      setIsPaused(video.paused);
      setIsMuted(video.muted);
      setVolume(video.volume);
    };

    const events = ["play", "pause", "volumechange", "emptied"];
    for (const event of events) {
      video.addEventListener(event, sync);
    }
    sync();

    return () => {
      for (const event of events) {
        video.removeEventListener(event, sync);
      }
    };
  }, [video]);

  useEffect(() => observePictureInPicture(video, () => setIsPictureInPicture(isInPictureInPicture(video))), [video]);

  useEffect(() => observeFullscreen(video, () => setIsFullscreenActive(isFullscreen(video))), [video]);

  const changePlaying = () => {
    if (video.paused) {
      void video.play().catch(() => {});
    } else {
      video.pause();
    }
  };

  const changeMuted = () => {
    // Unmuting a player the viewer had turned all the way down would stay silent.
    if (video.muted && video.volume === 0) {
      video.volume = RESTORED_VOLUME;
    }
    video.muted = !video.muted;
  };

  const changeVolume = (level: number) => {
    video.volume = level;
    video.muted = level === 0;
  };

  return (
    <div className="pointer-events-none absolute inset-x-0 bottom-0 z-20 bg-gradient-to-t from-black/80 to-transparent pt-10 pad-safe">
      <div className="flex items-center justify-between gap-2 px-3 pb-3 text-white sm:px-5 sm:pb-4">
        <div className="flex min-w-0 items-center gap-2">
          <Button
            aria-label={isPaused ? "Play" : "Pause"}
            className={BUTTON_CLASS}
            isIconOnly
            variant="ghost"
            onPress={changePlaying}
          >
            {isPaused ? <PlayIcon /> : <PauseIcon />}
          </Button>
          <Button
            aria-label={isMuted ? "Unmute" : "Mute"}
            aria-pressed={isMuted}
            className={BUTTON_CLASS}
            isIconOnly
            variant="ghost"
            onPress={changeMuted}
          >
            {isMuted ? <SpeakerXMarkIcon /> : <SpeakerWaveIcon />}
          </Button>
          {canSetVolume && (
            <input
              aria-label="Volume"
              className="pointer-events-auto w-20 accent-white sm:w-28"
              max={1}
              min={0}
              step={0.01}
              type="range"
              value={isMuted ? 0 : volume}
              onChange={(event) => changeVolume(event.target.valueAsNumber)}
            />
          )}
        </div>
        <div className="flex shrink-0 items-center gap-2">
          {supportsPictureInPicture(video) && (
            <Button
              aria-label={isPictureInPicture ? "Leave Picture-in-Picture" : "Watch in Picture-in-Picture"}
              aria-pressed={isPictureInPicture}
              className={BUTTON_CLASS}
              isIconOnly
              variant="ghost"
              onPress={() => void togglePictureInPicture(video).catch(() => {})}
            >
              <PictureInPictureIcon />
            </Button>
          )}
          {supportsFullscreen(video) && (
            <Button
              aria-label={isFullscreenActive ? "Leave fullscreen" : "Watch fullscreen"}
              aria-pressed={isFullscreenActive}
              className={BUTTON_CLASS}
              isIconOnly
              variant="ghost"
              onPress={() => void toggleFullscreen(video).catch(() => {})}
            >
              {isFullscreenActive ? <ArrowsPointingInIcon /> : <ArrowsPointingOutIcon />}
            </Button>
          )}
        </div>
      </div>
    </div>
  );
}
