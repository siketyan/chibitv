import { Chip } from "@heroui/react";
import { useQuery } from "@tanstack/react-query";
import clsx from "clsx";
import type { JSX } from "react";

import { chibitvClient, queryKeys } from "../api";
import { useStream } from "../api/stream";
import type { DateTime } from "../gen/chibitv/v1/chibitv_pb";

const timeFormatter = new Intl.DateTimeFormat("en-GB", {
  hour: "2-digit",
  minute: "2-digit",
});

function formatDuration(startAt: Date, endAt: Date): string {
  const secs = (endAt.valueOf() - startAt.valueOf()) / 1000;
  if (secs < 60) {
    return `${secs}s`;
  }

  const minutes = Math.ceil(secs / 60);
  if (minutes < 60) {
    return `${minutes}m`;
  }

  const hours = Math.ceil(minutes / 60);
  return `${hours}h`;
}

function toDate(value: DateTime | undefined): Date | undefined {
  if (!value) {
    return undefined;
  }

  return new Date(Number(value.seconds) * 1000 + value.nanos / 1_000_000);
}

export function Events(): JSX.Element {
  const now = new Date();
  const { state } = useStream();
  const serviceId = state?.service?.id;
  const { data: events = [] } = useQuery({
    queryKey: queryKeys.events(serviceId ?? 0),
    queryFn: async () => (await chibitvClient.listEvents({ serviceId: serviceId ?? 0 })).events,
    enabled: serviceId !== undefined,
    select: (events) =>
      events
        .flatMap((event) => {
          const startAt = toDate(event.startTime);
          const endAt = toDate(event.endTime);
          if (!startAt || !endAt || !event.title) {
            return [];
          }

          return [{ id: event.id, title: event.title, startAt, endAt }];
        })
        .filter((event) => event.endAt >= now)
        .toSorted((a, b) => a.startAt.valueOf() - b.startAt.valueOf()),
  });

  return (
    <div className="flex min-h-0 flex-1 flex-col overflow-y-auto p-3">
      <div className="flex items-center justify-between px-2 pb-3 pt-1">
        <h2 className="font-semibold">Schedule</h2>
      </div>
      {events.length === 0 ? (
        <p className="px-2 py-8 text-center text-sm text-muted">No schedule available.</p>
      ) : (
        events.map((event) => {
          const isLive = event.startAt <= now && event.endAt > now;

          return (
            <div
              key={event.id}
              className={clsx(
                "flex items-start gap-3 rounded-xl px-3 py-2.5",
                isLive && "bg-accent-soft text-accent-soft-foreground",
              )}
            >
              <div className="flex w-12 shrink-0 flex-col items-end gap-1">
                <div className="font-semibold tabular-nums">{timeFormatter.format(event.startAt)}</div>
                <div className="text-xs text-muted">{formatDuration(event.startAt, event.endAt)}</div>
              </div>
              <div className="min-w-0 flex-1 text-sm leading-5">{event.title}</div>
              {isLive && (
                <Chip color="accent" size="sm" variant="soft">
                  LIVE
                </Chip>
              )}
            </div>
          );
        })
      )}
    </div>
  );
}
