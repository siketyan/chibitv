import { XMarkIcon } from "@heroicons/react/24/outline";
import { Button, Modal } from "@heroui/react";
import clsx from "clsx";
import { type JSX, useState } from "react";

import logo from "../logo.svg";
import { useChromeHold, usePlayerChrome } from "../player/chrome";
import { useServiceKey } from "../router";
import { Channels } from "./Channels";
import { Events } from "./Events";
import { OverlayNavbar } from "./OverlayNavbar";
import { PinIcon } from "./PinIcon";
import { Player } from "./Player";
import { ProgramPane } from "./ProgramPane";
import { ScanChannels } from "./ScanChannels";
import { Tasks } from "./Tasks";

const EDGE_PEEK_WIDTH = 128;
const isNarrowScreen = () => window.matchMedia("(max-width: 1023px)").matches;
type PaneState = "closed" | "peek" | "open";

export function Page(): JSX.Element {
  const [channelsPane, setChannelsPane] = useState<PaneState>("closed");
  const [programPane, setProgramPane] = useState<PaneState>("closed");
  const isChannelsOpen = channelsPane !== "closed";
  const isProgramOpen = programPane !== "closed";
  const [isScheduleOpen, setIsScheduleOpen] = useState(false);
  const [areTasksOpen, setAreTasksOpen] = useState(false);
  const { isVisible } = usePlayerChrome();
  const service = useServiceKey();

  useChromeHold("panes", isChannelsOpen || isProgramOpen || isScheduleOpen || areTasksOpen);

  const changeChannelsOpen = (open: boolean) => {
    setChannelsPane(open ? "open" : "closed");
    if (open && isNarrowScreen()) {
      setProgramPane("closed");
    }
  };

  const changeProgramOpen = (open: boolean) => {
    setProgramPane(open ? "open" : "closed");
    if (open && isNarrowScreen()) {
      setChannelsPane("closed");
    }
  };

  return (
    <main
      className={clsx(
        "viewer relative h-viewport overflow-hidden bg-black text-foreground",
        !isVisible && "cursor-none",
      )}
      data-channels-pinned={channelsPane === "open"}
      data-program-pinned={programPane === "open"}
      onPointerMove={(event) => {
        if (event.pointerType !== "mouse" || isScheduleOpen || !window.matchMedia("(any-hover: hover)").matches) return;
        const target = event.target as Element;
        if (target.closest('[role="dialog"]')) return;
        const bounds = event.currentTarget.getBoundingClientRect();
        const pane = target.closest("aside")?.id;
        if (event.clientX <= bounds.left + EDGE_PEEK_WIDTH) {
          if (!isChannelsOpen) {
            setChannelsPane("peek");
            if (isNarrowScreen() && programPane !== "open") setProgramPane("closed");
          }
        } else if (channelsPane === "peek" && pane !== "channels-pane") {
          setChannelsPane("closed");
        }
        if (event.clientX >= bounds.right - EDGE_PEEK_WIDTH) {
          if (!isProgramOpen) {
            setProgramPane("peek");
            if (isNarrowScreen() && channelsPane !== "open") setChannelsPane("closed");
          }
        } else if (programPane === "peek" && pane !== "program-pane") {
          setProgramPane("closed");
        }
      }}
      onPointerLeave={() => {
        setChannelsPane((current) => (current === "peek" ? "closed" : current));
        setProgramPane((current) => (current === "peek" ? "closed" : current));
      }}
    >
      {/* Keep this subtree mounted when pinning so playback is uninterrupted. */}
      <div className="player-area absolute inset-y-0 min-w-0">
        <Player />
        <div className="pointer-events-none absolute inset-safe">
          <OverlayNavbar
            areTasksOpen={areTasksOpen}
            isChannelsOpen={isChannelsOpen}
            isProgramOpen={isProgramOpen}
            onChangeChannelsOpen={changeChannelsOpen}
            onChangeProgramOpen={changeProgramOpen}
            onChangeTasksOpen={setAreTasksOpen}
          />
        </div>
      </div>
      <div className="pointer-events-none absolute inset-safe">
        {/* Keep pane contents mounted so a details/scan dialog survives the pointer leaving its pane. */}
        <aside
          id="channels-pane"
          aria-label="Channels"
          data-open={isChannelsOpen}
          className={clsx(
            "side-pane pointer-events-auto absolute inset-y-0 left-0 z-40 w-(--channels-pane-width) min-h-0 flex-col overflow-hidden border-r border-white/10 bg-surface/95 p-3",
            isChannelsOpen ? "flex" : "hidden",
          )}
        >
          <div className="flex items-center justify-between gap-2 px-2 pb-3 pt-1">
            <h2 className="flex-1">
              <img src={logo} alt="chibitv" className="h-6" />
            </h2>
            <ScanChannels />
            <Button
              aria-label={channelsPane === "open" ? "Unpin channels" : "Pin channels"}
              aria-pressed={channelsPane === "open"}
              className="hidden [@media(hover:hover)_and_(pointer:fine)]:inline-flex"
              isIconOnly
              size="sm"
              variant={channelsPane === "open" ? "secondary" : "ghost"}
              onPress={() => setChannelsPane(channelsPane === "open" ? "peek" : "open")}
            >
              <PinIcon />
            </Button>
            <Button
              aria-label="Close channels"
              className="[@media(hover:hover)_and_(pointer:fine)]:hidden"
              isIconOnly
              size="sm"
              variant="ghost"
              onPress={() => changeChannelsOpen(false)}
            >
              <XMarkIcon />
            </Button>
          </div>
          <div className="flex min-h-0 flex-1 flex-col overflow-hidden">
            <Channels
              onServiceChange={() => {
                if (isNarrowScreen()) changeChannelsOpen(false);
              }}
            />
          </div>
        </aside>
        <aside
          id="program-pane"
          aria-label="Program"
          data-open={isProgramOpen}
          className={clsx(
            "side-pane pointer-events-auto absolute inset-y-0 right-0 z-40 w-(--program-pane-width) min-h-0 flex-col overflow-hidden border-l border-white/10 bg-surface/95",
            isProgramOpen ? "flex" : "hidden",
          )}
        >
          <ProgramPane
            isPinned={programPane === "open"}
            onChangePinned={() => setProgramPane(programPane === "open" ? "peek" : "open")}
            onClose={() => changeProgramOpen(false)}
            onExpand={() => setIsScheduleOpen(true)}
          />
        </aside>
        {areTasksOpen && (
          <aside className="pointer-events-auto absolute right-3 top-18 z-50 flex max-h-[min(24rem,calc(100%-6rem))] w-[min(22rem,calc(100%-1.5rem))] min-h-0 flex-col overflow-hidden rounded-2xl border border-white/10 bg-surface/95 p-3 shadow-2xl sm:right-4 sm:top-20">
            <div className="flex items-center justify-between px-2 pb-3 pt-1">
              <h2 className="font-semibold">Background tasks</h2>
            </div>
            <div className="flex min-h-0 flex-1 flex-col overflow-hidden">
              <Tasks />
            </div>
          </aside>
        )}
      </div>
      <Modal isOpen={isScheduleOpen} onOpenChange={setIsScheduleOpen}>
        <Modal.Backdrop>
          <Modal.Container size="full">
            <Modal.Dialog className="h-viewport min-h-0 overflow-hidden bg-surface">
              <Modal.Header>
                <Modal.Heading>Program guide</Modal.Heading>
              </Modal.Header>
              <Modal.CloseTrigger aria-label="Close full-screen program guide" />
              <Modal.Body className="flex min-h-0 flex-1 flex-col overflow-hidden p-0">
                <Events service={service} />
              </Modal.Body>
            </Modal.Dialog>
          </Modal.Container>
        </Modal.Backdrop>
      </Modal>
    </main>
  );
}
