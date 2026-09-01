import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { RouterProvider } from "@tanstack/react-router";

import { TaskWatcher } from "./api/tasks";
import { router } from "./router";

const queryClient = new QueryClient();

export default function App() {
  return (
    <QueryClientProvider client={queryClient}>
      <TaskWatcher />
      <RouterProvider router={router} />
    </QueryClientProvider>
  );
}
