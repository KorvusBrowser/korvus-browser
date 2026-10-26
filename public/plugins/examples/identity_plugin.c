// An identity plugin: the persona every new browser context wears, in place of korvus-stealth's (or of korvus's own,
// under `--identity korvus`). It answers the identity slot with a persona document (../README.md, Identities): a
// Chrome on a device with the cores and memory its configuration names, and korvus wears it -- that build's
// navigator, client hints and TLS, that device's hardware.
//
//   korvus fetch https://example.com/ --identity korvus --plugin ./identity.so:'{"cores": 6, "memory_gb": 8}'
//          --eval navigator.hardwareConcurrency
//
// Build: cc -shared -fPIC -O2 -I.. identity_plugin.c -o identity.so  (see ../README.md for macOS and Windows)

#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include "korvus_plugin.h"

static const KorvusHost* host;
static int cores = 4;
static int memory_gb = 8;

// The number after `"key":` in a flat JSON object, or `fallback`.
static int json_int(const char* json, const char* key, int fallback) {
  char pattern[64];
  snprintf(pattern, sizeof pattern, "\"%s\"", key);
  const char* at = json ? strstr(json, pattern) : NULL;
  if (!at) return fallback;
  at = strchr(at + strlen(pattern), ':');
  return at ? atoi(at + 1) : fallback;
}

static const char* identity(void* self, const KorvusContextInfo* ctx, const char* device_json) {
  (void)self;
  (void)device_json;
  // one persona OS per host, as korvus-stealth decides: macOS on a Mac, Windows everywhere else
  const char* os = strcmp(ctx->host_os, "macos") == 0 ? "macos" : "windows";
  char* doc = malloc(512);
  if (!doc) return NULL;
  snprintf(doc, 512,
           "{\"persona\": 1, \"name\": \"sample-identity\", "
           "\"device\": {\"os\": \"%s\", \"hardware\": {\"cores\": %d, \"memory_gb\": %d}}, "
           "\"build\": {\"browser\": \"chrome\", \"os\": \"%s\"}}",
           os, cores, memory_gb, os);
  return doc;
}

static void free_string(void* self, const char* s) {
  (void)self;
  free((void*)s);
}

KORVUS_PLUGIN_EXPORT const KorvusPlugin* korvus_plugin_init(const KorvusHost* h, const char* config_json) {
  host = h;
  cores = json_int(config_json, "cores", cores);
  memory_gb = json_int(config_json, "memory_gb", memory_gb);
  static KorvusPlugin plugin;
  memset(&plugin, 0, sizeof plugin);
  plugin.abi = KORVUS_PLUGIN_ABI;
  plugin.size = sizeof plugin;
  plugin.name = "sample-identity";
  plugin.version = "1.0.0";
  plugin.identity = identity;
  plugin.free_string = free_string;
  return &plugin;
}
