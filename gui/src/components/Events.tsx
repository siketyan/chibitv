import { ArrowPathIcon, ChevronDownIcon, ChevronLeftIcon, ChevronRightIcon } from "@heroicons/react/24/outline";
import { Button, Spinner } from "@heroui/react";
import { useQuery } from "@tanstack/react-query";
import { type CSSProperties, type JSX, useEffect, useMemo, useRef, useState } from "react";

import { chibitvClient, queryKeys } from "../api";
import type { DateTime, Event } from "../gen/chibitv/v1/chibitv_pb";

const MINUTES_PER_DAY = 24 * 60;
const PIXELS_PER_MINUTE = 1.5;
const SERVICE_WIDTH = 224;
const GUIDE_HEIGHT = MINUTES_PER_DAY * PIXELS_PER_MINUTE;
const HOURS = Array.from({ length: 24 }, (_, hour) => ({
  hour,
  label: `${String(hour).padStart(2, "0")}:00`,
}));

const timeFormatter = new Intl.DateTimeFormat("en-GB", {
  hour: "2-digit",
  minute: "2-digit",
});

const dateFormatter = new Intl.DateTimeFormat("en-GB", {
  day: "numeric",
  month: "short",
  weekday: "short",
});

interface GuideEvent {
  id: number;
  serviceId: number;
  title: string;
  startAt: Date;
  endAt: Date;
}

function toDate(value: DateTime | undefined): Date | undefined {
  if (!value) {
    return undefined;
  }

  return new Date(Number(value.seconds) * 1000 + value.nanos / 1_000_000);
}

function toGuideEvents(events: Event[]): GuideEvent[] {
  return events
    .flatMap((event) => {
      const startAt = toDate(event.startTime);
      const endAt = toDate(event.endTime);
      if (!startAt || !endAt) {
        return [];
      }

      return [{ id: event.id, serviceId: event.serviceId, title: event.title || "Untitled", startAt, endAt }];
    })
    .toSorted((a, b) => a.startAt.valueOf() - b.startAt.valueOf());
}

function toDateKey(date: Date): string {
  const year = date.getFullYear();
  const month = String(date.getMonth() + 1).padStart(2, "0");
  const day = String(date.getDate()).padStart(2, "0");
  return `${year}-${month}-${day}`;
}

function fromDateKey(dateKey: string): Date {
  const [year, month, day] = dateKey.split("-").map(Number);
  return new Date(year, month - 1, day);
}

