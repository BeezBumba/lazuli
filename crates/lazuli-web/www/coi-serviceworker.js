/**
 * coi-serviceworker.js — Cross-Origin Isolation service worker shim.
 *
 * Intercepts same-origin fetches and injects the two HTTP headers that
 * browsers require before `SharedArrayBuffer` and `Atomics` are available:
 *
 *   Cross-Origin-Opener-Policy:   same-origin
 *   Cross-Origin-Embedder-Policy: require-corp
 *
 * When these headers are present `window.crossOriginIsolated` is `true`,
 * which is a prerequisite for the CPU Worker shared-memory ring buffer
 * (Phase 3 SAB audio) and any future shared WASM memory.
 *
 * ## Usage
 *
 *   Register this file as a service worker from the page's `<head>`:
 *
 *   ```html
 *   <script>
 *     if ("serviceWorker" in navigator) {
 *       navigator.serviceWorker.register("./coi-serviceworker.js").then((r) => {
 *         if (!navigator.serviceWorker.controller) window.location.reload();
 *       });
 *     }
 *   </script>
 *   ```
 *
 *   The first visit installs the worker and reloads the page so the new
 *   headers take effect immediately.  Subsequent visits are served directly
 *   by the worker.
 *
 * ## Compatibility
 *
 *   Works on any static host (GitHub Pages, Python http.server, etc.) that
 *   cannot set custom response headers at the server level.  The production
 *   `web-serve` recipe in the project Justfile serves with native COOP/COEP
 *   headers and does not need this shim.
 */

// Install immediately — no waiting for existing service workers.
self.addEventListener("install", () => self.skipWaiting());

// Claim all open clients so the new headers take effect without a reload.
self.addEventListener("activate", (ev) =>
  ev.waitUntil(self.clients.claim()),
);

self.addEventListener("fetch", (ev) => {
  const url = new URL(ev.request.url);

  // Only add headers for same-origin requests.  Cross-origin resources
  // (CDN fonts, analytics, etc.) must serve their own CORP headers; injecting
  // COOP/COEP into their responses would corrupt them.
  if (url.origin !== self.location.origin) return;

  ev.respondWith(
    fetch(ev.request).then((resp) => {
      // Preserve all original headers and add the isolation pair.
      const headers = new Headers(resp.headers);
      headers.set("Cross-Origin-Opener-Policy",   "same-origin");
      headers.set("Cross-Origin-Embedder-Policy", "require-corp");

      return new Response(resp.body, {
        status:     resp.status,
        statusText: resp.statusText,
        headers,
      });
    }),
  );
});
