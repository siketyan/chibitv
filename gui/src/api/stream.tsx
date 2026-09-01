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

import type { StreamState } from "../gen/chibitv/v1/chibitv_pb";
import { chibitvClient } from ".";

type Fmp4Listener = (data: Uint8Array) => void;
const MAX_PENDING_FMP4 = 256;
/** How long to wait before picking a connection that was working back up. */
const RECONNECT_DELAY_MS = 1_000;
/** How long the wait grows to while connections keep breaking straight away. */
const MAX_RECONNECT_DELAY_MS = 8_000;
/** How long a connection has to carry media for to count as having worked. */
const STABLE_CONNECTION_MS = 10_000;
/**
 * How long the stream may stay silent before it counts as lost.
 *
 * What is being watched is live, so media keeps coming as long as the
 * connection is alive; a connection dropped without the socket noticing would
 * otherwise leave the picture frozen forever.
 */
const STALL_TIMEOUT_MS = 20_000;

interface StreamContextValue {
  state: StreamState | undefined;
  subscribeFmp4: (listener: Fmp4Listener) => () => void;
  playbackGeneration: number;
  /** Drops the connection and takes the stream up again from a fresh init segment. */
  reconnect: () => void;
}

const StreamContext = createContext<StreamContextValue | undefined>(undefined);

interface StreamProviderProps {
  /** The service to watch; the URL holds it, so a reload keeps the channel. */
  serviceId: number | undefined;
  children: ReactNode;
}

export function StreamProvider({ serviceId, children }: StreamProviderProps): JSX.Element {
  // The watched service is picked by this client alone; the server tunes only
  // while the stream below is held open and shares it with other watching
  // clients.
  const [state, setState] = useState<StreamState>();
  const [playbackGeneration, setPlaybackGeneration] = useState(0);
  const listeners = useRef(new Set<Fmp4Listener>());
  const pendingFmp4 = useRef<Uint8Array[]>([]);
  const abortConnection = useRef<() => void>(undefined);

  const subscribeFmp4 = useCallback((listener: Fmp4Listener) => {
    listeners.current.add(listener);

    for (const data of pendingFmp4.current.splice(0)) {
      listener(data);
    }

    return () => listeners.current.delete(listener);
  }, []);

  /**
   * Starts the player over on the media the next connection brings.
   *
   * Every connection opens with an init segment of its own, and a decoder that
   * is already running cannot take a second one, so the pipeline is rebuilt
   * across the break rather than fed through it. Whatever is still buffered
   * belongs to the connection that ended, so it goes with it.
   */
  const restartPlayback = useCallback(() => {
    listeners.current.clear();
    pendingFmp4.current = [];
    setPlaybackGeneration((generation) => generation + 1);
  }, []);

  const reconnect = useCallback(() => {
    abortConnection.current?.();
  }, []);

  useEffect(() => {
    if (serviceId === undefined) {
      return;
    }

    const closed = new AbortController();
    setState(undefined);

    const deliver = (data: Uint8Array) => {
      if (listeners.current.size === 0) {
        if (pendingFmp4.current.length === MAX_PENDING_FMP4) {
          pendingFmp4.current.shift();
        }
        pendingFmp4.current.push(data);
        return;
      }

      for (const listener of listeners.current) {
        listener(data);
      }
    };

    const receive = async () => {
      let failures = 0;

      while (!closed.signal.aborted) {
        restartPlayback();

        const connection = new AbortController();
        const abort = () => connection.abort();
        closed.signal.addEventListener("abort", abort);
        abortConnection.current = abort;

        const startedAt = Date.now();
        let received = false;
        let stallTimer: number | undefined;
        const watchForStall = () => {
          window.clearTimeout(stallTimer);
          stallTimer = window.setTimeout(abort, STALL_TIMEOUT_MS);
        };

        try {
          watchForStall();

          const stream = chibitvClient.stream({ serviceId }, { signal: connection.signal });
          for await (const { payload } of stream) {
            if (payload.case === "state") {
              setState(payload.value);
              continue;
            }

            if (payload.case === "fmp4") {
              received = true;
              watchForStall();
              deliver(payload.value);
            }
          }
        } catch (error) {
          if (!closed.signal.aborted) {
            console.error("Stream RPC failed", error);
          }
        } finally {
          window.clearTimeout(stallTimer);
          closed.signal.removeEventListener("abort", abort);
          abortConnection.current = undefined;
        }

        if (closed.signal.aborted) {
          break;
        }

        // A connection that carried media for a while was working, so it is
        // taken straight back up; one that broke immediately is backed off
        // from, so that a tuner that stays busy or media that cannot be played
        // at all is retried at a slower pace than it fails at.
        const worked = received && Date.now() - startedAt >= STABLE_CONNECTION_MS;
        failures = worked ? 0 : failures + 1;
        const delay = Math.min(RECONNECT_DELAY_MS * 2 ** failures, MAX_RECONNECT_DELAY_MS);
        await new Promise((resolve) => setTimeout(resolve, delay));
      }
    };

    void receive();

    return () => {
      closed.abort();
      pendingFmp4.current = [];
    };
  }, [serviceId, restartPlayback]);

  const value = useMemo(
    () => ({ state, subscribeFmp4, playbackGeneration, reconnect }),
    [state, subscribeFmp4, playbackGeneration, reconnect],
  );

  return <StreamContext value={value}>{children}</StreamContext>;
}

export function useStream(): StreamContextValue {
  const context = useContext(StreamContext);
  if (!context) {
    throw new Error("useStream must be used within StreamProvider");
  }

  return context;
}
