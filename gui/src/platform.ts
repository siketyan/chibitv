/**
 * Whether this is an app installed to the Home Screen on iOS.
 *
 * `standalone` is set by Safari for iOS alone, and only there, so it says both
 * which platform this is and that it is not running in a browser tab. WebKit
 * gets a couple of things wrong in that mode alone, and this is what the code
 * working around them keys off.
 */
export function isIosWebApp(): boolean {
  return window.navigator.standalone === true;
}
