import { createRootRoute, createRoute, createRouter, redirect, useNavigate, useParams } from "@tanstack/react-router";
import { type JSX, useCallback, useEffect } from "react";

import { isSameService, type ServiceKey, useServices } from "./api/services";
import { StreamProvider } from "./api/stream";
import { Page } from "./components/Page";
import { PlayerChromeProvider } from "./player/chrome";

/**
 * The route the watched service is kept in.
 *
 * A service is named by the stream carrying it as well as by its own id, as BS
 * 2K and BS 4K number their services alike.
 *
 * Both are optional parameters instead of a route of its own, so that every
 * service — and the state before one is picked — is served by the same route
 * component. Switching a channel then only updates the parameters, and React
 * keeps the whole page, including the `<video>` element, mounted: a remounted
 * element would start muted again and lose the volume the viewer set.
 */
const SERVICE_PATH = "/streams/{-$streamId}/services/{-$serviceId}";

const rootRoute = createRootRoute();

const indexRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: "/",
  beforeLoad: () => {
    throw redirect({ to: SERVICE_PATH, params: {}, replace: true });
  },
});

const serviceRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: SERVICE_PATH,
  component: Watch,
});

const routeTree = rootRoute.addChildren([indexRoute, serviceRoute]);

export const router = createRouter({ routeTree });

declare module "@tanstack/react-router" {
  interface Register {
    router: typeof router;
  }
}

function Watch(): JSX.Element {
  const service = useServiceKey();

  useDefaultService(service);

  return (
    <StreamProvider service={service}>
      <PlayerChromeProvider>
        <Page />
      </PlayerChromeProvider>
    </StreamProvider>
  );
}

/** The service being watched, as taken from the URL. */
export function useServiceKey(): ServiceKey | undefined {
  const { streamId, serviceId } = useParams({ from: SERVICE_PATH });
  if (streamId === undefined || serviceId === undefined) {
    return undefined;
  }

  const stream = Number(streamId);
  const service = Number(serviceId);

  return Number.isInteger(stream) && Number.isInteger(service) ? { streamId: stream, serviceId: service } : undefined;
}

/** Watches another service, keeping the previous one in the browser history. */
export function useSelectService(): (service: ServiceKey) => void {
  const navigate = useNavigate();

  return useCallback(
    (service: ServiceKey) => void navigate({ to: SERVICE_PATH, params: pathParams(service) }),
    [navigate],
  );
}

function pathParams(service: ServiceKey): { streamId: string; serviceId: string } {
  return { streamId: String(service.streamId), serviceId: String(service.serviceId) };
}

/**
 * Falls back to the first service of the first channel, so that opening the GUI
 * plays something without picking a channel first, and a URL naming a service
 * the server no longer knows does not leave the player stuck.
 */
function useDefaultService(service: ServiceKey | undefined): void {
  const { data: services = [] } = useServices();
  const navigate = useNavigate();

  useEffect(() => {
    if (services.length === 0 || services.some(({ key }) => isSameService(key, service))) {
      return;
    }

    const [first] = [...services].sort(
      (a, b) => a.channelId - b.channelId || (a.key?.serviceId ?? 0) - (b.key?.serviceId ?? 0),
    );
    if (first.key) {
      void navigate({ to: SERVICE_PATH, params: pathParams(first.key), replace: true });
    }
  }, [navigate, service, services]);
}
