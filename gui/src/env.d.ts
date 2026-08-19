declare module "*.css";

// rsbuild replaces `import.meta.env` at build time; only the flags used here are declared.
interface ImportMetaEnv {
  /** Whether the bundle was built in production mode. */
  readonly PROD: boolean;
}

interface ImportMeta {
  readonly env: ImportMetaEnv;
}

// TypeScript does not ship DOM types for the Managed Media Source API, which is the only MSE
// implementation available on Safari for iOS.
interface ManagedMediaSourceEventMap extends MediaSourceEventMap {
  startstreaming: Event;
  endstreaming: Event;
}

interface ManagedMediaSource extends MediaSource {
  /** Whether the media element currently wants the application to append more data. */
  readonly streaming: boolean;
  addEventListener<K extends keyof ManagedMediaSourceEventMap>(
    type: K,
    listener: (this: ManagedMediaSource, event: ManagedMediaSourceEventMap[K]) => unknown,
    options?: boolean | AddEventListenerOptions,
  ): void;
  addEventListener(
    type: string,
    listener: EventListenerOrEventListenerObject,
    options?: boolean | AddEventListenerOptions,
  ): void;
  removeEventListener<K extends keyof ManagedMediaSourceEventMap>(
    type: K,
    listener: (this: ManagedMediaSource, event: ManagedMediaSourceEventMap[K]) => unknown,
    options?: boolean | EventListenerOptions,
  ): void;
  removeEventListener(
    type: string,
    listener: EventListenerOrEventListenerObject,
    options?: boolean | EventListenerOptions,
  ): void;
}

interface MediaSourceConstructor {
  new (): MediaSource;
  isTypeSupported(type: string): boolean;
}

interface ManagedMediaSourceConstructor extends MediaSourceConstructor {
  prototype: ManagedMediaSource;
  new (): ManagedMediaSource;
}

interface Window {
  readonly ManagedMediaSource?: ManagedMediaSourceConstructor;
}