export function Events(): JSX.Element {
  const now = new Date();
  const todayKey = toDateKey(now);
  const [refreshedEvents, setRefreshedEvents] = useState<Map<string, Event>>(new Map());
  const [requestedDateKey, setRequestedDateKey] = useState<string>();
  const [expandedChannelIds, setExpandedChannelIds] = useState<Set<number>>(new Set());
  const [isRefreshing, setIsRefreshing] = useState(false);
  const [refreshError, setRefreshError] = useState<string>();
  const refreshAbortController = useRef<AbortController>(null);
  const { data: channels = [] } = useQuery({
    queryKey: queryKeys.channels,
    queryFn: async () => (await chibitvClient.listChannels({})).channels,
  });
  const { data: services = [] } = useQuery({
    queryKey: queryKeys.services,
    queryFn: async () => (await chibitvClient.listServices({})).services,
  });
  const { data: initialEvents = [] } = useQuery({
    queryKey: queryKeys.events(),
    queryFn: async () => (await chibitvClient.listEvents({})).events,
  });

  useEffect(() => {
    return () => refreshAbortController.current?.abort();
  }, []);

  const refresh = async () => {
    refreshAbortController.current?.abort();
    const abortController = new AbortController();
    refreshAbortController.current = abortController;
    setRefreshError(undefined);
    setIsRefreshing(true);

    try {
      for await (const event of chibitvClient.refreshEvents({}, { signal: abortController.signal })) {
        setRefreshedEvents((current) => {
          const next = new Map(current);
          next.set(`${event.serviceId}:${event.id}`, event);
          return next;
        });
      }
    } catch (error) {
      if (!abortController.signal.aborted) {
        setRefreshError(error instanceof Error ? error.message : String(error));
      }
    } finally {
      if (refreshAbortController.current === abortController) {
        refreshAbortController.current = null;
        setIsRefreshing(false);
      }
    }
  };

  const allEvents = useMemo(() => {
    const events = new Map(initialEvents.map((event) => [`${event.serviceId}:${event.id}`, event]));
    for (const [key, event] of refreshedEvents) {
      events.set(key, event);
    }
    return toGuideEvents([...events.values()]);
  }, [initialEvents, refreshedEvents]);
  const eventsByServiceId = useMemo(() => {
    const grouped = new Map<number, GuideEvent[]>();
    for (const event of allEvents) {
      const events = grouped.get(event.serviceId) ?? [];
      events.push(event);
      grouped.set(event.serviceId, events);
    }
    return grouped;
  }, [allEvents]);

  const channelGroups = useMemo(
    () =>
      channels.map((channel) => {
        const channelServices = services
          .filter((service) => service.channelId === channel.id)
          .map((service) => ({
            serviceId: service.id,
            serviceName: service.name,
            events: eventsByServiceId.get(service.id) ?? [],
          }));

        return {
          channel,
          canExpand: channelServices.length > 1,
          isExpanded: expandedChannelIds.has(channel.id),
          services: expandedChannelIds.has(channel.id) ? channelServices : channelServices.slice(0, 1),
        };
      }),
    [channels, services, eventsByServiceId, expandedChannelIds],
  );
  const eventDateKeys = allEvents.flatMap((event) => [
    toDateKey(event.startAt),
    toDateKey(new Date(event.endAt.valueOf() - 1)),
  ]);
  const dateKeys = [...new Set([todayKey, ...eventDateKeys])].toSorted();
  const selectedDateKey = requestedDateKey && dateKeys.includes(requestedDateKey) ? requestedDateKey : todayKey;
  const selectedPageIndex = dateKeys.indexOf(selectedDateKey);
  const selectedDate = fromDateKey(selectedDateKey);
  const dayEnd = new Date(selectedDate);
  dayEnd.setDate(dayEnd.getDate() + 1);
  const nowOffset = (now.valueOf() - selectedDate.valueOf()) / 60_000;
  const showNow = selectedDateKey === todayKey && nowOffset >= 0 && nowOffset < MINUTES_PER_DAY;

  return (
    <div className="flex min-h-0 flex-1 flex-col overflow-hidden">
      <div className="flex shrink-0 items-center gap-3 border-b border-white/10 px-3 py-2">
        <h2 className="mr-auto font-semibold">Program guide</h2>
        {isRefreshing && (
          <div className="flex items-center gap-2 text-xs text-muted">
            <Spinner size="sm" />
            Refreshing events
          </div>
        )}
        {refreshError && <p className="max-w-80 truncate text-xs text-danger">{refreshError}</p>}
        <Button
          aria-label="Refresh events"
          isDisabled={isRefreshing}
          isIconOnly
          size="sm"
          variant="ghost"
          onPress={() => void refresh()}
        >
          <ArrowPathIcon />
        </Button>
        <Button
          aria-label="Previous day"
          isDisabled={selectedPageIndex <= 0}
          isIconOnly
          size="sm"
          variant="ghost"
          onPress={() => setRequestedDateKey(dateKeys[selectedPageIndex - 1])}
        >
          <ChevronLeftIcon />
        </Button>
        <time className="min-w-24 text-center text-sm font-medium" dateTime={selectedDateKey}>
          {dateFormatter.format(selectedDate)}
        </time>
        <Button
          aria-label="Next day"
          isDisabled={selectedPageIndex < 0 || selectedPageIndex >= dateKeys.length - 1}
          isIconOnly
          size="sm"
          variant="ghost"
          onPress={() => setRequestedDateKey(dateKeys[selectedPageIndex + 1])}
        >
          <ChevronRightIcon />
        </Button>
      </div>

      <div className="min-h-0 flex-1 overflow-auto">
        <div className="min-w-max">
          <div className="sticky top-0 z-30 flex h-18 border-b border-white/10 bg-surface/90 backdrop-blur-xl">
            <div className="sticky left-0 z-40 w-16 shrink-0 border-r border-white/10 bg-surface/95" />
            {channelGroups.map(({ channel, services: channelServices, canExpand, isExpanded }) => {
              const laneCount = Math.max(channelServices.length, 1);
              return (
                <div
                  key={channel.id}
                  className="shrink-0 border-r border-white/10"
                  style={{ width: laneCount * SERVICE_WIDTH }}
                >
                  <div className="flex h-8 items-center justify-center gap-1 border-b border-white/10 px-2 text-xs font-semibold">
                    <span className="truncate">{channel.name}</span>
                    {canExpand && (
                      <Button
                        aria-label={isExpanded ? `Collapse ${channel.name}` : `Expand ${channel.name}`}
                        aria-pressed={isExpanded}
                        className="h-5 min-h-5 w-5 min-w-5 shrink-0"
                        isIconOnly
                        size="sm"
                        variant="ghost"
                        onPress={() =>
                          setExpandedChannelIds((current) => {
                            const next = new Set(current);
                            if (isExpanded) {
                              next.delete(channel.id);
                            } else {
                              next.add(channel.id);
                            }
                            return next;
                          })
                        }
                      >
                        {isExpanded ? <ChevronDownIcon /> : <ChevronRightIcon />}
                      </Button>
                    )}
                  </div>
                  <div className="grid" style={{ gridTemplateColumns: `repeat(${laneCount}, minmax(0, 1fr))` }}>
                    {channelServices.length === 0 ? (
                      <div className="truncate px-3 py-2 text-center text-xs text-muted">No services</div>
                    ) : (
                      channelServices.map((service) => (
                        <div
                          key={service.serviceId}
                          className="truncate border-r border-white/5 px-3 py-2 text-center text-xs"
                        >
                          {service.serviceName}
                        </div>
                      ))
                    )}
                  </div>
                </div>
              );
            })}
          </div>

          <div className="flex">
            <TimeAxis />
            {channelGroups.map(({ channel, services: channelServices }) => {
              const laneCount = Math.max(channelServices.length, 1);
              return (
                <div
                  key={channel.id}
                  className="grid shrink-0 border-r border-white/10"
                  style={{
                    width: laneCount * SERVICE_WIDTH,
                    gridTemplateColumns: `repeat(${laneCount}, minmax(0, 1fr))`,
                    height: GUIDE_HEIGHT,
                  }}
                >
                  {channelServices.length === 0 ? (
                    <GuideLane
                      events={[]}
                      dayEnd={dayEnd}
                      dayStart={selectedDate}
                      nowOffset={showNow ? nowOffset : undefined}
                    />
                  ) : (
                    channelServices.map((service) => (
                      <GuideLane
                        key={service.serviceId}
                        events={service.events}
                        dayEnd={dayEnd}
                        dayStart={selectedDate}
                        nowOffset={showNow ? nowOffset : undefined}
                      />
                    ))
                  )}
                </div>
              );
            })}
          </div>
        </div>
      </div>
    </div>
  );
}

