import { QueryClient, QueryClientProvider } from "@tanstack/react-query";

import { Page } from "./components/Page";

const queryClient = new QueryClient();

export default function App() {
  return (
    <QueryClientProvider client={queryClient}>
      <Page />
    </QueryClientProvider>
  );
}
