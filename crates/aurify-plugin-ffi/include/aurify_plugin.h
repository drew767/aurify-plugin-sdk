/* C ABI of the Aurify plugin SDK. Hand-written and kept in step with src/lib.rs;
 * the binding tests fail when the two disagree on a signature.
 *
 * Strings are NUL-terminated UTF-8. Every string this library returns must go back to
 * aurify_plugin_string_free. Every string a callback returns must have been made by
 * aurify_plugin_string_alloc, so each side frees with the allocator that allocated. */

#ifndef AURIFY_PLUGIN_H
#define AURIFY_PLUGIN_H

#include <stdbool.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

/* A running plugin: local server plus platform client. Opaque. */
typedef struct AurifyPluginHost AurifyPluginHost;

/* Runs one declared operation.
 *   name       operation name from the manifest
 *   args_json  the operation's arguments as a JSON object
 *   user_data  the pointer given to aurify_plugin_host_start
 * Returns a string from aurify_plugin_string_alloc holding either
 *   {"result": ...}   or   {"error": {"code": "...", "message": "..."}}
 * NULL is reported to the client as an internal error. */
typedef char *(*aurify_plugin_operation_fn)(const char *name, const char *args_json, void *user_data);

/* Version of the native library. Static; do not free. */
const char *aurify_plugin_version(void);

/* Copies text into a string owned by this library. */
char *aurify_plugin_string_alloc(const char *text);

/* Frees a string owned by this library. NULL is ignored. */
void aurify_plugin_string_free(char *text);

/* NULL when the manifest is valid; otherwise a JSON array of messages. */
char *aurify_plugin_manifest_validate(const char *manifest_json);

/* Validates the manifest, reads the launch context, binds 127.0.0.1:port.
 * launch_json carries {"port","secret","platformUrl","platformToken","dataDir"} as
 * strings; NULL reads AURIFY_PLUGIN_* from this process's environment. Bindings pass
 * the values explicitly: their runtime does not always share the environment with
 * this library (.NET on Unix keeps its own copy).
 * On failure returns NULL and, when error_out is not NULL, stores a message in it. */
AurifyPluginHost *aurify_plugin_host_start(const char *manifest_json,
                                           const char *launch_json,
                                           aurify_plugin_operation_fn on_operation,
                                           void *user_data,
                                           char **error_out);

uint16_t aurify_plugin_host_port(const AurifyPluginHost *host);

/* status: "starting", "ready" or "failed". message may be NULL. */
void aurify_plugin_host_set_health(AurifyPluginHost *host, const char *status, const char *message);

/* Whether the launch environment carried AURIFY_PLATFORM_URL and AURIFY_PLATFORM_TOKEN. */
bool aurify_plugin_host_has_platform(const AurifyPluginHost *host);

/* One call to the platform on the person's behalf. body_json may be NULL.
 * Returns the answer as JSON ("null" for an empty answer) or NULL with error_out filled. */
char *aurify_plugin_platform_call(const AurifyPluginHost *host,
                                  const char *method,
                                  const char *path,
                                  const char *body_json,
                                  char **error_out);

/* Stops the local server and releases the host. NULL is ignored. */
void aurify_plugin_host_stop(AurifyPluginHost *host);

#ifdef __cplusplus
}
#endif

#endif /* AURIFY_PLUGIN_H */
