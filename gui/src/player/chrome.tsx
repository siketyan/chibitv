import clsx from "clsx";
import {
  createContext,
  type JSX,
  type ReactNode,
  useCallback,
  useContext,
  useEffect,
  useMemo,
  useRef,
  useState,
} from "react";

/** How long the UI stays on screen after the last sign of the viewer. */
const IDLE_DELAY_MS = 3000;

/** What counts as the viewer still being there. */
const ACTIVITY_EVENTS = ["pointermove", "pointerdown", "keydown", "wheel"];

interface PlayerChromeValue {
  /** Whether the UI drawn over the picture is on screen. */
  isVisible: boolean;
  /**
   * Keeps that UI on screen while `held`.
   *
   * Every caller names its own reason, so that holds taken for different
   * reasons at the same time do not release each other.
   */
  hold: (reason: string, held: boolean) => void;
}

const PlayerChromeContext = createContext<PlayerChromeValue | undefined>(undefined);

/**
 * Fades the UI drawn over the picture out while the viewer is doing nothing,
 * the way a video player does, and brings it back on any input.
 */
export function PlayerChromeProvider({ children }: { children: ReactNode }): JSX.Element {
  const [isIdle, setIsIdle] = useState(false);
  const [holds, setHolds] = useState<ReadonlySet<string>>(() => new Set());
  const timer = useRef<number | undefined>(undefined);

  const wake = useCallback(() => {
    setIsIdle(false);
    window.clearTimeout(timer.current);
    timer.current = window.setTimeout(() => setIsIdle(true), IDLE_DELAY_MS);
  }, []);

  useEffect(() => {
    for (const event of ACTIVITY_EVENTS) {
      window.addEventListener(event, wake, { passive: true });
    }

    return () => {
      for (const event of ACTIVITY_EVENTS) {
        window.removeEventListener(event, wake);
      }
      window.clearTimeout(timer.current);
    };
  }, [wake]);

  // Releasing the last hold starts the countdown again instead of hiding the UI
  // from under the viewer straight away. This also starts it on the first render.
  useEffect(() => {
    if (holds.size === 0) {
      wake();
    }
  }, [holds, wake]);

  const hold = useCallback((reason: string, held: boolean) => {
    setHolds((current) => {
      if (current.has(reason) === held) return current;

      const next = new Set(current);
      if (held) {
        next.add(reason);
      } else {
        next.delete(reason);
      }
      return next;
    });
  }, []);

  const value = useMemo(() => ({ isVisible: !isIdle || holds.size > 0, hold }), [isIdle, holds, hold]);

  return <PlayerChromeContext value={value}>{children}</PlayerChromeContext>;
}

export function usePlayerChrome(): PlayerChromeValue {
  const context = useContext(PlayerChromeContext);
  if (!context) {
    throw new Error("usePlayerChrome must be used within PlayerChromeProvider");
  }

  return context;
}

/** Holds the UI on screen while `held`, and lets go when the caller unmounts. */
export function useChromeHold(reason: string, held: boolean): void {
  const { hold } = usePlayerChrome();

  useEffect(() => {
    hold(reason, held);

    return () => hold(reason, false);
  }, [hold, reason, held]);
}

/** The classes that fade an overlay in and out with the rest of the UI. */
export function chromeTransition(isVisible: boolean): string {
  // `visibility` also takes the controls out of the tab order once faded out,
  // and only flips at the end of the transition, so the fade stays visible.
  //
  // A pointer resting on an overlay and a control holding focus both keep it
  // where it is. Expressing that here rather than in state keeps the overlay a
  // plain container: it never has to listen for pointer or focus events itself.
  return clsx(
    "transition-[opacity,visibility] duration-300",
    "hover:visible hover:opacity-100 focus-within:visible focus-within:opacity-100",
    isVisible ? "opacity-100" : "invisible opacity-0",
  );
}
