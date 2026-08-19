/**
 * Media Session tells the browser and the operating system what is on air, so
 * that the notification, the lock screen, the media hub of the browser and a
 * Picture-in-Picture window name the programme and can pause it.
 *
 * Selecting a service is deliberately left to the GUI: `previoustrack` and
 * `nexttrack` stay unhandled, so no platform control offers to change the
 * channel. Seeking is left out as well, because a broadcast is live and has no
 * position to seek within.
 */

/** The icons of the installed app double as the artwork of what is playing. */
const ARTWORK: MediaImage[] = [
  { src: "/icons/icon-192.png", sizes: "192x192", type: "image/png" },
  { src: "/icons/icon-512.png", sizes: "512x512", type: "image/png" },
];

export interface NowPlaying {
  /** The programme currently on air. */
  title: string;
  /** The service it is broadcast on. */
  service: string;
  /** The broadcaster of that service. */
  provider: string;
}

function getSession(): MediaSession | undefined {
  return "mediaSession" in navigator ? navigator.mediaSession : undefined;
}

/** Names what is playing, or clears the name while nothing is. */
export function publishNowPlaying(nowPlaying: NowPlaying | undefined): void {
  const session = getSession();
  if (!session) return;

  session.metadata = nowPlaying
    ? new MediaMetadata({
        title: nowPlaying.title,
        artist: nowPlaying.service,
        album: nowPlaying.provider,
        artwork: ARTWORK,
      })
    : null;
}

/**
 * Lets the platform controls play and pause the element, and keeps them showing
 * whether it is playing.
 *
 * Returns a function that hands the session back to the browser.
 */
export function bindMediaSession(video: HTMLVideoElement): () => void {
  const session = getSession();
  if (!session) return () => {};

  const actions: [MediaSessionAction, MediaSessionActionHandler][] = [
    ["play", () => void video.play().catch(() => {})],
    ["pause", () => video.pause()],
  ];
  const setActionHandler = (action: MediaSessionAction, handler: MediaSessionActionHandler | null) => {
    try {
      session.setActionHandler(action, handler);
    } catch {
      // The browser does not know this action; the remaining ones still apply.
    }
  };

  const syncPlaybackState = () => {
    session.playbackState = video.paused ? "paused" : "playing";
  };

  for (const [action, handler] of actions) {
    setActionHandler(action, handler);
  }
  video.addEventListener("play", syncPlaybackState);
  video.addEventListener("pause", syncPlaybackState);
  syncPlaybackState();

  return () => {
    video.removeEventListener("play", syncPlaybackState);
    video.removeEventListener("pause", syncPlaybackState);
    for (const [action] of actions) {
      setActionHandler(action, null);
    }
    session.playbackState = "none";
    session.metadata = null;
  };
}
