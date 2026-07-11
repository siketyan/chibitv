import { CalendarDaysIcon, InformationCircleIcon, QueueListIcon, XMarkIcon } from "@heroicons/react/24/outline";
import { Button, Tooltip } from "@heroui/react";
import type { JSX } from "react";

import { useStream } from "../api/stream";

interface OverlayNavbarProps {
  isChannelsOpen: boolean;
  isScheduleOpen: boolean;
  onChangeChannelsOpen: (open: boolean) => void;
  onChangeScheduleOpen: (open: boolean) => void;
}

export function OverlayNavbar({
  isChannelsOpen,
  isScheduleOpen,
  onChangeChannelsOpen,
  onChangeScheduleOpen,
}: OverlayNavbarProps): JSX.Element {
  const { state } = useStream();
  const event = state?.event;
  const description = event?.description.filter(({ content }) => content.length > 0) ?? [];

  return (
    <nav className="pointer-events-none absolute inset-x-0 top-0 z-30 flex items-start justify-between gap-3 bg-gradient-to-b from-black/80 to-transparent px-3 pb-10 pt-3 text-white sm:px-5 sm:pt-4">
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
    </nav>
  );
}
