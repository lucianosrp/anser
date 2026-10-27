// Which requests the service worker answers from its cache: the page only
// for navigations to / and /app/, the shell files, and nothing else. In
// particular never /login, /api/ or another navigation (that once fed a
// sign-in loop). Run: node desktop/web/sw.test.mjs
import { readFileSync } from 'node:fs';
import vm from 'node:vm';
import assert from 'node:assert/strict';

const listeners = {};
const self = {
  location: { origin: 'https://server.example.ts.net' },
  addEventListener: (type, fn) => { listeners[type] = fn; },
  clients: { claim: async () => {} },
  skipWaiting: () => {},
};
const src = readFileSync(new URL('./sw.js', import.meta.url), 'utf8');
const caches = { open: async () => ({ match: async () => null, put: () => {} }), keys: async () => [] };
vm.runInNewContext(src, { self, caches, fetch: async () => ({ ok: false }), Request: class {}, URL, Set, console });

function answered(path, { mode = 'no-cors', method = 'GET', origin = self.location.origin } = {}) {
  let took = false;
  listeners.fetch({ request: { url: origin + path, mode, method }, respondWith: () => { took = true; } });
  return took;
}

const nav = { mode: 'navigate' };
assert.equal(answered('/', nav), true, 'the page at /');
assert.equal(answered('/app/?gl=webgl', nav), true, 'the page at /app/, any query');
assert.equal(answered('/app/finance_web_bg.wasm'), true, 'a shell file');
for (const p of ['/login', '/login?next=/', '/api/export.json', '/api/ledger.json', '/api/whoami', '/sw.js', '/settings', '/app/other']) {
  assert.equal(answered(p, nav), false, `navigation to ${p} reaches the server`);
  assert.equal(answered(p), false, `fetch of ${p} reaches the server`);
}
assert.equal(answered('/app/finance_web.js', { method: 'POST' }), false, 'non-GET');
assert.equal(answered('/', { ...nav, origin: 'https://elsewhere.example' }), false, 'another origin');
console.log('sw.js: ok');
