/** Where the Service Worker is served from, matching the file in `public/`. */
const SERVICE_WORKER_URL = "/sw.js";

/**
 * Registers the Service Worker that makes the app installable.
 *
 * A registered worker outlives the page it was registered from, and the
 * development server serves the bundles from memory instead of `dist`, so the
 * worker is only registered in a production build.
 */
export function registerServiceWorker(): void {
  if (!import.meta.env.PROD) return;
  if (!("serviceWorker" in navigator)) return;

  const register = () => {
    navigator.serviceWorker.register(SERVICE_WORKER_URL).catch((error) => {
      console.error("Failed to register the Service Worker", error);
    });
  };

  // Registering competes with the first stream for bandwidth, so it waits until
  // the page has finished loading.
  if (document.readyState === "complete") {
    register();
  } else {
    window.addEventListener("load", register, { once: true });
  }
}
