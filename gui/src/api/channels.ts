import { type UseQueryResult, useQuery } from "@tanstack/react-query";

import { type Channel, DeliverySystem } from "../gen/chibitv/v1/chibitv_pb";
import { chibitvClient, queryKeys } from ".";

/**
 * The broadcast waves a channel may be carried on, in the order they are
 * offered in, with the name each is known by.
 *
 * The unspecified one is there for a channel the server describes with a wave
 * this build does not know; nothing is filed under it in practice.
 */
export const DELIVERY_SYSTEMS: { id: DeliverySystem; label: string }[] = [
  { id: DeliverySystem.ISDB_T, label: "Terrestrial" },
  { id: DeliverySystem.ISDB_S, label: "BS/CS" },
  { id: DeliverySystem.ISDB_S3, label: "BS 4K" },
  { id: DeliverySystem.UNSPECIFIED, label: "Other" },
];

/** Groups the channels by the wave they are carried on, leaving out the waves nothing is on. */
export function groupByDeliverySystem(
  channels: Channel[],
): { id: DeliverySystem; label: string; channels: Channel[] }[] {
  return DELIVERY_SYSTEMS.flatMap(({ id, label }) => {
    const groupChannels = channels.filter((channel) => channel.deliverySystem === id);

    return groupChannels.length === 0 ? [] : [{ id, label, channels: groupChannels }];
  });
}

/** Lists the channels the server is configured with. */
export function useChannels(): UseQueryResult<Channel[]> {
  return useQuery({
    queryKey: queryKeys.channels,
    queryFn: async () => (await chibitvClient.listChannels({})).channels,
  });
}
