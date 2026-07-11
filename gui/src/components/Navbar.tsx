import { CalendarDaysIcon, InformationCircleIcon, XMarkIcon } from "@heroicons/react/24/outline";
import { Button, Link, Tooltip } from "@heroui/react";
import type { JSX } from "react";

import { $api } from "../api";
import { MobileChannels } from "./MobileChannels";

interface NavbarProps {
  isScheduleOpen: boolean;
  onChangeScheduleOpen: (open: boolean) => void;
}

export function Navbar({ isScheduleOpen, onChangeScheduleOpen }: NavbarProps): JSX.Element {
  const { data: stream = {} } = $api.useQuery(
    "get",
    "/streams/{id}",
    { params: { path: { id: 0 } } },
    {
      refetchInterval: 5000,
      refetchIntervalInBackground: true,
    },
  );
  const event = stream.event ?? undefined;
  const description = event?.description.filter(({ content }) => content.length > 0) ?? [];

  return (
    <header className="flex h-16 shrink-0 items-center justify-between gap-3 border-b border-separator bg-surface px-3 sm:px-5">
      <div className="flex min-w-0 items-center gap-2 sm:gap-4">
        <MobileChannels />
        <Link className="shrink-0 text-xl font-bold tracking-tight text-foreground no-underline" href="/">
          chibitv
        </Link>
        {event?.title && <h1 className="truncate text-sm font-medium sm:text-base">{event.title}</h1>}
        {description.length > 0 && (
          <Tooltip delay={0}>
            <Button aria-label="Event details" isIconOnly size="sm" variant="ghost">
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
        isIconOnly
        variant={isScheduleOpen ? "secondary" : "ghost"}
        onPress={() => onChangeScheduleOpen(!isScheduleOpen)}
      >
        {isScheduleOpen ? <XMarkIcon /> : <CalendarDaysIcon />}
      </Button>
    </header>
  );
}
