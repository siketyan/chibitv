import { Bars3Icon, InformationCircleIcon, XMarkIcon } from "@heroicons/react/24/outline";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import clsx from "clsx";
import mpegts from "mpegts.js";
import { type JSX, useEffect, useRef, useState } from "react";

import { $api } from "./api";

function Navbar({
  event,
  isMenuOpen,
  onChangeMenuOpen,
}: {
  event: { title: string; description: { name: string; content: string }[] } | undefined;
  isMenuOpen: boolean;
  onChangeMenuOpen: (open: boolean) => void;
}): JSX.Element {
  const description = event?.description.filter(({ content }) => content.length > 0) ?? [];

  return (
    <div className="navbar flex items-center justify-between">
      <div className="flex items-center gap-4">
        <a className="btn btn-ghost text-xl" href="/">
          chibitv
        </a>
        <h2>{event?.title}</h2>
        {description.length > 0 && (
          <div className="tooltip tooltip-bottom">
            <div className="tooltip-content text-start p-4 max-w-[512px]">
              <dl className="flex flex-col gap-4">
                {description.map(({ name, content }) => (
                  <div key={name} className="flex flex-col gap-2">
                    <dt className="text-slate-300">{name}</dt>
                    <dd className="leading-5">{content.replaceAll("\r", "\n") || "-"}</dd>
                  </div>
                ))}
              </dl>
            </div>
            <button type="button" className="btn btn-ghost btn-circle">
              <InformationCircleIcon className="size-[1.5rem]" />
            </button>
          </div>
        )}
      </div>
      <label className="btn btn-ghost btn-circle swap swap-rotate">
        <input type="checkbox" checked={isMenuOpen} onChange={(event) => onChangeMenuOpen(event.target.checked)} />
        <Bars3Icon className="swap-off size-[1.5rem]" />
        <XMarkIcon className="swap-on size-[1.5rem]" />
      </label>
    </div>
  );
}

function Channels({
  serviceId,
  onChangeService,
}: {
  serviceId: number | undefined;
  onChangeService: (serviceId: number) => void;
}): JSX.Element {
  const { data: services = [] } = $api.useQuery("get", "/services");

  return (
    <div role="tablist" className="tabs tabs-border">
      {services.map((service) => (
        <button
          key={service.id}
          type="button"
          role="tab"
          className={clsx("tab", service.id === serviceId && "tab-active")}
          onClick={() => {
            onChangeService(service.id);
          }}
        >
          {service.name}
        </button>
      ))}
    </div>
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
  const now = new Date();

  return (
    <div className="w-[480px] overflow-y-auto flex flex-col">
      {events.map((event) => (
        <div
          key={event.id}
          className={clsx(
            "px-4 py-2 flex items-start gap-2",
            event.startAt <= now && event.endAt > now && "bg-blue-700",
          )}
        >
          <div className="flex flex-col items-end gap-1">
            <div className="font-semibold">{timeFormatter.format(event.startAt)}</div>
            <div className="text-slate-300 text-sm">{formatDuration(event.startAt, event.endAt)}</div>
          </div>
          <div>{event.title}</div>
        </div>
      ))}
    </div>
  );
}

function Player(): JSX.Element {
  const ref = useRef<HTMLVideoElement>(null);

  useEffect(() => {
    if (!ref.current) {
      return;
    }

    const player = mpegts.createPlayer(
      {
        type: "mse",
        isLive: true,
        url: new URL("/api/streams/0/stream.ts", location.href).toString(),
      },
      {
        enableWorker: true,
        enableWorkerForMSE: true,
        enableStashBuffer: true,
        stashInitialSize: 2048 * 1024,
        isLive: true,
        liveBufferLatencyChasing: true,
      },
    );

    player.attachMediaElement(ref.current);
    player.load();

    return () => {
      player.pause();
      player.unload();
      player.detachMediaElement();
      player.destroy();
    };
  }, []);

  return <video ref={ref} controls muted autoPlay playsInline className="w-full h-full object-contain" />;
}

function Page() {
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
    onSuccess: () => {
      void refetch();
    },
  });

  const serviceId = stream.service?.id;
  const handleChangeService = (serviceId: number) => {
    mutate({ params: { path: { id: 0 } }, body: { service_id: serviceId } });
  };

  const [isMenuOpen, setIsMenuOpen] = useState(false);

  return (
    <div className="h-[100vh] flex flex-col">
      <Navbar event={stream.event ?? undefined} isMenuOpen={isMenuOpen} onChangeMenuOpen={setIsMenuOpen} />
      <Channels serviceId={serviceId} onChangeService={handleChangeService} />
      <div className="min-h-[0] flex-1 grid grid-cols-[1fr_max-content]">
        {!isPending && <Player />}
        {isMenuOpen && serviceId && <Events serviceId={serviceId} />}
      </div>
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
