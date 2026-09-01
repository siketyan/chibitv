import { createClient } from "@connectrpc/connect";
import { createConnectTransport } from "@connectrpc/connect-web";

import { ChibitvService } from "../gen/chibitv/v1/chibitv_pb";

const transport = createConnectTransport({
  baseUrl: `${location.origin}/api`,
});

export const chibitvClient = createClient(ChibitvService, transport);

export const queryKeys = {
  channels: ["channels"] as const,
  services: ["services"] as const,
  // The key of every event is the key of the events of one service without the
  // service, so invalidating the former invalidates the latter as well.
  events: (serviceId?: number) => (serviceId === undefined ? (["events"] as const) : (["events", serviceId] as const)),
  tasks: ["tasks"] as const,
};
