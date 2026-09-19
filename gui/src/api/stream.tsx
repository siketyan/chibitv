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

import { StreamErrorKind, type StreamState } from "../gen/chibitv/v1/chibitv_pb";
import { chibitvClient } from ".";
import type { ServiceKey } from "./services";

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
/**
 * The errors taking the stream back up cannot get past.
 *
 * A card that hands over no key answers the next ECM the same way, so
 * reconnecting would only occupy a tuner to be refused again; the viewer asks
 * for it to be tried again once whatever is in the way has been seen to.
 */
const PERMANENT_ERROR_KINDS: readonly StreamErrorKind[] = [
  StreamErrorKind.NOT_CONTRACTED,
  StreamErrorKind.DESCRAMBLING_REFUSED,
];

/**
 * What stopped the stream: what the server said stopped it, or the call to it
 * breaking, which is how a tuner or a card that cannot be opened at all
 * arrives.
 */
export type StreamFailure = {
  kind: StreamErrorKind;
  message: string;
};

interface StreamContextValue {
  state: StreamState | undefined;
  /** What stopped the stream, until it is taken up again. */
  error: StreamFailure | undefined;
  /** Whether the stream stopped for good, waiting to be asked to try again. */
  stopped: boolean;
  subscribeFmp4: (listener: Fmp4Listener) => () => void;
  playbackGeneration: number;
  /** Drops the connection and takes the stream up again from a fresh init segment. */
  reconnect: () => void;
  /** Takes the stream up again after an error it does not reconnect through. */
  retry: () => void;
}

const StreamContext = createContext<StreamContextValue | undefined>(undefined);

interface StreamProviderProps {
  /** The service to watch; the URL holds it, so a reload keeps the channel. */
  service: ServiceKey | undefined;
  children: ReactNode;
}

export function StreamProvider({ service, children }: StreamProviderProps): JSX.Element {
  // The watched service is picked by this client alone; the server tunes only
  // while the stream below is held open and shares it with other watching
  // clients.
  const [state, setState] = useState<StreamState>();
  const [error, setError] = useState<StreamFailure>();
  const [stopped, setStopped] = useState(false);
  const [playbackGeneration, setPlaybackGeneration] = useState(0);
  /** Bumped to open the stream again once it has stopped for good. */
  const [attempt, setAttempt] = useState(0);
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

  const retry = useCallback(() => setAttempt((attempt) => attempt + 1), []);

  // The service is an object, so the effect below follows what it holds rather
  // than the identity of the object the router hands it in.
  const streamId = service?.streamId;
  const serviceId = service?.serviceId;

  // biome-ignore lint/correctness/useExhaustiveDependencies: the attempt deliberately opens the stream again without the service changing.
  useEffect(() => {
    if (streamId === undefined || serviceId === undefined) {
      return;
    }

    const closed = new AbortController();
    setState(undefined);
    setError(undefined);
    setStopped(false);

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
      let stopForGood = false;

      while (!closed.signal.aborted) {
        restartPlayback();
        setError(undefined);

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

          const stream = chibitvClient.stream({ service: { streamId, serviceId } }, { signal: connection.signal });
          for await (const { payload } of stream) {
            if (payload.case === "state") {
              setState(payload.value);
              continue;
            }

            if (payload.case === "error") {
              setError(payload.value);
              stopForGood = PERMANENT_ERROR_KINDS.includes(payload.value.kind);
              continue;
            }

            if (payload.case === "fmp4") {
              received = true;
              watchForStall();
              deliver(payload.value);
            }
          }
        } catch (rpcError) {
          if (!closed.signal.aborted) {
            console.error("Stream RPC failed", rpcError);
            // The call never got as far as a stream to stop, so there is no
            // kind to it; what it says is all there is to go on.
            setError({
              kind: StreamErrorKind.INTERNAL,
              message: rpcError instanceof Error ? rpcError.message : String(rpcError),
            });
          }
        } finally {
          window.clearTimeout(stallTimer);
          closed.signal.removeEventListener("abort", abort);
          abortConnection.current = undefined;
        }

        if (closed.signal.aborted || stopForGood) {
          setStopped(stopForGood);
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
  }, [streamId, serviceId, restartPlayback, attempt]);

  const value = useMemo(
    () => ({ state, error, stopped, subscribeFmp4, playbackGeneration, reconnect, retry }),
    [state, error, stopped, subscribeFmp4, playbackGeneration, reconnect, retry],
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
