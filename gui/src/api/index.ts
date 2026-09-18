import { createClient } from "@connectrpc/connect";
import { createConnectTransport } from "@connectrpc/connect-web";

import { ChibitvService, type DeliverySystem } from "../gen/chibitv/v1/chibitv_pb";
import type { ServiceKey } from "./services";

const transport = createConnectTransport({
  baseUrl: `${location.origin}/api`,
});

export const chibitvClient = createClient(ChibitvService, transport);

export const queryKeys = {
  channels: ["channels"] as const,
  services: ["services"] as const,
  // The key of every event is the key of the events of one service, or of one
  // broadcast wave, without what narrows them down, so invalidating the former
  // invalidates the latter as well.
  events: (service?: ServiceKey) =>
    service === undefined ? (["events"] as const) : (["events", service.streamId, service.serviceId] as const),
  eventsOfWave: (deliverySystem: DeliverySystem) => ["events", "wave", deliverySystem] as const,
  tasks: ["tasks"] as const,
  scanResult: ["scan-result"] as const,
};
