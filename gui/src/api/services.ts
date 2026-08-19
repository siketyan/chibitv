import { type UseQueryResult, useQuery } from "@tanstack/react-query";

import type { Service } from "../gen/chibitv/v1/chibitv_pb";
import { chibitvClient, queryKeys } from ".";

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
