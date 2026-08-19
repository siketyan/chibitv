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

interface StreamContextValue {
  state: StreamState | undefined;
  subscribeFmp4: (listener: Fmp4Listener) => () => void;
  playbackGeneration: number;
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

  const subscribeFmp4 = useCallback((listener: Fmp4Listener) => {
    listeners.current.add(listener);

    for (const data of pendingFmp4.current.splice(0)) {
      listener(data);
    }

    return () => listeners.current.delete(listener);
  }, []);

  useEffect(() => {
    if (serviceId === undefined) {
      return;
    }

    const abortController = new AbortController();
    listeners.current.clear();
    pendingFmp4.current = [];
    setState(undefined);
    setPlaybackGeneration((generation) => generation + 1);

    const receive = async () => {
      while (!abortController.signal.aborted) {
        try {
          const stream = chibitvClient.stream({ serviceId }, { signal: abortController.signal });
          for await (const { payload } of stream) {
            if (payload.case === "state") {
              setState(payload.value);
              continue;
            }

            if (payload.case === "fmp4") {
              if (listeners.current.size === 0) {
                if (pendingFmp4.current.length === MAX_PENDING_FMP4) {
                  pendingFmp4.current.shift();
                }
                pendingFmp4.current.push(payload.value);
              } else {
                for (const listener of listeners.current) {
                  listener(payload.value);
                }
              }
            }
          }
        } catch (error) {
          if (!abortController.signal.aborted) {
            console.error("Stream RPC failed", error);
          }
        }

        if (!abortController.signal.aborted) {
          await new Promise((resolve) => setTimeout(resolve, 1000));
        }
      }
    };

    void receive();

    return () => {
      abortController.abort();
      pendingFmp4.current = [];
    };
  }, [serviceId]);

  const value = useMemo(
    () => ({ state, subscribeFmp4, playbackGeneration }),
    [state, subscribeFmp4, playbackGeneration],
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
