// Service worker for the estrogen demo. It makes the page installable and lets
// it open offline after a first visit. The page itself and the files whose
// names do not change (the manifest, the icons) go to the network first and
// fall back to the cache offline, so a deploy reaches returning visitors on
// their next load. The build's hashed files under /_astro/ never change under
// one name, so they are served cache-first. Cross-origin requests (the Holo
// stylesheets and the fonts) are left to the network.

// v1 served the page cache-first and kept it; a new name drops that cache.
const CACHE = 'estrogen-v2';

self.addEventListener('install', () => {
	self.skipWaiting();
});

self.addEventListener('activate', (event) => {
	event.waitUntil(
		(async () => {
			const names = await caches.keys();
			await Promise.all(
				names.filter((name) => name !== CACHE).map((name) => caches.delete(name)),
			);
			await self.clients.claim();
		})(),
	);
});

self.addEventListener('fetch', (event) => {
	const request = event.request;
	if (request.method !== 'GET') return;
	const url = new URL(request.url);
	if (url.origin !== self.location.origin) return;
	const hashed = url.pathname.startsWith('/_astro/');
	event.respondWith(
		(async () => {
			const cache = await caches.open(CACHE);
			if (hashed) {
				const hit = await cache.match(request);
				if (hit) return hit;
			}
			try {
				const response = await fetch(request);
				if (response.ok) cache.put(request, response.clone());
				return response;
			} catch (err) {
				const hit = await cache.match(request);
				if (hit) return hit;
				throw err;
			}
		})(),
	);
});
