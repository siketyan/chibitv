/**
 * How the player is presented: Picture-in-Picture and fullscreen.
 *
 * Both come in a standard and a WebKit flavour. Safari still needs its own on
 * the platforms where the standard API is missing, so every entry point here
 * picks whichever the browser actually offers and reports when it offers
 * neither, letting the GUI leave the control out.
 */

/** The events either flavour announces a Picture-in-Picture change with. */
const PICTURE_IN_PICTURE_EVENTS = ["enterpictureinpicture", "leavepictureinpicture", "webkitpresentationmodechanged"];

/** The events either flavour announces a fullscreen change with. */
const FULLSCREEN_EVENTS = ["fullscreenchange", "webkitfullscreenchange"];

export function supportsPictureInPicture(video: HTMLVideoElement): boolean {
  if (document.pictureInPictureEnabled && !video.disablePictureInPicture) return true;

  return video.webkitSupportsPresentationMode?.("picture-in-picture") === true;
}

export function isInPictureInPicture(video: HTMLVideoElement): boolean {
  return document.pictureInPictureElement === video || video.webkitPresentationMode === "picture-in-picture";
}

export async function togglePictureInPicture(video: HTMLVideoElement): Promise<void> {
  if (!document.pictureInPictureEnabled && video.webkitSetPresentationMode) {
    video.webkitSetPresentationMode(isInPictureInPicture(video) ? "inline" : "picture-in-picture");
    return;
  }

  if (document.pictureInPictureElement === video) {
    await document.exitPictureInPicture();
  } else {
    await video.requestPictureInPicture();
  }
}

export function observePictureInPicture(video: HTMLVideoElement, onChange: () => void): () => void {
  for (const event of PICTURE_IN_PICTURE_EVENTS) {
    video.addEventListener(event, onChange);
  }

  return () => {
    for (const event of PICTURE_IN_PICTURE_EVENTS) {
      video.removeEventListener(event, onChange);
    }
  };
}

// The whole page goes fullscreen rather than the video alone, so that the
// overlaid UI stays on top of it. The player already fills the viewport, so
// this only takes the browser chrome away.
function fullscreenTarget(): HTMLElement {
  return document.documentElement;
}

export function supportsFullscreen(): boolean {
  return document.fullscreenEnabled || typeof fullscreenTarget().webkitRequestFullscreen === "function";
}

export function isFullscreen(): boolean {
  return Boolean(document.fullscreenElement ?? document.webkitFullscreenElement);
}

export async function toggleFullscreen(): Promise<void> {
  if (isFullscreen()) {
    if (document.exitFullscreen) {
      await document.exitFullscreen();
    } else {
      document.webkitExitFullscreen?.();
    }
    return;
  }

  const target = fullscreenTarget();
  if (target.requestFullscreen) {
    await target.requestFullscreen();
  } else {
    target.webkitRequestFullscreen?.();
  }
}

export function observeFullscreen(onChange: () => void): () => void {
  for (const event of FULLSCREEN_EVENTS) {
    document.addEventListener(event, onChange);
  }

  return () => {
    for (const event of FULLSCREEN_EVENTS) {
      document.removeEventListener(event, onChange);
    }
  };
}
