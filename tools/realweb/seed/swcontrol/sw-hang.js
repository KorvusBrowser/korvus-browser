self.addEventListener('install', () => self.skipWaiting());
self.addEventListener('fetch', e => { if (e.request.url.includes('data.txt')) e.respondWith(new Promise(() => {})); });
