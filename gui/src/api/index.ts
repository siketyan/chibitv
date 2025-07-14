import createFetchClient from "openapi-fetch";
import createClient from "openapi-react-query";

import type { paths } from "./schema.d.ts";

const fetchClient = createFetchClient<paths>({
  baseUrl: "/api",
});

export const $api = createClient(fetchClient);
