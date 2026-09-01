import { type UseMutationResult, useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { useEffect } from "react";

import { type Task, TaskKind, TaskState } from "../gen/chibitv/v1/chibitv_pb";
import { chibitvClient, queryKeys } from ".";

/** How long the task stream waits before it is opened again after it drops. */
const RECONNECT_DELAY = 1000;

/** Whether the task is still to do its work, or is doing it right now. */
export function isTaskRunning(task: Task): boolean {
  return task.state === TaskState.PENDING || task.state === TaskState.RUNNING;
}

/** Every background task of the server, oldest first. */
export function useTasks(): Task[] {
  const { data = [] } = useQuery({
    queryKey: queryKeys.tasks,
    queryFn: async () => (await chibitvClient.listTasks({})).tasks,
  });

  return data;
}

/**
 * Keeps the tasks up to date from the server as they run.
 *
 * The stream this opens is the only one the app needs for tasks, so it belongs
 * at the root of the app rather than in each component reading the tasks.
 */
export function useWatchTasks(): void {
  const queryClient = useQueryClient();

  useEffect(() => {
    const abortController = new AbortController();

    const watch = async () => {
      while (!abortController.signal.aborted) {
        try {
          for await (const task of chibitvClient.watchTasks({}, { signal: abortController.signal })) {
            queryClient.setQueryData<Task[]>(queryKeys.tasks, (tasks = []) =>
              [...tasks.filter((current) => current.id !== task.id), task].toSorted((a, b) =>
                a.id === b.id ? 0 : a.id < b.id ? -1 : 1,
              ),
            );

            // The crawler stores what it collects as it goes, so the guide is
            // reloaded while the refresh runs and not only once it is over.
            if (task.kind === TaskKind.REFRESH_EVENTS) {
              void queryClient.invalidateQueries({ queryKey: queryKeys.services });
              void queryClient.invalidateQueries({ queryKey: queryKeys.events() });
            }
          }
        } catch {
          // The server went away or the stream broke: it is opened again below,
          // and the tasks it reports then replace the ones shown until now.
        }

        if (abortController.signal.aborted) {
          break;
        }

        await new Promise((resolve) => setTimeout(resolve, RECONNECT_DELAY));
      }
    };

    void watch();

    return () => abortController.abort();
  }, [queryClient]);
}

/**
 * Follows the background tasks for as long as the app is open.
 *
 * It draws nothing of its own: the tasks are shown by whatever reads them.
 */
export function TaskWatcher(): null {
  useWatchTasks();

  return null;
}

/** Starts collecting the programme guide in the background. */
export function useRefreshEvents(): UseMutationResult<Task | undefined, Error, void> {
  const queryClient = useQueryClient();

  return useMutation({
    mutationFn: async () => (await chibitvClient.refreshEvents({})).task,
    // The task is reported by the stream as well, but the list is read again so
    // that the task shows up even while the stream is being opened again.
    onSuccess: () => queryClient.invalidateQueries({ queryKey: queryKeys.tasks }),
  });
}

/** Asks a task to stop, which it may take a moment to act on. */
export function useCancelTask(): UseMutationResult<Task | undefined, Error, Task> {
  const queryClient = useQueryClient();

  return useMutation({
    mutationFn: async (task: Task) => (await chibitvClient.cancelTask({ taskId: task.id })).task,
    onSuccess: () => queryClient.invalidateQueries({ queryKey: queryKeys.tasks }),
  });
}
