import { Bars3Icon, InformationCircleIcon, XMarkIcon } from "@heroicons/react/24/outline";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import clsx from "clsx";
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
  disabled,
  onChangeService,
}: {
  serviceId: number | undefined;
  disabled: boolean;
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
          disabled={disabled}
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
    <div className="min-h-0 w-[480px] overflow-y-auto flex flex-col">
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
  const [isMenuOpen, setIsMenuOpen] = useState(false);

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

  return (
    <div className="h-dvh overflow-hidden flex flex-col">
      <Navbar event={stream.event ?? undefined} isMenuOpen={isMenuOpen} onChangeMenuOpen={setIsMenuOpen} />
      <div className="shrink-0 overflow-x-auto">
        <Channels serviceId={serviceId} disabled={isPending} onChangeService={handleChangeService} />
      </div>
      <div className="min-h-0 flex-1 overflow-hidden grid grid-cols-[minmax(0,1fr)_max-content]">
        <Player streamVersion={streamVersion} />
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
