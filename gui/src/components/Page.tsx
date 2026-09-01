import clsx from "clsx";
import { type JSX, useState } from "react";

import { usePlayerChrome } from "../player/chrome";
import { Channels } from "./Channels";
import { Events } from "./Events";
import { OverlayNavbar } from "./OverlayNavbar";
import { Player } from "./Player";
import { Tasks } from "./Tasks";

const isNarrowScreen = () => window.matchMedia("(max-width: 767px)").matches;

export function Page(): JSX.Element {
  const [isChannelsOpen, setIsChannelsOpen] = useState(() => !isNarrowScreen());
  const [isScheduleOpen, setIsScheduleOpen] = useState(false);
  const [areTasksOpen, setAreTasksOpen] = useState(false);
  // The panels stay put while they are open: the viewer asked for them, and
  // only the UI drawn over the picture fades away.
  const { isVisible } = usePlayerChrome();

  const changeChannelsOpen = (open: boolean) => {
    setIsChannelsOpen(open);
    if (open && isNarrowScreen()) {
      setIsScheduleOpen(false);
    }
  };

  const changeScheduleOpen = (open: boolean) => {
    setIsScheduleOpen(open);
    if (open && isNarrowScreen()) {
      setIsChannelsOpen(false);
    }
  };

  return (
    <main className={clsx("relative h-viewport overflow-hidden bg-black text-foreground", !isVisible && "cursor-none")}>
      <Player />
      {/* The video fills the display, while everything drawn on top of it stays
          inside the safe area so that an installed app keeps it reachable. */}
      <div className="pointer-events-none absolute inset-safe">
        <OverlayNavbar
          areTasksOpen={areTasksOpen}
          isChannelsOpen={isChannelsOpen}
          isScheduleOpen={isScheduleOpen}
          onChangeChannelsOpen={changeChannelsOpen}
          onChangeScheduleOpen={changeScheduleOpen}
          onChangeTasksOpen={setAreTasksOpen}
        />
        {isChannelsOpen && (
          <aside className="pointer-events-auto absolute bottom-18 left-3 top-18 z-20 flex w-[min(18rem,calc(100%-1.5rem))] min-h-0 flex-col overflow-hidden rounded-2xl border border-white/10 bg-surface/75 p-3 shadow-2xl backdrop-blur-xl sm:bottom-20 sm:left-4 sm:top-20">
            <div className="flex items-center justify-between px-2 pb-3 pt-1">
              <h2 className="font-semibold">Channels</h2>
            </div>
            <div className="flex min-h-0 flex-1 flex-col overflow-hidden">
              <Channels />
            </div>
          </aside>
        )}
        {areTasksOpen && (
          <aside className="pointer-events-auto absolute right-3 top-18 z-20 flex max-h-[min(24rem,calc(100%-6rem))] w-[min(22rem,calc(100%-1.5rem))] min-h-0 flex-col overflow-hidden rounded-2xl border border-white/10 bg-surface/75 p-3 shadow-2xl backdrop-blur-xl sm:right-4 sm:top-20">
            <div className="flex items-center justify-between px-2 pb-3 pt-1">
              <h2 className="font-semibold">Background tasks</h2>
            </div>
            <div className="flex min-h-0 flex-1 flex-col overflow-hidden">
              <Tasks />
            </div>
          </aside>
        )}
        {isScheduleOpen && (
          <aside className="pointer-events-auto absolute inset-x-3 bottom-18 top-18 z-20 flex min-h-0 overflow-hidden rounded-2xl border border-white/10 bg-surface/80 shadow-2xl backdrop-blur-xl sm:inset-x-4 sm:bottom-20 sm:top-20">
            <Events />
          </aside>
        )}
      </div>
    </main>
  );
}
