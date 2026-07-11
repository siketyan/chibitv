import clsx from "clsx";
import { type JSX, useState } from "react";

import { Channels } from "./Channels";
import { Events } from "./Events";
import { Navbar } from "./Navbar";
import { Player } from "./Player";

export function Page(): JSX.Element {
  const [isScheduleOpen, setIsScheduleOpen] = useState(false);

  return (
    <div className="flex h-dvh flex-col overflow-hidden bg-background text-foreground">
      <Navbar isScheduleOpen={isScheduleOpen} onChangeScheduleOpen={setIsScheduleOpen} />
      <main
        className={clsx(
          "grid min-h-0 flex-1 overflow-hidden",
          isScheduleOpen
            ? "grid-cols-[minmax(8rem,1fr)_minmax(11rem,40%)] md:grid-cols-[16rem_minmax(8rem,1fr)_minmax(11rem,35%)] lg:grid-cols-[16rem_minmax(8rem,1fr)_24rem]"
            : "grid-cols-[minmax(0,1fr)] md:grid-cols-[16rem_minmax(0,1fr)]",
        )}
      >
        <aside className="hidden min-h-0 flex-col border-r border-separator bg-surface p-3 md:flex">
          <div className="flex items-center justify-between px-2 pb-3 pt-1">
            <h2 className="font-semibold">Channels</h2>
          </div>
          <div className="flex min-h-0 flex-1 flex-col overflow-y-auto">
            <Channels />
          </div>
        </aside>
        <Player />
        {isScheduleOpen && (
          <aside className="flex min-h-0 min-w-0 overflow-hidden border-l border-separator bg-surface">
            <Events />
          </aside>
        )}
      </main>
    </div>
  );
}
