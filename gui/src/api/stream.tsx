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
}

const StreamContext = createContext<StreamContextValue | undefined>(undefined);

interface StreamProviderProps {
  children: ReactNode;
}

export function StreamProvider({ children }: StreamProviderProps): JSX.Element {
  const [state, setState] = useState<StreamState>();
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
    const abortController = new AbortController();

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
      pendingFmp4.current = [];
    };
  }, []);

  const value = useMemo(() => ({ state, subscribeFmp4 }), [state, subscribeFmp4]);

  return <StreamContext value={value}>{children}</StreamContext>;
}

export function useStream(): StreamContextValue {
  const context = useContext(StreamContext);
  if (!context) {
    throw new Error("useStream must be used within StreamProvider");
  }

  return context;
}
