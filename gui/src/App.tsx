import { QueryClient, QueryClientProvider } from "@tanstack/react-query";

import { StreamProvider } from "./api/stream";
import { Page } from "./components/Page";

const queryClient = new QueryClient();

export default function App() {
  return (
    <QueryClientProvider client={queryClient}>
      <StreamProvider>
        <Page />
      </StreamProvider>
    </QueryClientProvider>
  );
}
