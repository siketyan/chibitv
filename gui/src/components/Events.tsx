import { ArrowPathIcon, ChevronDownIcon, ChevronLeftIcon, ChevronRightIcon } from "@heroicons/react/24/outline";
import { Button, Tabs } from "@heroui/react";
import { useQuery } from "@tanstack/react-query";
import { type CSSProperties, type JSX, useMemo, useState } from "react";

import { chibitvClient, queryKeys } from "../api";
import { groupByDeliverySystem, useChannels } from "../api/channels";
import { isSameService, type ServiceKey, serviceKeyId, useServices } from "../api/services";
import { isTaskRunning, useRefreshEvents, useTasks } from "../api/tasks";
import { toDate } from "../api/time";
import { type Channel, DeliverySystem, type Event, TaskKind } from "../gen/chibitv/v1/chibitv_pb";
import { EventDetails } from "./EventDetails";

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
  service: ServiceKey;
  title: string;
  startAt: Date;
  endAt: Date;
  /** The event as the server reports it, which the details are read from. */
  event: Event;
}

function toGuideEvents(events: Event[]): GuideEvent[] {
  return events
    .flatMap((event) => {
      const startAt = toDate(event.startTime);
      const endAt = toDate(event.endTime);
      if (!startAt || !endAt || !event.service) {
        return [];
      }

      return [{ id: event.id, service: event.service, title: event.title || "Untitled", startAt, endAt, event }];
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

export function Events({ service, compact = false }: { service?: ServiceKey; compact?: boolean }): JSX.Element {
  const now = new Date();
  const todayKey = toDateKey(now);
  const [requestedDateKey, setRequestedDateKey] = useState<string>();
  const [requestedDeliverySystem, setRequestedDeliverySystem] = useState<DeliverySystem>();
  const [expandedChannelIds, setExpandedChannelIds] = useState<Set<number>>(new Set());
  const [selectedEvent, setSelectedEvent] = useState<GuideEvent>();
  const { data: channels = [] } = useChannels();
  const { data: services = [] } = useServices();

  // One tab per broadcast wave that has channels on it, the guide showing the
  // schedule of one wave at a time. The wave asked for is kept only while it
  // still has channels, so that a guide opened before the channels arrive
  // settles on the first wave rather than on nothing.
  const waves = groupByDeliverySystem(channels);
  const currentChannel = channels.find((channel) =>
    services.some((listed) => listed.channelId === channel.id && isSameService(listed.key, service)),
  );
  const selectedWave =
    waves.find((wave) => wave.id === requestedDeliverySystem)?.id ?? currentChannel?.deliverySystem ?? waves[0]?.id;

  // The pane asks for the watched service; the expanded guide asks for one wave.
  const {
    data: events = [],
    isPending,
    isError,
  } = useQuery({
    queryKey: compact ? queryKeys.events(service) : queryKeys.eventsOfWave(selectedWave ?? DeliverySystem.UNSPECIFIED),
    queryFn: async () =>
      (await chibitvClient.listEvents(compact ? { service } : { deliverySystem: selectedWave })).events,
    enabled: compact ? service !== undefined : selectedWave !== undefined,
  });

  // Refreshing is a background task: this button only starts one, and how it
  // is getting on is shown with every other task rather than here. Starting a
  // second one is refused by the server, so the button waits for the first.
  const refreshEvents = useRefreshEvents();
  const isRefreshing = useTasks().some((task) => task.kind === TaskKind.REFRESH_EVENTS && isTaskRunning(task));

  const allEvents = useMemo(() => toGuideEvents(events), [events]);
  // The services of two streams may share an id, so they are grouped under the
  // whole key rather than under the service id alone.
  const eventsByService = useMemo(() => {
    const grouped = new Map<string, GuideEvent[]>();
    for (const event of allEvents) {
      const events = grouped.get(serviceKeyId(event.service)) ?? [];
      events.push(event);
      grouped.set(serviceKeyId(event.service), events);
    }
    return grouped;
  }, [allEvents]);

  // The lanes of one wave, which is what a tab is filled with. Only the tab
  // that is open renders, so the events of the wave it was asked for are the
  // ones its lanes are filled with.
  const laneGroupsOf = (waveChannels: Channel[]) =>
    waveChannels.map((channel) => {
      const channelServices = services.flatMap((listed) => {
        if (listed.channelId !== channel.id || !listed.key || (compact && !isSameService(listed.key, service))) {
          return [];
        }

        const id = serviceKeyId(listed.key);

        return [{ id, serviceName: listed.name, events: eventsByService.get(id) ?? [] }];
      });

      return {
        channel,
        canExpand: channelServices.length > 1,
        isExpanded: expandedChannelIds.has(channel.id),
        services: expandedChannelIds.has(channel.id) ? channelServices : channelServices.slice(0, 1),
      };
    });
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

  const renderGuide = (waveChannels: Channel[]) => {
    const laneGroups = laneGroupsOf(waveChannels);

    return (
      <div className={compact ? "min-w-0" : "min-w-max"}>
        <div className="sticky top-0 z-30 flex h-18 border-b border-white/10 bg-surface/90 backdrop-blur-xl">
          <div className="sticky left-0 z-40 w-16 shrink-0 border-r border-white/10 bg-surface/95" />
          {laneGroups.map(({ channel, services: channelServices, canExpand, isExpanded }) => {
            const laneCount = Math.max(channelServices.length, 1);
            return (
              <div
                key={channel.id}
                className="shrink-0 border-r border-white/10"
                style={{ width: compact ? "calc(100% - 4rem)" : laneCount * SERVICE_WIDTH }}
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
                      <div key={service.id} className="truncate border-r border-white/5 px-3 py-2 text-center text-xs">
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
          {laneGroups.map(({ channel, services: channelServices }) => {
            const laneCount = Math.max(channelServices.length, 1);
            return (
              <div
                key={channel.id}
                className="grid shrink-0 border-r border-white/10"
                style={{
                  width: compact ? "calc(100% - 4rem)" : laneCount * SERVICE_WIDTH,
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
                    onSelect={setSelectedEvent}
                  />
                ) : (
                  channelServices.map((service) => (
                    <GuideLane
                      key={service.id}
                      events={service.events}
                      dayEnd={dayEnd}
                      dayStart={selectedDate}
                      nowOffset={showNow ? nowOffset : undefined}
                      onSelect={setSelectedEvent}
                    />
                  ))
                )}
              </div>
            );
          })}
        </div>
      </div>
    );
  };

  // The toolbar wraps on narrow screens, and the pane needs only the date controls.
  const titleBar = (
    <div className="flex shrink-0 flex-wrap items-center justify-between gap-2 border-b border-white/10 px-3 py-2">
      {compact || waves.length === 0 ? (
        <div />
      ) : (
        <Tabs.ListContainer className="min-w-0">
          <Tabs.List aria-label="Broadcast waves">
            {waves.map((wave) => (
              <Tabs.Tab key={wave.id} id={wave.id} className="w-auto shrink-0">
                <Tabs.Indicator />
                {wave.label}
              </Tabs.Tab>
            ))}
          </Tabs.List>
        </Tabs.ListContainer>
      )}
      <div className="flex flex-1 items-center justify-end gap-1">
        <Button
          aria-label="Refresh events"
          isDisabled={isRefreshing || refreshEvents.isPending}
          isIconOnly
          size="sm"
          variant="ghost"
          onPress={() => refreshEvents.mutate()}
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
    </div>
  );

  const details = selectedEvent && (
    <EventDetails
      event={selectedEvent.event}
      serviceName={
        services.find((service) => service.key && serviceKeyId(service.key) === serviceKeyId(selectedEvent.service))
          ?.name
      }
      onClose={() => setSelectedEvent(undefined)}
    />
  );

  if (compact) {
    return (
      <div className="flex min-h-0 flex-1 flex-col overflow-hidden">
        {titleBar}
        {details}
        <div className="min-h-0 flex-1 overflow-auto">
          {!service ? (
            <p className="p-3 text-sm text-muted">Select a channel to see its schedule.</p>
          ) : isError ? (
            <p className="p-3 text-sm text-danger">Could not load the schedule.</p>
          ) : isPending ? (
            <p className="p-3 text-sm text-muted">Loading schedule</p>
          ) : !currentChannel || allEvents.length === 0 ? (
            <p className="p-3 text-sm text-muted">No schedule is available for this channel.</p>
          ) : (
            renderGuide([currentChannel])
          )}
        </div>
      </div>
    );
  }

  if (waves.length === 0) {
    return (
      <div className="flex min-h-0 flex-1 flex-col overflow-hidden">
        {titleBar}
        <p className="p-3 text-sm text-muted">No channels are available.</p>
      </div>
    );
  }

  return (
    // The tabs hold the title bar as well as the guide, the list of them being
    // part of it. Their own gap would show as a seam under it, so it goes.
    <Tabs
      className="min-h-0 flex-1 gap-0 overflow-hidden"
      selectedKey={selectedWave}
      onSelectionChange={(key) => setRequestedDeliverySystem(Number(key) as DeliverySystem)}
    >
      {titleBar}
      {details}
      {waves.map((wave) => (
        <Tabs.Panel key={wave.id} id={wave.id} className="min-h-0 flex-1 overflow-auto p-0">
          {renderGuide(wave.channels)}
        </Tabs.Panel>
      ))}
    </Tabs>
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
  onSelect,
}: {
  events: GuideEvent[];
  dayStart: Date;
  dayEnd: Date;
  nowOffset: number | undefined;
  onSelect: (event: GuideEvent) => void;
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
            <button
              key={`${event.id}-${event.startAt.toISOString()}`}
              type="button"
              className="absolute inset-x-1 overflow-hidden rounded-lg border border-accent/25 bg-accent-soft/85 px-2 py-1 text-left text-accent-soft-foreground shadow-sm transition-colors hover:bg-accent-soft focus-visible:outline-2 focus-visible:outline-accent"
              style={{ top, height }}
              title={`${timeFormatter.format(event.startAt)}–${timeFormatter.format(event.endAt)} ${event.title}`}
              onClick={() => onSelect(event)}
            >
              <div className="text-[0.65rem] tabular-nums opacity-70">
                {timeFormatter.format(event.startAt)}–{timeFormatter.format(event.endAt)}
              </div>
              <div className="text-xs font-medium leading-4">{event.title}</div>
            </button>
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
