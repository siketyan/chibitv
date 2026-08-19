/**
 * The Service Worker backing the installed app.
 *
 * It is served as-is from `public/`, so it must stay plain JavaScript: the
 * bundler never sees this file, and the browser runs it exactly as written.
 *
 * Live streams and the RPC API are never cached; only the application shell and
 * the content-hashed bundles are, so that an installed app still opens while the
 * server is unreachable.
 */

const VERSION = "v1";
const SHELL_CACHE = `chibitv-shell-${VERSION}`;
const ASSET_CACHE = `chibitv-assets-${VERSION}`;
const CACHES = [SHELL_CACHE, ASSET_CACHE];

/** The entry point every navigation falls back to, matching `start_url`. */
const SHELL_URL = "/";

/** The prefix the bundler emits content-hashed assets under. */
const ASSET_PREFIX = "/static/";

/** The prefix of the RPC API, including the live stream. */
const API_PREFIX = "/api/";

/**
 * How many hashed assets to keep. Their names change on every build, so old
 * ones would pile up forever otherwise.
 */
const ASSET_LIMIT = 96;

self.addEventListener("install", (event) => {
  event.waitUntil(caches.open(SHELL_CACHE).then((cache) => cache.add(SHELL_URL)));
});

self.addEventListener("activate", (event) => {
  event.waitUntil(
    caches
      .keys()
      .then((keys) => Promise.all(keys.filter((key) => !CACHES.includes(key)).map((key) => caches.delete(key))))
      .then(() => self.clients.claim()),
  );
});

self.addEventListener("fetch", (event) => {
  const { request } = event;
  if (request.method !== "GET") return;

  const url = new URL(request.url);
  if (url.origin !== self.location.origin) return;
  if (url.pathname.startsWith(API_PREFIX)) return;

  if (request.mode === "navigate") {
    event.respondWith(handleNavigation(request));
  } else if (url.pathname.startsWith(ASSET_PREFIX)) {
    event.respondWith(handleAsset(request));
  }
});

/**
 * Navigations go to the network first so that a deployed update is picked up on
 * the next reload, and fall back to the cached shell while offline.
 */
async function handleNavigation(request) {
  try {
    const response = await fetch(request);
    if (response.ok) {
      const cache = await caches.open(SHELL_CACHE);
      await cache.put(SHELL_URL, response.clone());
    }
    return response;
  } catch (error) {
    const cached = await caches.match(SHELL_URL, { cacheName: SHELL_CACHE });
    if (cached) return cached;
    throw error;
  }
}

/** Hashed assets never change under the same URL, so the cache always wins. */
async function handleAsset(request) {
  const cache = await caches.open(ASSET_CACHE);
  const cached = await cache.match(request);
  if (cached) return cached;

  const response = await fetch(request);
  if (response.ok) {
    await cache.put(request, response.clone());
    await prune(cache);
  }
  return response;
}

async function prune(cache) {
  const keys = await cache.keys();
  // `keys()` yields the insertion order, so the excess is the least recently
  // added.
  const excess = Math.max(keys.length - ASSET_LIMIT, 0);
  await Promise.all(keys.slice(0, excess).map((key) => cache.delete(key)));
}
