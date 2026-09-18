import { type UseQueryResult, useQuery } from "@tanstack/react-query";

import type { Service } from "../gen/chibitv/v1/chibitv_pb";
import { chibitvClient, queryKeys } from ".";

/**
 * Names one service among the ones on air.
 *
 * A service id alone does not: BS 2K and BS 4K number their services alike, so
 * the stream carrying one goes with it. This is the generated `ServiceKey`
 * without the message it belongs to, which is what the URL holds and what the
 * requests below are built from.
 */
export interface ServiceKey {
  streamId: number;
  serviceId: number;
}

export function isSameService(a: ServiceKey | undefined, b: ServiceKey | undefined): boolean {
  return a !== undefined && b !== undefined && a.streamId === b.streamId && a.serviceId === b.serviceId;
}

/** The service as one string, for the places React and HeroUI want a key. */
export function serviceKeyId(key: ServiceKey): string {
  return `${key.streamId}-${key.serviceId}`;
}

/**
 * Lists every service the server knows about.
 *
 * The server discovers them while it tunes, so an empty list is polled until it
 * yields something.
 */
export function useServices(): UseQueryResult<Service[]> {
  return useQuery({
    queryKey: queryKeys.services,
    queryFn: async () => (await chibitvClient.listServices({})).services,
    refetchInterval: (query) => (query.state.data?.length ? false : 1000),
  });
}
