import { type JSX, useState } from "react";

import { Channels } from "./Channels";
import { Events } from "./Events";
import { OverlayNavbar } from "./OverlayNavbar";
import { Player } from "./Player";

const isNarrowScreen = () => window.matchMedia("(max-width: 767px)").matches;

export function Page(): JSX.Element {
  const [isChannelsOpen, setIsChannelsOpen] = useState(() => !isNarrowScreen());
  const [isScheduleOpen, setIsScheduleOpen] = useState(false);

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
    <main className="relative h-dvh overflow-hidden bg-black text-foreground">
      <Player />
      <OverlayNavbar
        isChannelsOpen={isChannelsOpen}
        isScheduleOpen={isScheduleOpen}
        onChangeChannelsOpen={changeChannelsOpen}
        onChangeScheduleOpen={changeScheduleOpen}
      />
      {isChannelsOpen && (
        <aside className="absolute bottom-3 left-3 top-18 z-20 flex w-[min(18rem,calc(100%-1.5rem))] min-h-0 flex-col overflow-hidden rounded-2xl border border-white/10 bg-surface/75 p-3 shadow-2xl backdrop-blur-xl sm:bottom-4 sm:left-4 sm:top-20">
          <div className="flex items-center justify-between px-2 pb-3 pt-1">
            <h2 className="font-semibold">Channels</h2>
          </div>
          <div className="flex min-h-0 flex-1 flex-col overflow-y-auto">
            <Channels />
          </div>
        </aside>
      )}
      {isScheduleOpen && (
        <aside className="absolute bottom-3 right-3 top-18 z-20 flex w-[min(24rem,calc(100%-1.5rem))] min-h-0 overflow-hidden rounded-2xl border border-white/10 bg-surface/75 shadow-2xl backdrop-blur-xl sm:bottom-4 sm:right-4 sm:top-20">
          <Events />
        </aside>
      )}
    </main>
  );
}
