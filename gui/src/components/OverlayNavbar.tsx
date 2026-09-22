import { ArrowPathIcon, InformationCircleIcon, QueueListIcon, XMarkIcon } from "@heroicons/react/24/outline";
import { Button, Spinner } from "@heroui/react";
import clsx from "clsx";
import type { JSX } from "react";

import { useStream } from "../api/stream";
import { isTaskRunning, useStartTaskError, useTasks } from "../api/tasks";
import { chromeTransition, usePlayerChrome } from "../player/chrome";

interface OverlayNavbarProps {
  areTasksOpen: boolean;
  isChannelsOpen: boolean;
  isProgramOpen: boolean;
  onChangeChannelsOpen: (open: boolean) => void;
  onChangeProgramOpen: (open: boolean) => void;
  onChangeTasksOpen: (open: boolean) => void;
}

export function OverlayNavbar({
  areTasksOpen,
  isChannelsOpen,
  isProgramOpen,
  onChangeChannelsOpen,
  onChangeProgramOpen,
  onChangeTasksOpen,
}: OverlayNavbarProps): JSX.Element {
  const { isVisible } = usePlayerChrome();
  const { state } = useStream();
  const tasks = useTasks();
  const runningTasks = tasks.filter(isTaskRunning);
  const startTaskError = useStartTaskError();
  const event = state?.event;
  // The programme on air is known only once its SI has been received, so until
  // then the service names what is being watched.
  const title = event?.title || state?.service?.name;

  return (
    <nav
      className={clsx(
        "pointer-events-none absolute inset-x-0 top-0 z-30 flex items-start justify-between gap-3 bg-gradient-to-b from-black/80 to-transparent px-3 pb-10 pt-3 text-white sm:px-5 sm:pt-4",
        chromeTransition(isVisible),
      )}
    >
      <div className="flex min-w-0 items-center gap-2">
        <Button
          aria-label={isChannelsOpen ? "Close channels" : "Open channels"}
          aria-expanded={isChannelsOpen}
          aria-controls="channels-pane"
          className="pointer-events-auto shrink-0 text-white data-[hover=true]:bg-white/15"
          isIconOnly
          variant="ghost"
          onPress={() => onChangeChannelsOpen(!isChannelsOpen)}
        >
          {isChannelsOpen ? <XMarkIcon /> : <QueueListIcon />}
        </Button>
        {/* No shadow under the title: the gradient behind this bar is what
            keeps it legible over the picture, and a shadow on top of that only
            showed up as a smudge on an installed app for iOS. */}
        {title && <h1 className="truncate text-sm font-medium sm:text-base">{title}</h1>}
      </div>
      <div className="flex shrink-0 items-center gap-2">
        {/* The server starts no task of its own accord, so the button appears
            once there is something to look at — a task, or a task that could
            not be started — and stays for as long as it is kept, or for as
            long as the panel is open to close it again with. */}
        {(areTasksOpen || tasks.length > 0 || startTaskError !== undefined) && (
          <Button
            aria-label={areTasksOpen ? "Close background tasks" : "Open background tasks"}
            aria-pressed={areTasksOpen}
            className="pointer-events-auto shrink-0 text-white data-[hover=true]:bg-white/15"
            isIconOnly
            variant="ghost"
            onPress={() => onChangeTasksOpen(!areTasksOpen)}
          >
            {runningTasks.length > 0 ? <Spinner size="sm" /> : <ArrowPathIcon />}
          </Button>
        )}
        <Button
          aria-label={isProgramOpen ? "Close program pane" : "Open program pane"}
          aria-expanded={isProgramOpen}
          aria-controls="program-pane"
          className="pointer-events-auto shrink-0 text-white data-[hover=true]:bg-white/15"
          isIconOnly
          variant="ghost"
          onPress={() => onChangeProgramOpen(!isProgramOpen)}
        >
          {isProgramOpen ? <XMarkIcon /> : <InformationCircleIcon />}
        </Button>
      </div>
    </nav>
  );
}
