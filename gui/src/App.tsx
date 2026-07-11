import {
  CalendarDaysIcon,
  CheckIcon,
  InformationCircleIcon,
  QueueListIcon,
  XMarkIcon,
} from "@heroicons/react/24/outline";
import { Button, Chip, Drawer, Link, ListBox, Spinner, Tooltip, useOverlayState } from "@heroui/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import clsx from "clsx";
import { type JSX, useEffect, useRef, useState } from "react";

import { $api } from "./api";
import type { components } from "./api/schema.d.ts";

type Service = components["schemas"]["Service"];
type CurrentEvent = Pick<components["schemas"]["Event"], "description" | "title">;

function Channels({
  services,
  serviceId,
  isLoading,
  isError,
  disabled,
  onChangeService,
}: {
  services: Service[];
  serviceId: number | undefined;
  isLoading: boolean;
  isError: boolean;
  disabled: boolean;
  onChangeService: (serviceId: number) => void;
}): JSX.Element {
  if (isLoading) {
    return (
      <div className="flex flex-1 items-center justify-center gap-3 text-sm text-muted">
        <Spinner size="sm" />
        Loading channels
      </div>
    );
  }

  if (isError) {
    return <p className="p-3 text-sm text-danger">Could not load channels.</p>;
  }

  if (services.length === 0) {
    return <p className="p-3 text-sm text-muted">No channels are available.</p>;
  }

  return (
    <ListBox
      aria-label="Channels"
      className="gap-1 p-0"
      selectedKeys={serviceId === undefined ? [] : [serviceId]}
      selectionMode="single"
      onSelectionChange={(keys) => {
        if (keys === "all") {
          return;
        }

        const [key] = keys;
        const selectedServiceId = Number(key);
        if (!Number.isNaN(selectedServiceId) && selectedServiceId !== serviceId) {
          onChangeService(selectedServiceId);
        }
      }}
    >
      {services.map((service) => (
        <ListBox.Item
          key={service.id}
          id={service.id}
          className="min-h-12 rounded-xl px-3 data-[selected=true]:bg-accent-soft data-[selected=true]:text-accent-soft-foreground"
          isDisabled={disabled}
          textValue={service.name}
        >
          <div className="flex min-w-0 flex-1 flex-col">
            <span className="truncate text-sm font-medium">{service.name}</span>
            {service.provider_name && <span className="truncate text-xs text-muted">{service.provider_name}</span>}
          </div>
          {disabled && service.id === serviceId ? (
            <Spinner className="ms-auto shrink-0" size="sm" />
          ) : (
            <ListBox.ItemIndicator className="text-accent">
              <CheckIcon className="size-4" />
            </ListBox.ItemIndicator>
          )}
        </ListBox.Item>
      ))}
    </ListBox>
  );
}

type ChannelsProps = Parameters<typeof Channels>[0];

function MobileChannels(props: ChannelsProps): JSX.Element {
  const drawerState = useOverlayState();

  return (
    <Drawer state={drawerState}>
      <Button aria-label="Open channel" className="md:hidden" isIconOnly variant="ghost">
        <QueueListIcon />
      </Button>
      <Drawer.Backdrop variant="blur">
        <Drawer.Content placement="left">
          <Drawer.Dialog>
            <Drawer.CloseTrigger aria-label="Close" />
            <Drawer.Header>
              <Drawer.Heading>Channels</Drawer.Heading>
            </Drawer.Header>
            <Drawer.Body className="mt-3">
              <Channels
                {...props}
                onChangeService={(serviceId) => {
                  props.onChangeService(serviceId);
                  drawerState.close();
                }}
              />
            </Drawer.Body>
          </Drawer.Dialog>
        </Drawer.Content>
      </Drawer.Backdrop>
    </Drawer>
  );
}

