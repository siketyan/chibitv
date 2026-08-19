import { createRootRoute, createRoute, createRouter, redirect, useNavigate, useParams } from "@tanstack/react-router";
import { type JSX, useCallback, useEffect } from "react";

import { useServices } from "./api/services";
import { StreamProvider } from "./api/stream";
import { Page } from "./components/Page";
import { PlayerChromeProvider } from "./player/chrome";

/**
 * The route the watched service is kept in.
 *
 * The service is an optional parameter instead of a route of its own, so that
 * every service — and the state before one is picked — is served by the same
 * route component. Switching a channel then only updates the parameter, and
 * React keeps the whole page, including the `<video>` element, mounted: a
 * remounted element would start muted again and lose the volume the viewer set.
 */
const SERVICE_PATH = "/services/{-$serviceId}";

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
  const serviceId = useServiceId();

  useDefaultService(serviceId);

  return (
    <StreamProvider serviceId={serviceId}>
      <PlayerChromeProvider>
        <Page />
      </PlayerChromeProvider>
    </StreamProvider>
  );
}

/** The service being watched, as taken from the URL. */
export function useServiceId(): number | undefined {
  const { serviceId } = useParams({ from: SERVICE_PATH });
  if (serviceId === undefined) {
    return undefined;
  }

  const parsed = Number(serviceId);

  return Number.isInteger(parsed) ? parsed : undefined;
}

/** Watches another service, keeping the previous one in the browser history. */
export function useSelectService(): (serviceId: number) => void {
  const navigate = useNavigate();

  return useCallback(
    (serviceId: number) => void navigate({ to: SERVICE_PATH, params: { serviceId: String(serviceId) } }),
    [navigate],
  );
}

/**
 * Falls back to the first service of the first channel, so that opening the GUI
 * plays something without picking a channel first, and a URL naming a service
 * the server no longer knows does not leave the player stuck.
 */
function useDefaultService(serviceId: number | undefined): void {
  const { data: services = [] } = useServices();
  const navigate = useNavigate();

  useEffect(() => {
    if (services.length === 0 || services.some((service) => service.id === serviceId)) {
      return;
    }

    const [first] = [...services].sort((a, b) => a.channelId - b.channelId || a.id - b.id);
    void navigate({ to: SERVICE_PATH, params: { serviceId: String(first.id) }, replace: true });
  }, [navigate, serviceId, services]);
}
