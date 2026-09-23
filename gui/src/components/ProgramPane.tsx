import { ArrowsPointingOutIcon, XMarkIcon } from "@heroicons/react/24/outline";
import { Button, Tabs } from "@heroui/react";
import { type JSX, type ReactNode, useState } from "react";

import { isSameService } from "../api/services";
import { useStream } from "../api/stream";
import { useServiceKey } from "../router";
import { EventInformation } from "./EventDetails";
import { Events } from "./Events";
import { PinIcon } from "./PinIcon";
import { ScanChannels } from "./ScanChannels";

export function ProgramPane({
  channels,
  isPinned,
  onChangePinned,
  onClose,
  onExpand,
}: {
  /** Adds a tab for the channels, for the stacked layout that has no pane of their own. */
  channels?: ReactNode;
  isPinned?: boolean;
  /** Leaving this out, with `onClose`, docks the pane under the picture instead of beside it. */
  onChangePinned?: () => void;
  onClose?: () => void;
  onExpand: () => void;
}): JSX.Element {
  const [tab, setTab] = useState(channels ? "channels" : "information");
  const service = useServiceKey();
  const { state } = useStream();
  const event = isSameService(state?.service?.key, service) ? state?.event : undefined;

  return (
    <Tabs
      className="min-h-0 flex-1 gap-0 overflow-hidden"
      selectedKey={tab}
      onSelectionChange={(key) => setTab(String(key))}
    >
      <div className="flex shrink-0 flex-wrap items-center justify-between gap-1 border-b border-white/10 p-2">
        <Tabs.ListContainer className="min-w-0">
          <Tabs.List aria-label="Program">
            {channels && (
              <Tabs.Tab id="channels" className="w-auto shrink-0">
                <Tabs.Indicator />
                Channels
              </Tabs.Tab>
            )}
            <Tabs.Tab id="information" className="w-auto shrink-0">
              <Tabs.Indicator />
              Information
            </Tabs.Tab>
            <Tabs.Tab id="schedule" className="w-auto shrink-0">
              <Tabs.Indicator />
              Schedule
            </Tabs.Tab>
          </Tabs.List>
        </Tabs.ListContainer>
        <div className="flex shrink-0">
          {tab === "channels" && <ScanChannels />}
          {tab === "schedule" && (
            <Button aria-label="Expand program guide" isIconOnly size="sm" variant="ghost" onPress={onExpand}>
              <ArrowsPointingOutIcon />
            </Button>
          )}
          {onChangePinned && (
            <Button
              aria-label={isPinned ? "Unpin program pane" : "Pin program pane"}
              aria-pressed={isPinned}
              className="hidden [@media(hover:hover)_and_(pointer:fine)]:inline-flex"
              isIconOnly
              size="sm"
              variant={isPinned ? "secondary" : "ghost"}
              onPress={onChangePinned}
            >
              <PinIcon />
            </Button>
          )}
          {onClose && (
            <Button
              aria-label="Close program pane"
              className="[@media(hover:hover)_and_(pointer:fine)]:hidden"
              isIconOnly
              size="sm"
              variant="ghost"
              onPress={onClose}
            >
              <XMarkIcon />
            </Button>
          )}
        </div>
      </div>
      {channels && (
        <Tabs.Panel id="channels" className="flex min-h-0 flex-1 flex-col overflow-hidden p-3">
          {channels}
        </Tabs.Panel>
      )}
      <Tabs.Panel id="information" className="min-h-0 flex-1 overflow-y-auto p-4">
        {event ? (
          <>
            <h2 className="mb-3 font-semibold">{event.title || "Untitled"}</h2>
            <EventInformation event={event} serviceName={state?.service?.name} />
          </>
        ) : (
          <p className="text-sm text-muted">No program information is available.</p>
        )}
      </Tabs.Panel>
      <Tabs.Panel id="schedule" className="flex min-h-0 flex-1 flex-col overflow-hidden p-0">
        <Events service={service} compact />
      </Tabs.Panel>
    </Tabs>
  );
}
