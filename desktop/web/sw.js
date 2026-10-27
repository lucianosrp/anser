// The web app's service worker, served at /sw.js (root scope).
//
// The shell (the page, the JS glue, the wasm with its bundled fonts, the
// manifest and icons) is cached under a name tied to the build, so the app
// opens without the network once installed. Cache-first: the browser
// fetches this file again on every navigation (the server sends it
// no-cache), and a new build has a new BUILD below, so it installs beside
// the running one and waits; the page then offers "Update available,
// reload" (index.html), which tells it to take over.
//
// /api/ (the data), /login and every non-GET request always go to the
// network and are never stored.
const BUILD = '__BUILD__';
const CACHE = `finance-shell-${BUILD}`;
const SHELL = [
  '/app/',
  '/app/finance_web.js',
  '/app/finance_web_bg.wasm',
  '/app/manifest.webmanifest',
  '/app/icon.svg',
  '/app/icon-192.png',
  '/app/icon-512.png',
  '/app/icon-maskable-192.png',
  '/app/icon-maskable-512.png',
  '/app/apple-touch-icon.png',
];

self.addEventListener('install', (event) => {
  // cache: 'reload' skips the HTTP cache, so the new build's files come in
  event.waitUntil(caches.open(CACHE).then((c) => c.addAll(SHELL.map((u) => new Request(u, { cache: 'reload' })))));
});

self.addEventListener('activate', (event) => {
  event.waitUntil((async () => {
    for (const name of await caches.keys()) {
      if (name.startsWith('finance-shell-') && name !== CACHE) await caches.delete(name);
    }
    await self.clients.claim();
  })());
});

self.addEventListener('message', (event) => {
  if (event.data === 'skip-waiting') self.skipWaiting();
});

// Only ever the shell: the page for a navigation to exactly / or /app/, and
// the files in SHELL. Never /login, /api/, /sw.js or any other navigation:
// those always reach the server. (Answering another navigation with the
// cached page would turn a redirect to a server page into the app again,
// which fetches, is refused, and goes round: the loop this rule prevents.)
const PAGES = new Set(['/', '/app/']);
const NEVER = (p) => p === '/login' || p === '/sw.js' || p.startsWith('/api/');

self.addEventListener('fetch', (event) => {
  const req = event.request;
  const url = new URL(req.url);
  if (req.method !== 'GET' || url.origin !== self.location.origin) return;
  if (NEVER(url.pathname)) return;
  // the page is the same file at / and /app/, whatever the query (?gl=, ?timing=)
  const page = req.mode === 'navigate' && PAGES.has(url.pathname);
  if (req.mode === 'navigate' && !page) return;
  if (!page && !SHELL.includes(url.pathname)) return;
  event.respondWith((async () => {
    const cache = await caches.open(CACHE);
    const hit = await cache.match(page ? '/app/' : url.pathname);
    if (hit) return hit;
    const res = await fetch(req);
    if (res.ok && !page) cache.put(url.pathname, res.clone());
    return res;
  })());
});
