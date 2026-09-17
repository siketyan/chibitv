import { type UseMutationResult, useMutation, useQuery, useQueryClient } from "@tanstack/react-query";

import { type DeliverySystem, type GetScanResultResponse, type Task, TaskKind } from "../gen/chibitv/v1/chibitv_pb";
import { chibitvClient, queryKeys } from ".";
import { isTaskRunning, useTasks } from "./tasks";

/**
 * The key every mutation starting a task shares, as `tasks.ts` declares it.
 *
 * A scan that never started is not in the list of tasks, so this is what the
 * error is read back from.
 */
const START_TASK_MUTATION_KEY = ["start-task"] as const;

export interface ScanRequest {
  deliverySystem: DeliverySystem;
  /** Read a satellite network out of one transponder instead of tuning to every stream. */
  fast: boolean;
  /** Time spent on each channel. Zero leaves it to the server. */
  timeoutSeconds: number;
}

/** Starts looking for the channels on air in the background. */
export function useScanChannels(): UseMutationResult<Task | undefined, Error, ScanRequest> {
  const queryClient = useQueryClient();

  return useMutation({
    mutationKey: START_TASK_MUTATION_KEY,
    mutationFn: async (request: ScanRequest) => (await chibitvClient.scanChannels(request)).task,
    // The task is reported by the stream as well, but the list is read again so
    // that it shows up even while the stream is being opened again.
    onSuccess: () => queryClient.invalidateQueries({ queryKey: queryKeys.tasks }),
  });
}

/** What the last scan found, which is empty until one has finished. */
export function useScanResult(): GetScanResultResponse | undefined {
  const { data } = useQuery({
    queryKey: queryKeys.scanResult,
    queryFn: async () => await chibitvClient.getScanResult({}),
  });

  return data;
}

/** The scan running right now, if one is. */
export function useRunningScan(): Task | undefined {
  return useTasks().find((task) => task.kind === TaskKind.SCAN_CHANNELS && isTaskRunning(task));
}
