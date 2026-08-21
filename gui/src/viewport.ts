import { isIosWebApp } from "./platform";

/** The height the page sizes itself to, read by the `h-viewport` utility. */
const VIEWPORT_HEIGHT_PROPERTY = "--viewport-height";

/**
 * Sizes the page to the display of an installed app on iOS.
 *
 * `dvh` follows the browser UI that comes and goes while scrolling, which is
 * what a browser wants. An installed app has no such UI, and WebKit measures
 * the unit there as if Safari's toolbar were still on screen, leaving a gap
 * under the page. Being a dynamic unit is all that ever cleared that gap: it is
 * re-evaluated while scrolling, so a scroll shook it out.
 *
 * `innerHeight` is the viewport rather than a guess at what is left of it, so
 * measuring it is right by construction where there is no browser UI to follow.
 * Everywhere else the property stays unset and the utility keeps its `dvh`.
 */
export function trackViewportHeight(): void {
  if (!isIosWebApp()) return;

  const measure = () => {
    document.documentElement.style.setProperty(VIEWPORT_HEIGHT_PROPERTY, `${window.innerHeight}px`);
  };

  measure();
  // `orientationchange` arrives before the viewport has settled, so the resize
  // that follows it is what actually gets the new height.
  window.addEventListener("resize", measure);
  window.addEventListener("orientationchange", measure);
}
