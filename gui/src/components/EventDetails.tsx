import { VideoCameraIcon } from "@heroicons/react/24/outline";
import { Button, Modal } from "@heroui/react";
import type { JSX } from "react";

import { useScheduleRecording } from "../api/tasks";
import { toDate } from "../api/time";
import type { Event } from "../gen/chibitv/v1/chibitv_pb";

const scheduleFormatter = new Intl.DateTimeFormat("en-GB", {
  day: "numeric",
  month: "short",
  weekday: "short",
  hour: "2-digit",
  minute: "2-digit",
});

const timeFormatter = new Intl.DateTimeFormat("en-GB", {
  hour: "2-digit",
  minute: "2-digit",
});

/**
 * What one programme is, and what can be done with it.
 *
 * Booking a recording only starts a task, which is followed with every other
 * one, so the dialog closes as soon as the server has taken it.
 */
export function EventDetails({
  event,
  serviceName,
  onClose,
}: {
  event: Event;
  serviceName: string | undefined;
  onClose: () => void;
}): JSX.Element {
  const scheduleRecording = useScheduleRecording();
  const description = event.description.filter(({ content }) => content.length > 0);
  const startAt = toDate(event.startTime);
  const endAt = toDate(event.endTime);
  const when = startAt && [scheduleFormatter.format(startAt), endAt && timeFormatter.format(endAt)].filter(Boolean);

  return (
    <Modal isOpen onOpenChange={(isOpen) => !isOpen && onClose()}>
      <Modal.Backdrop variant="blur">
        <Modal.Container placement="center" size="lg">
          <Modal.Dialog>
            <Modal.Header>
              <Modal.Heading className="pe-8">{event.title || "Untitled"}</Modal.Heading>
            </Modal.Header>
            <Modal.CloseTrigger />
            <Modal.Body>
              <p className="mb-4 text-sm text-muted">{[serviceName, when?.join("–")].filter(Boolean).join(" · ")}</p>
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
            <Modal.Footer>
              <Button
                isDisabled={scheduleRecording.isPending}
                variant="primary"
                onPress={() =>
                  scheduleRecording.mutate({ serviceId: event.serviceId, eventId: event.id }, { onSuccess: onClose })
                }
              >
                <VideoCameraIcon className="size-4" />
                Record
              </Button>
            </Modal.Footer>
          </Modal.Dialog>
        </Modal.Container>
      </Modal.Backdrop>
    </Modal>
  );
}