function TimeAxis(): JSX.Element {
  return (
    <div
      className="sticky left-0 z-20 w-16 shrink-0 border-r border-white/10 bg-surface/95"
      style={{ height: GUIDE_HEIGHT }}
    >
      {HOURS.map(({ hour, label }) => (
        <time
          key={label}
          className="absolute right-2 -translate-y-1/2 text-xs tabular-nums text-muted"
          style={{ top: hour * 60 * PIXELS_PER_MINUTE }}
        >
          {label}
        </time>
      ))}
    </div>
  );
}

function GuideLane({
  events,
  dayStart,
  dayEnd,
  nowOffset,
}: {
  events: GuideEvent[];
  dayStart: Date;
  dayEnd: Date;
  nowOffset: number | undefined;
}): JSX.Element {
  const guideStyle = {
    height: GUIDE_HEIGHT,
    backgroundImage: "linear-gradient(to bottom, rgb(255 255 255 / 0.08) 1px, transparent 1px)",
    backgroundSize: `100% ${60 * PIXELS_PER_MINUTE}px`,
  } satisfies CSSProperties;

  return (
    <div className="relative border-r border-white/5" style={guideStyle}>
      {events
        .filter((event) => event.startAt < dayEnd && event.endAt > dayStart)
        .map((event) => {
          const visibleStart = new Date(Math.max(event.startAt.valueOf(), dayStart.valueOf()));
          const visibleEnd = new Date(Math.min(event.endAt.valueOf(), dayEnd.valueOf()));
          const top = ((visibleStart.valueOf() - dayStart.valueOf()) / 60_000) * PIXELS_PER_MINUTE;
          const height = ((visibleEnd.valueOf() - visibleStart.valueOf()) / 60_000) * PIXELS_PER_MINUTE;

          return (
            <article
              key={`${event.id}-${event.startAt.toISOString()}`}
              className="absolute inset-x-1 overflow-hidden rounded-lg border border-accent/25 bg-accent-soft/85 px-2 py-1 text-accent-soft-foreground shadow-sm"
              style={{ top, height }}
              title={`${timeFormatter.format(event.startAt)}–${timeFormatter.format(event.endAt)} ${event.title}`}
            >
              <div className="text-[0.65rem] tabular-nums opacity-70">
                {timeFormatter.format(event.startAt)}–{timeFormatter.format(event.endAt)}
              </div>
              <div className="text-xs font-medium leading-4">{event.title}</div>
            </article>
          );
        })}
      {nowOffset !== undefined && (
        <div
          className="pointer-events-none absolute inset-x-0 z-10 border-t border-danger"
          style={{ top: nowOffset * PIXELS_PER_MINUTE }}
        />
      )}
    </div>
  );
}