function Navbar({
  event,
  isScheduleOpen,
  onChangeScheduleOpen,
  channelsProps,
}: {
  event: CurrentEvent | undefined;
  isScheduleOpen: boolean;
  onChangeScheduleOpen: (open: boolean) => void;
  channelsProps: ChannelsProps;
}): JSX.Element {
  const description = event?.description.filter(({ content }) => content.length > 0) ?? [];

  return (
    <header className="flex h-16 shrink-0 items-center justify-between gap-3 border-b border-separator bg-surface px-3 sm:px-5">
      <div className="flex min-w-0 items-center gap-2 sm:gap-4">
        <MobileChannels {...channelsProps} />
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

function Events({ serviceId }: { serviceId: number }): JSX.Element {
  const now = new Date();
  const { data: events = [] } = $api.useQuery(
    "get",
    "/services/{id}/events",
    { params: { path: { id: serviceId } } },
    {
      select: (events) =>
        events
          .map((event) => {
            if (!event.start_time || !event.end_time || !event.title) return undefined;
            return {
              id: event.id,
              title: event.title,
              startAt: new Date(event.start_time),
              endAt: new Date(event.end_time),
            };
          })
          .filter((event) => event != null)
          .filter((event) => event.endAt >= now)
          .toSorted((a, b) => a.startAt.valueOf() - b.startAt.valueOf()),
    },
  );

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

function Player({ streamVersion }: { streamVersion: number }): JSX.Element {
  const ref = useRef<HTMLVideoElement>(null);

  useEffect(() => {
    const video = ref.current;
    if (!video) {
      return;
    }

    const mimeTypes = [
      'video/mp4; codecs="hev1.2.4.L153.0, mp4a.40.2"',
      'video/mp4; codecs="hev1.2.4.L153.0, mp4a.40.5"',
    ];
    const mimeType = mimeTypes.find((type) => MediaSource.isTypeSupported(type));
    if (!mimeType) {
      console.error("MSE codecs are not supported", mimeTypes);
      return;
    }

    const abortController = new AbortController();
    const mediaSource = new MediaSource();
    const objectUrl = URL.createObjectURL(mediaSource);
    const queue: ArrayBuffer[] = [];
    let sourceBuffer: SourceBuffer | undefined;
    let initialSeekDone = false;

    const seekToBufferedStart = () => {
      if (initialSeekDone || !sourceBuffer || sourceBuffer.buffered.length === 0) {
        return;
      }

      video.currentTime = sourceBuffer.buffered.start(0);
      initialSeekDone = true;
      void video.play().catch(() => {});
    };

    const appendNext = () => {
      if (!sourceBuffer || sourceBuffer.updating || queue.length === 0 || mediaSource.readyState !== "open") {
        seekToBufferedStart();
        return;
      }

      const chunk = queue.shift();
      if (!chunk) {
        return;
      }

      try {
        sourceBuffer.appendBuffer(chunk);
      } catch (error) {
        console.error("appendBuffer failed", error);
        abortController.abort();
      }
    };

    const readStream = async () => {
      const streamUrl = new URL("/api/streams/0/stream.mp4", location.href);
      streamUrl.searchParams.set("v", streamVersion.toString());

      const response = await fetch(streamUrl, {
        signal: abortController.signal,
      });
      if (!response.ok || !response.body) {
        throw new Error(`Stream request failed: ${response.status}`);
      }

      const reader = response.body.getReader();
      while (!abortController.signal.aborted) {
        const { done, value } = await reader.read();
        if (done) {
          break;
        }

        if (value) {
          queue.push(value.buffer.slice(value.byteOffset, value.byteOffset + value.byteLength) as ArrayBuffer);
          appendNext();
        }
      }
    };

    const handleSourceOpen = () => {
      sourceBuffer = mediaSource.addSourceBuffer(mimeType);
      sourceBuffer.mode = "segments";
      sourceBuffer.addEventListener("updateend", appendNext);
      sourceBuffer.addEventListener("error", () => {
        console.error("SourceBuffer error", mediaSource.readyState);
        abortController.abort();
      });

      void readStream().catch((error) => {
        if (!abortController.signal.aborted) {
          console.error("Failed to read stream", error);
        }
      });
    };

    mediaSource.addEventListener("sourceopen", handleSourceOpen, { once: true });
    video.src = objectUrl;
    void video.play().catch(() => {});

    return () => {
      abortController.abort();
      mediaSource.removeEventListener("sourceopen", handleSourceOpen);
      sourceBuffer?.removeEventListener("updateend", appendNext);
      video.removeAttribute("src");
      video.load();
      URL.revokeObjectURL(objectUrl);
    };
  }, [streamVersion]);

  return (
    <div className="min-h-0 min-w-0 overflow-hidden bg-black">
      <video ref={ref} controls muted autoPlay playsInline className="block h-full max-h-full w-full object-contain" />
    </div>
  );
}

function Page() {
  const [streamVersion, setStreamVersion] = useState(0);
  const [isScheduleOpen, setIsScheduleOpen] = useState(false);

  const {
    data: services = [],
    isLoading: areServicesLoading,
    isError: areServicesError,
  } = $api.useQuery("get", "/services");

  const { data: stream = {}, refetch } = $api.useQuery(
    "get",
    "/streams/{id}",
    { params: { path: { id: 0 } } },
    {
      refetchInterval: 5000,
      refetchIntervalInBackground: true,
    },
  );

  const { mutate, isPending } = $api.useMutation("patch", "/streams/{id}", {
    onSuccess: async () => {
      await refetch();
      setStreamVersion((version) => version + 1);
    },
  });

  const serviceId = stream.service?.id;
  const handleChangeService = (serviceId: number) => {
    mutate({ params: { path: { id: 0 } }, body: { service_id: serviceId } });
  };

  const channelsProps: ChannelsProps = {
    services,
    serviceId,
    isLoading: areServicesLoading,
    isError: areServicesError,
    disabled: isPending,
    onChangeService: handleChangeService,
  };

  return (
    <div className="flex h-dvh flex-col overflow-hidden bg-background text-foreground">
      <Navbar
        channelsProps={channelsProps}
        event={stream.event ?? undefined}
        isScheduleOpen={isScheduleOpen}
        onChangeScheduleOpen={setIsScheduleOpen}
      />
      <main
        className={clsx(
          "grid min-h-0 flex-1 overflow-hidden",
          isScheduleOpen
            ? "grid-cols-[minmax(8rem,1fr)_minmax(11rem,40%)] md:grid-cols-[16rem_minmax(8rem,1fr)_minmax(11rem,35%)] lg:grid-cols-[16rem_minmax(8rem,1fr)_24rem]"
            : "grid-cols-[minmax(0,1fr)] md:grid-cols-[16rem_minmax(0,1fr)]",
        )}
      >
        <aside className="hidden min-h-0 flex-col border-r border-separator bg-surface p-3 md:flex">
          <div className="flex items-center justify-between px-2 pb-3 pt-1">
            <h2 className="font-semibold">Channels</h2>
            <span className="text-xs text-muted">{services.length}</span>
          </div>
          <div className="flex min-h-0 flex-1 flex-col overflow-y-auto">
            <Channels {...channelsProps} />
          </div>
        </aside>
        <Player streamVersion={streamVersion} />
        {isScheduleOpen && serviceId !== undefined && (
          <aside className="flex min-h-0 min-w-0 overflow-hidden border-l border-separator bg-surface">
            <Events serviceId={serviceId} />
          </aside>
        )}
      </main>
    </div>
  );
}

const queryClient = new QueryClient();

export default function App() {
  return (
    <QueryClientProvider client={queryClient}>
      <Page />
    </QueryClientProvider>
  );
}
