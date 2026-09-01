import { XMarkIcon } from "@heroicons/react/24/outline";
import { Button, ProgressBar } from "@heroui/react";
import type { JSX } from "react";

import { isTaskRunning, useCancelTask, useStartTaskError, useTasks } from "../api/tasks";
import { type Task, TaskState } from "../gen/chibitv/v1/chibitv_pb";

const STATE_LABELS: Record<TaskState, string> = {
  [TaskState.UNSPECIFIED]: "Unknown",
  [TaskState.PENDING]: "Queued",
  [TaskState.RUNNING]: "Running",
  [TaskState.SUCCEEDED]: "Done",
  [TaskState.FAILED]: "Failed",
  [TaskState.CANCELLED]: "Cancelled",
};

/** What the server is doing in the background, and how to stop it. */
export function Tasks(): JSX.Element {
  const tasks = useTasks();
  const startError = useStartTaskError();

  return (
    <div className="flex min-h-0 flex-col gap-2 overflow-hidden">
      {startError && <p className="px-1 text-xs text-danger">Could not start a task: {startError}</p>}
      {tasks.length === 0 ? (
        <p className="px-2 py-6 text-center text-sm text-muted">Nothing is running in the background.</p>
      ) : (
        <ul className="flex flex-col gap-2 overflow-auto">
          {/* The task started last is the one being waited on, so it leads. */}
          {tasks.toReversed().map((task) => (
            <li key={String(task.id)} className="rounded-xl border border-white/10 bg-white/5 p-3">
              <TaskItem task={task} />
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}

function TaskItem({ task }: { task: Task }): JSX.Element {
  const cancelTask = useCancelTask();
  const isRunning = isTaskRunning(task);
  const percentage = task.progress === undefined ? undefined : Math.round(task.progress * 100);

  return (
    <div className="flex flex-col gap-2">
      <div className="flex items-center gap-2">
        <h3 className="mr-auto truncate text-sm font-medium">{task.title}</h3>
        <span className="shrink-0 text-xs tabular-nums text-muted">
          {isRunning && percentage !== undefined
            ? `${STATE_LABELS[task.state]} · ${percentage}%`
            : STATE_LABELS[task.state]}
        </span>
        {isRunning && task.cancellable && (
          <Button
            aria-label={`Cancel ${task.title}`}
            className="h-6 min-h-6 w-6 min-w-6 shrink-0"
            isDisabled={cancelTask.isPending}
            isIconOnly
            size="sm"
            variant="ghost"
            onPress={() => cancelTask.mutate(task)}
          >
            <XMarkIcon />
          </Button>
        )}
      </div>
      {isRunning && (
        <ProgressBar aria-label={task.title} isIndeterminate={percentage === undefined} size="sm" value={percentage}>
          <ProgressBar.Track>
            <ProgressBar.Fill />
          </ProgressBar.Track>
        </ProgressBar>
      )}
      {task.message && <p className="truncate text-xs text-muted">{task.message}</p>}
      {task.error && <p className="text-xs text-danger">{task.error}</p>}
      {cancelTask.error && <p className="text-xs text-danger">{cancelTask.error.message}</p>}
    </div>
  );
}
