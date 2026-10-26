// A metrics korvus plugin: counts a context's traffic through the ABI-3 host calls, in one file built against
// korvus_plugin.h alone.
//
//   cc -shared -fPIC -O2 -I.. metrics_plugin.c -o metrics.so            # Linux
//   cc -shared -O2 -I.. metrics_plugin.c -o metrics.dylib               # macOS
//   cl /LD /MT /O2 /I.. metrics_plugin.c /Fe:metrics.dll                # Windows, a Visual Studio prompt
//
//   korvus serve --plugin ./metrics.so
//
// It shows the `host->metric(name, value, labels_json)` call ABI 3 adds: it adds to this context's running totals
// ("requests" per request, "bytes_in" per response with a Content-Length, "blocked" when it refuses one). korvus
// keeps the per-context totals and serves them at `GET /korvus/metrics`, so the counts appear with no channel of
// the plugin's own.
//
// A request to any host ending in `.blocked.invalid` is refused and counted, as the sample does, so this and the
// sample plugin can run side by side.
//
// (ABI 3 also adds `host->page_eval`; see the README. It is NOT used here on purpose: `page_eval` blocks until the
// page runs the script, so a plugin must not call it on the top-level document request -- the page is still
// fetching that document, so the call would stall the navigation. Call it from a sub-request or the plugin's own
// worker.)
#include "korvus_plugin.h"

#include <stdlib.h>
#include <string.h>

static const KorvusHost* host;

static int ends_with(const char* s, const char* tail) {
  size_t a = strlen(s), b = strlen(tail);
  return a >= b && strcmp(s + a - b, tail) == 0;
}

static void hostname_of(const char* url, char* out, size_t cap) {
  out[0] = 0;
  const char* h = strstr(url, "://");
  if (!h) return;
  h += 3;
  size_t n = strcspn(h, ":/?#");
  if (n >= cap) n = cap - 1;
  memcpy(out, h, n);
  out[n] = 0;
}

static int on_request(void* self, KorvusRequest* req) {
  (void)self;
  char hostname[256];
  hostname_of(host->req_url(req), hostname, sizeof hostname);
  if (ends_with(hostname, ".blocked.invalid")) {
    host->metric("blocked", 1.0, "{\"reason\":\"blocked.invalid\"}");
    return KORVUS_BLOCK;
  }
  host->metric("requests", 1.0, "");
  return KORVUS_CONTINUE;
}

static void on_response(void* self, KorvusResponse* res) {
  (void)self;
  const char* len = host->res_header(res, "content-length");
  if (len) {
    double n = strtod(len, NULL);
    if (n > 0) host->metric("bytes_in", n, "");
  }
}

static KorvusPlugin plugin;

KORVUS_PLUGIN_EXPORT const KorvusPlugin* korvus_plugin_init(const KorvusHost* h, const char* config_json) {
  (void)config_json;
  // This plugin needs the ABI-3 calls; decline a host too old for them.
  if (h->abi < 3) return NULL;
  host = h;
  memset(&plugin, 0, sizeof plugin);
  plugin.abi = KORVUS_PLUGIN_ABI;
  plugin.size = sizeof plugin;
  plugin.name = "metrics";
  plugin.version = "1.0";
  plugin.on_request = on_request;
  plugin.on_response = on_response;
  return &plugin;
}
