// A sample korvus plugin: one file, built against korvus_plugin.h alone.
//
//   cc -shared -fPIC -O2 -I.. sample_plugin.c -o sample.so            # Linux
//   cc -shared -O2 -I.. sample_plugin.c -o sample.dylib               # macOS
//   cl /LD /MT /O2 /I.. sample_plugin.c /Fe:sample.dll                # Windows, a Visual Studio prompt
//
//   korvus serve --plugin ./sample.so:'{"cores": 12}'
//
// It fills three slots:
//   * Web API answers: `navigator.hardwareConcurrency` (in the page and in its workers) answers the configured
//     number of cores, 13 when none is configured;
//   * the network: every request carries `X-Korvus-Sample: 1`, and a request to any host ending in
//     `.blocked.invalid` is refused before it is sent;
//   * an observer: it counts the events it saw and logs the count when the process ends.
#include "korvus_plugin.h"

#include <stdio.h>
#include <stdlib.h>
#include <string.h>

static const KorvusHost* host;
static double cores = 13;
static unsigned long events;

static const char* const labels[] = {
    "Navigator.get hardwareConcurrency",
    "WorkerNavigator.get hardwareConcurrency",
    NULL,
};

static void on_call(void* self, KorvusCall* call) {
  (void)self;
  const char* label = host->label(call);
  if (strstr(label, "hardwareConcurrency")) host->set_number(call, cores);
}

static int ends_with(const char* s, const char* tail) {
  size_t a = strlen(s), b = strlen(tail);
  return a >= b && strcmp(s + a - b, tail) == 0;
}

static int on_request(void* self, KorvusRequest* req) {
  (void)self;
  const char* url = host->req_url(req);
  // the host part of scheme://host[:port]/path
  const char* h = strstr(url, "://");
  char hostname[256] = {0};
  if (h) {
    h += 3;
    size_t n = strcspn(h, ":/?#");
    if (n >= sizeof hostname) n = sizeof hostname - 1;
    memcpy(hostname, h, n);
  }
  if (ends_with(hostname, ".blocked.invalid")) return KORVUS_BLOCK;
  host->req_set_header(req, "X-Korvus-Sample", "1");
  return KORVUS_CONTINUE;
}

static void on_event(void* self, const KorvusEvent* ev) {
  (void)self;
  (void)ev;
  events++;
}

static void sample_shutdown(void* self) {
  (void)self;
  char line[96];
  snprintf(line, sizeof line, "sample: saw %lu event(s)", events);
  host->log(KORVUS_LOG_INFO, line);
}

static KorvusPlugin plugin;

KORVUS_PLUGIN_EXPORT const KorvusPlugin* korvus_plugin_init(const KorvusHost* h, const char* config_json) {
  if (h->abi < 1) return NULL;
  host = h;
  // `{"cores": N}` -- a sample's JSON reading: find the key, read the number after it.
  const char* k = config_json ? strstr(config_json, "\"cores\"") : NULL;
  if (k) {
    const char* colon = strchr(k, ':');
    if (colon) {
      double n = strtod(colon + 1, NULL);
      if (n >= 1 && n <= 256) cores = n;
    }
  }
  memset(&plugin, 0, sizeof plugin);
  plugin.abi = KORVUS_PLUGIN_ABI;
  plugin.size = sizeof plugin;
  plugin.name = "sample";
  plugin.version = "1.0";
  plugin.labels = labels;
  plugin.on_call = on_call;
  plugin.on_request = on_request;
  plugin.on_event = on_event;
  plugin.shutdown = sample_shutdown;
  return &plugin;
}
