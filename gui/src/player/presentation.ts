/**
 * How the player is presented: Picture-in-Picture and fullscreen.
 *
 * Both come in a standard and a WebKit flavour. Safari still needs its own on
 * the platforms where the standard API is missing, so every entry point here
 * picks whichever the browser actually offers and reports when it offers
 * neither, letting the GUI leave the control out.
 */

import { isIosWebApp } from "../platform";

/** How WebKit announces that the video moved between inline, Picture-in-Picture and fullscreen. */
const PRESENTATION_MODE_EVENT = "webkitpresentationmodechanged";

/** The events either flavour announces a Picture-in-Picture change with. */
const PICTURE_IN_PICTURE_EVENTS = ["enterpictureinpicture", "leavepictureinpicture", PRESENTATION_MODE_EVENT];

/** The events either flavour announces a fullscreen change with. */
const FULLSCREEN_EVENTS = ["fullscreenchange", "webkitfullscreenchange"];

export function supportsPictureInPicture(video: HTMLVideoElement): boolean {
  // An installed app on iOS claims the WebKit presentation mode below and then
  // ignores every request for it, so the control has to go by the platform.
  if (isIosWebApp()) return false;

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

function supportsPageFullscreen(): boolean {
  return document.fullscreenEnabled || typeof fullscreenTarget().webkitRequestFullscreen === "function";
}

/**
 * Safari for iPhone offers no fullscreen for elements at all: only the video
 * itself can go fullscreen, and it does so in the native player, without the
 * overlaid UI. That is the only way to fill the display there, so the GUI still
 * offers it rather than hiding the control.
 */
function supportsVideoFullscreen(video: HTMLVideoElement): boolean {
  return video.webkitSupportsPresentationMode?.("fullscreen") === true;
}

export function supportsFullscreen(video: HTMLVideoElement): boolean {
  return supportsPageFullscreen() || supportsVideoFullscreen(video);
}

export function isFullscreen(video: HTMLVideoElement): boolean {
  return (
    Boolean(document.fullscreenElement ?? document.webkitFullscreenElement) ||
    video.webkitPresentationMode === "fullscreen"
  );
}

export async function toggleFullscreen(video: HTMLVideoElement): Promise<void> {
  if (!supportsPageFullscreen() && supportsVideoFullscreen(video)) {
    video.webkitSetPresentationMode?.(video.webkitPresentationMode === "fullscreen" ? "inline" : "fullscreen");
    return;
  }

  if (isFullscreen(video)) {
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

export function observeFullscreen(video: HTMLVideoElement, onChange: () => void): () => void {
  for (const event of FULLSCREEN_EVENTS) {
    document.addEventListener(event, onChange);
  }
  // WebKit announces the fullscreen of the video itself on the element instead.
  video.addEventListener(PRESENTATION_MODE_EVENT, onChange);

  return () => {
    for (const event of FULLSCREEN_EVENTS) {
      document.removeEventListener(event, onChange);
    }
    video.removeEventListener(PRESENTATION_MODE_EVENT, onChange);
  };
}
