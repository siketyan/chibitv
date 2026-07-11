import { createClient } from "@connectrpc/connect";
import { createConnectTransport } from "@connectrpc/connect-web";

import { ChibitvService } from "../gen/chibitv/v1/chibitv_pb";

const transport = createConnectTransport({
  baseUrl: location.origin,
});

export const chibitvClient = createClient(ChibitvService, transport);

export const queryKeys = {
  services: ["services"] as const,
  events: (serviceId: number) => ["events", serviceId] as const,
};
