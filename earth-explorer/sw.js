// Earth Explorer service worker: the app shell and bundled data work offline;
// map tiles, photos and API answers are kept in a bounded runtime cache.
const VERSION = 'ee-v3';
const SHELL = [
  './', 'index.html', 'css/style.css', 'manifest.webmanifest',
  'js/main.js', 'js/animals3d.js', 'js/icons.js', 'js/reader.js', 'js/countries.js', 'js/news.js', 'js/learn.js', 'js/live.js',
  'js/data/wonders.js', 'js/data/landmarks.js', 'js/data/animals.js', 'js/data/resources.js', 'js/data/timeline.js', 'js/data/tours.js',
  'vendor/earth-libs.js', 'assets/data/countries.geojson', 'assets/data/countries-info.json',
  'assets/fonts/public-sans-latin.woff2', 'assets/fonts/newsreader-latin.woff2', 'assets/icons/icon-192.png',
];
const RUNTIME = 'ee-runtime';
const RUNTIME_MAX = 900;
const CACHEABLE = /(arcgisonline\.com|gibs\.earthdata\.nasa\.gov|cdn\.jsdelivr\.net|upload\.wikimedia\.org|flagcdn\.com|en\.wikipedia\.org\/api\/rest_v1\/page\/(summary|html|media-list))/;

self.addEventListener('install', (e) => {
  e.waitUntil(caches.open(VERSION).then((c) => c.addAll(SHELL)).then(() => self.skipWaiting()));
});
self.addEventListener('activate', (e) => {
  e.waitUntil(caches.keys().then((keys) => Promise.all(keys.filter((k) => k !== VERSION && k !== RUNTIME).map((k) => caches.delete(k)))).then(() => self.clients.claim()));
});
async function trim() {
  const c = await caches.open(RUNTIME);
  const keys = await c.keys();
  for (let i = 0; i < keys.length - RUNTIME_MAX; i++) await c.delete(keys[i]);
}
self.addEventListener('fetch', (e) => {
  const req = e.request;
  if (req.method !== 'GET') return;
  const url = new URL(req.url);
  if (url.origin === location.origin) {
    // app shell: network first so updates arrive, cache as the offline fallback
    e.respondWith(fetch(req).then((res) => { const copy = res.clone(); if (res.ok) caches.open(VERSION).then((c) => c.put(req, copy)); return res; }).catch(() => caches.match(req, { ignoreSearch: true }).then((r) => r || caches.match('index.html'))));
    return;
  }
  if (CACHEABLE.test(req.url)) {
    // tiles, photos, articles: cache first, refresh in the background
    e.respondWith(caches.open(RUNTIME).then(async (c) => {
      const hit = await c.match(req);
      const net = fetch(req).then((res) => { if (res.ok) { c.put(req, res.clone()); trim(); } return res; }).catch(() => hit);
      return hit || net;
    }));
  }
});
