import { CalendarDaysIcon, InformationCircleIcon, QueueListIcon, XMarkIcon } from "@heroicons/react/24/outline";
import { Button, Tooltip } from "@heroui/react";
import { type JSX, useEffect, useRef, useState } from "react";

import { useStream } from "../api/stream";
import { startMpeg2Playback } from "../player/playback";

interface PlayerProps {
  isChannelsOpen: boolean;
  isScheduleOpen: boolean;
  onChangeChannelsOpen: (open: boolean) => void;
  onChangeScheduleOpen: (open: boolean) => void;
}

export function Player({
  isChannelsOpen,
  isScheduleOpen,
  onChangeChannelsOpen,
  onChangeScheduleOpen,
}: PlayerProps): JSX.Element {
  const ref = useRef<HTMLVideoElement>(null);
  const [error, setError] = useState<string>();
  const { state, subscribeFmp4, playbackGeneration } = useStream();
  const event = state?.event;
  const description = event?.description.filter(({ content }) => content.length > 0) ?? [];

  // biome-ignore lint/correctness/useExhaustiveDependencies: the generation deliberately restarts playback without remounting the video element.
  useEffect(() => {
    const video = ref.current;
    if (!video) return;

    setError(undefined);
    return startMpeg2Playback(video, subscribeFmp4, {
      onError(playbackError) {
        console.error("MPEG-2 playback failed", playbackError);
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
      <div className="pointer-events-none absolute inset-x-0 top-0 z-30 flex items-start justify-between gap-3 bg-gradient-to-b from-black/80 to-transparent px-3 pb-10 pt-3 text-white sm:px-5 sm:pt-4">
        <div className="flex min-w-0 items-center gap-2">
          <Button
            aria-label={isChannelsOpen ? "Close channels" : "Open channels"}
            aria-pressed={isChannelsOpen}
            className="pointer-events-auto shrink-0 text-white data-[hover=true]:bg-white/15"
            isIconOnly
            variant="ghost"
            onPress={() => onChangeChannelsOpen(!isChannelsOpen)}
          >
            {isChannelsOpen ? <XMarkIcon /> : <QueueListIcon />}
          </Button>
          {event?.title && <h1 className="truncate text-sm font-medium drop-shadow sm:text-base">{event.title}</h1>}
          {description.length > 0 && (
            <Tooltip delay={0}>
              <Button
                aria-label="Event details"
                className="pointer-events-auto text-white data-[hover=true]:bg-white/15"
                isIconOnly
                size="sm"
                variant="ghost"
              >
                <InformationCircleIcon />
              </Button>
              <Tooltip.Content className="max-w-lg p-4 text-start" placement="bottom" showArrow>
                <dl className="flex flex-col gap-4">
                  {description.map(({ name, content }) => (
                    <div key={name} className="flex flex-col gap-2">
                      <dt className="text-muted">{name}</dt>
                      <dd className="whitespace-pre-line text-sm leading-5">{content.replaceAll("\r", "\n") || "-"}</dd>
                    </div>
                  ))}
                </dl>
              </Tooltip.Content>
            </Tooltip>
          )}
        </div>
        <div className="flex shrink-0 items-center gap-1">
          <Button
            aria-label={isScheduleOpen ? "Close schedule" : "Open schedule"}
            aria-pressed={isScheduleOpen}
            className="pointer-events-auto shrink-0 text-white data-[hover=true]:bg-white/15"
            isIconOnly
            variant="ghost"
            onPress={() => onChangeScheduleOpen(!isScheduleOpen)}
          >
            {isScheduleOpen ? <XMarkIcon /> : <CalendarDaysIcon />}
          </Button>
        </div>
      </div>
      {error && (
        <div className="absolute inset-x-4 bottom-4 z-30 rounded-lg bg-danger/90 p-3 text-sm text-white shadow-lg">
          {error}
        </div>
      )}
    </div>
  );
}
