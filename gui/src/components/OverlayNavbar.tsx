import {
  ArrowPathIcon,
  CalendarDaysIcon,
  InformationCircleIcon,
  QueueListIcon,
  XMarkIcon,
} from "@heroicons/react/24/outline";
import { Button, Modal, Spinner } from "@heroui/react";
import clsx from "clsx";
import { type JSX, useState } from "react";

import { useStream } from "../api/stream";
import { isTaskRunning, useStartTaskError, useTasks } from "../api/tasks";
import { chromeTransition, useChromeHold, usePlayerChrome } from "../player/chrome";

interface OverlayNavbarProps {
  areTasksOpen: boolean;
  isChannelsOpen: boolean;
  isScheduleOpen: boolean;
  onChangeChannelsOpen: (open: boolean) => void;
  onChangeScheduleOpen: (open: boolean) => void;
  onChangeTasksOpen: (open: boolean) => void;
}

export function OverlayNavbar({
  areTasksOpen,
  isChannelsOpen,
  isScheduleOpen,
  onChangeChannelsOpen,
  onChangeScheduleOpen,
  onChangeTasksOpen,
}: OverlayNavbarProps): JSX.Element {
  const { isVisible } = usePlayerChrome();
  const { state } = useStream();
  const [areDetailsOpen, setAreDetailsOpen] = useState(false);
  const tasks = useTasks();
  const runningTasks = tasks.filter(isTaskRunning);
  const startTaskError = useStartTaskError();
  const event = state?.event;
  const description = event?.description.filter(({ content }) => content.length > 0) ?? [];
  // The programme on air is known only once its SI has been received, so until
  // then the service names what is being watched.
  const title = event?.title || state?.service?.name;

  // The details are a dialog rather than a tooltip because a touch screen has
  // no hover to open one with, so the UI stays put while it is open.
  useChromeHold("event-details", areDetailsOpen);

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
          aria-pressed={isChannelsOpen}
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
        {description.length > 0 && (
          <Modal isOpen={areDetailsOpen} onOpenChange={setAreDetailsOpen}>
            <Button
              aria-label="Event details"
              className="pointer-events-auto text-white data-[hover=true]:bg-white/15"
              isIconOnly
              size="sm"
              variant="ghost"
            >
              <InformationCircleIcon />
            </Button>
            <Modal.Backdrop variant="blur">
              <Modal.Container placement="center" size="lg">
                <Modal.Dialog>
                  <Modal.Header>
                    <Modal.Heading className="pe-8">{title ?? "Event details"}</Modal.Heading>
                  </Modal.Header>
                  <Modal.CloseTrigger />
                  <Modal.Body>
                    <dl className="flex flex-col gap-4">
                      {description.map(({ name, content }, index) => (
                        // The summary carries no name of its own, and a detail may
                        // well repeat one, so the position is the only stable key.
                        // biome-ignore lint/suspicious/noArrayIndexKey: see above
                        <div key={index} className="flex flex-col gap-2">
                          <dt className="text-muted">{name}</dt>
                          <dd className="whitespace-pre-line text-sm leading-5 text-foreground">
                            {content.replaceAll("\r", "\n") || "-"}
                          </dd>
                        </div>
                      ))}
                    </dl>
                  </Modal.Body>
                </Modal.Dialog>
              </Modal.Container>
            </Modal.Backdrop>
          </Modal>
        )}
      </div>
      <div className="flex shrink-0 items-center gap-2">
        {/* The server starts no task of its own accord, so the button appears
            once there is something to look at — a task, or a task that could
            not be started — and stays for as long as it is kept. */}
        {(tasks.length > 0 || startTaskError !== undefined) && (
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
          aria-label={isScheduleOpen ? "Close schedule" : "Open schedule"}
          aria-pressed={isScheduleOpen}
          className="pointer-events-auto shrink-0 text-white data-[hover=true]:bg-white/15"
          isIconOnly
          variant="ghost"
          onPress={() => onChangeScheduleOpen(!isScheduleOpen)}
        >
          {isScheduleOpen ? <XMarkIcon /> : <CalendarDaysIcon />}
        </Button>
      </div>
    </nav>
  );
}
