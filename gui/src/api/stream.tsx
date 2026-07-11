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
  updateService: (serviceId: number) => Promise<void>;
}

const StreamContext = createContext<StreamContextValue | undefined>(undefined);

interface StreamProviderProps {
  children: ReactNode;
}

export function StreamProvider({ children }: StreamProviderProps): JSX.Element {
  const [state, setState] = useState<StreamState>();
  const [connectionGeneration, setConnectionGeneration] = useState(0);
  const [playbackGeneration, setPlaybackGeneration] = useState(0);
  const listeners = useRef(new Set<Fmp4Listener>());
  const pendingFmp4 = useRef<Uint8Array[]>([]);
  const connectionAbortController = useRef<AbortController | null>(null);

  const subscribeFmp4 = useCallback((listener: Fmp4Listener) => {
    listeners.current.add(listener);

    for (const data of pendingFmp4.current.splice(0)) {
      listener(data);
    }

    return () => listeners.current.delete(listener);
  }, []);

  const updateService = useCallback(async (serviceId: number) => {
    connectionAbortController.current?.abort();
    listeners.current.clear();
    pendingFmp4.current = [];
    setPlaybackGeneration((generation) => generation + 1);

    try {
      await chibitvClient.updateStream({ streamId: 0, serviceId });
    } finally {
      setConnectionGeneration((generation) => generation + 1);
    }
  }, []);

  // biome-ignore lint/correctness/useExhaustiveDependencies: the generation deliberately reconnects the stream after tuning.
  useEffect(() => {
    const abortController = new AbortController();
    connectionAbortController.current = abortController;

    const receive = async () => {
      try {
        const initialState = await chibitvClient.getStream({ streamId: 0 }, { signal: abortController.signal });
        setState(initialState);
      } catch (error) {
        if (!abortController.signal.aborted) {
          console.error("GetStream RPC failed", error);
        }
      }

      while (!abortController.signal.aborted) {
        try {
          const stream = chibitvClient.stream({ streamId: 0 }, { signal: abortController.signal });
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
      if (connectionAbortController.current === abortController) {
        connectionAbortController.current = null;
      }
      pendingFmp4.current = [];
    };
  }, [connectionGeneration]);

  const value = useMemo(
    () => ({ state, subscribeFmp4, playbackGeneration, updateService }),
    [state, subscribeFmp4, playbackGeneration, updateService],
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
