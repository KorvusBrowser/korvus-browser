export function probe() {
  return { sameUrl: document.URL === location.href, path: location.pathname + location.search, docPath: new URL(document.URL).pathname,
    isTop: window === top, framevar: typeof FRAMEVAR, pagevar: typeof PAGEVAR, meta: new URL(import.meta.url).pathname };
}
