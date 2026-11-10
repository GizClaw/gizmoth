/* Per-port H2Loader adapter over the pinned GizOS Host Core. The flow mirrors
 * GizOS tools/gizloader/native/session.c, but every connection-scoped value
 * lives in gm_h2l_t so one daemon process can own many ports. */
#include "gizmoth_serial_core.h"

#include <stdarg.h>
#include <stdatomic.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include "gizmoth_serial_pal.h"
#include "h2_bleikcp.h"
#include "h2_h2loader_cli_target.h"
#include "h2_h2loader_host_package.h"
#include "projects/h2loader/apps/cli/app/src/h2_h2loader_cli_internal.h"

#ifdef _WIN32
#define WIN32_LEAN_AND_MEAN
#include <windows.h>
#endif

#define GM_MAX_PACKAGE_BYTES (256ull * 1024ull * 1024ull)
#define GM_STATUS_TIMEOUT_MS 4000u
#define GM_COMMAND_TIMEOUT_MS 120000u
#define GM_CANCEL_POLL_MS 10u

struct gm_h2l {
  char port[H2_PAL_SERIAL_HOST_PORT_ID_MAX_LEN];
  uint32_t handshake_timeout_ms;
  gm_h2l_log_fn on_log;
  void* log_user;
  h2_h2loader_host_serial_connection_t* connection;
  h2_h2loader_host_status_t status;
  char identity[H2_H2LOADER_HOST_DEVICE_UID_MAX_LEN];
  gm_h2l_cancel_fn cancel;
  void* cancel_user;
  const h2_pal_time_api_t* base_time;
  h2_pal_time_api_t time;
  h2_pal_serial_host_api_t serial;
  uint64_t guard_start_ms;
  uint32_t guard_timeout_ms;
  int guard_active;
};

typedef struct writer {
  char* out;
  size_t cap;
  size_t len;
  int overflow;
} writer_t;

static void w_raw(writer_t* w, const char* text, size_t n) {
  if (w->overflow || w->len + n + 1u > w->cap) {
    w->overflow = 1;
    return;
  }
  memcpy(w->out + w->len, text, n);
  w->len += n;
  w->out[w->len] = 0;
}
/* Concatenation rejects pointers: this helper accepts string literals only. */
#define W_LITERAL(writer, literal) \
  w_raw((writer), "" literal, sizeof("" literal) - 1u)

static void w_fmt(writer_t* w, const char* format, ...) {
  char buffer[96];
  va_list args;
  va_start(args, format);
  int n = vsnprintf(buffer, sizeof(buffer), format, args);
  va_end(args);
  if (n < 0 || (size_t)n >= sizeof(buffer)) {
    w->overflow = 1;
    return;
  }
  w_raw(w, buffer, (size_t)n);
}
static void w_str(writer_t* w, const char* s) {
  W_LITERAL(w, "\"");
  for (const unsigned char* p = (const unsigned char*)s; *p; ++p) {
    if (*p == '"' || *p == '\\') {
      char e[2] = {'\\', (char)*p};
      w_raw(w, e, 2);
    } else if (*p < 32u) {
      w_fmt(w, "\\u%04x", (unsigned)*p);
    } else {
      w_raw(w, (const char*)p, 1);
    }
  }
  W_LITERAL(w, "\"");
}
static int w_done(writer_t* w) { return w->overflow ? H2_PAL_ERR_FULL : 0; }

static const char* role_name(h2_h2loader_host_active_role_t role) {
  return role == H2_H2LOADER_HOST_ACTIVE_ROLE_APP      ? "app"
         : role == H2_H2LOADER_HOST_ACTIVE_ROLE_LOADER ? "loader"
                                                       : "unknown";
}
static void w_metadata(writer_t* w, const h2_h2loader_host_metadata_t* m) {
  w_fmt(w, "{\"valid\":%s,\"packageSize\":%llu,\"imageSize\":%llu",
        m->valid ? "true" : "false", (unsigned long long)m->package_size,
        (unsigned long long)m->image_size);
  W_LITERAL(w, ",\"packageChecksum\":");
  w_str(w, m->package_checksum);
  W_LITERAL(w, ",\"imageChecksum\":");
  w_str(w, m->image_checksum);
  W_LITERAL(w, ",\"version\":");
  w_str(w, m->version);
  W_LITERAL(w, ",\"board\":");
  w_str(w, m->board);
  W_LITERAL(w, ",\"target\":");
  w_str(w, m->target);
  W_LITERAL(w, ",\"role\":");
  w_str(w, role_name(m->role));
  W_LITERAL(w, "}");
}
static void w_status(writer_t* w, const h2_h2loader_host_status_t* s) {
  W_LITERAL(w, "{\"board\":");
  w_str(w, s->board);
  W_LITERAL(w, ",\"target\":");
  w_str(w, s->target);
  W_LITERAL(w, ",\"chip\":");
  w_str(w, s->chip);
  W_LITERAL(w, ",\"deviceUid\":");
  w_str(w, s->device_uid);
  w_fmt(w, ",\"capabilities\":%u,\"commandAvailability\":%u",
        (unsigned)s->capabilities, (unsigned)s->command_availability);
  W_LITERAL(w, ",\"active\":{\"role\":");
  w_str(w, role_name(s->active_role));
  W_LITERAL(w, ",\"version\":");
  w_str(w, s->active_version);
  W_LITERAL(w, ",\"imageChecksum\":");
  w_str(w, s->active_checksum);
  w_fmt(w, ",\"imageSize\":%llu}", (unsigned long long)s->active_image_size);
  w_fmt(w, ",\"runningPartition\":%u,\"nextPartition\":%u",
        (unsigned)s->running_partition, (unsigned)s->next_partition);
  W_LITERAL(w, ",\"bootIntent\":");
  w_str(w, s->boot_intent == H2_H2LOADER_HOST_BOOT_INTENT_LOADER ? "loader"
                                                                 : "auto");
  W_LITERAL(w, ",\"stage\":");
  w_metadata(w, &s->stage);
  W_LITERAL(w, ",\"partition1\":");
  w_metadata(w, &s->partition_1);
  W_LITERAL(w, ",\"partition2\":");
  w_metadata(w, &s->partition_2);
  w_fmt(w, ",\"lastResult\":%d}", (int)s->last);
}

static atomic_int started;
static atomic_int start_result;

int gm_h2l_abi_version(void) { return GM_H2L_ABI_VERSION; }

int gm_h2l_global_start(void) {
  /* The Rust daemon serializes this call through std::sync::Once. */
  if (atomic_load(&started)) return atomic_load(&start_result);
  h2_pal_result_t rc = h2_h2loader_cli_target_start();
  if (rc == H2_PAL_OK) rc = h2_bleikcp_global_init();
  atomic_store(&start_result, rc);
  atomic_store(&started, 1);
  return rc;
}

const char* gm_h2l_result_name(int result) {
  switch (result) {
    case H2_PAL_OK:
      return "ok";
    case H2_PAL_EXIT:
      return "cancelled";
    case H2_PAL_ERR_INVALID_ARG:
      return "invalid_argument";
    case H2_PAL_ERR_UNAVAILABLE:
      return "unavailable";
    case H2_PAL_ERR_UNSUPPORTED:
      return "unsupported";
    case H2_PAL_ERR_IO:
      return "io";
    case H2_PAL_ERR_NO_MEMORY:
      return "no_memory";
    case H2_PAL_ERR_TIMEOUT:
      return "timeout";
    case H2_PAL_ERR_INVALID_STATE:
      return "invalid_state";
    case H2_PAL_ERR_NOT_FOUND:
      return "not_found";
    case H2_PAL_ERR_CLOSED:
      return "closed";
    case H2_PAL_ERR_FULL:
      return "full";
    case H2_PAL_ERR_BUSY:
      return "busy";
    case H2_PAL_ERR_FORMAT:
      return "format";
    case H2_PAL_ERR_TRUNCATED:
      return "truncated";
    default:
      return "error";
  }
}

static h2_pal_result_t forward_log(void* user, const uint8_t* data,
                                   size_t len) {
  gm_h2l_t* ctx = user;
  if (ctx->on_log && len) ctx->on_log(ctx->log_user, data, len);
  return H2_PAL_OK;
}
static int ctx_cancelled(void* user) {
  gm_h2l_t* ctx = user;
  return ctx->cancel && ctx->cancel(ctx->cancel_user);
}

/* The pinned core polls its injected clock in admission, read_until and
 * wait_delivery, but those paths do not accept a cancellation callback.
 * Returning the control result from this per-context clock interrupts those
 * loops without changing Host Core or shortening flash/transfer command
 * deadlines. */
static h2_pal_result_t guarded_now(void* user, uint64_t* out_ms) {
  gm_h2l_t* ctx = user;
  if (ctx->guard_active && ctx_cancelled(ctx)) return H2_PAL_EXIT;
  h2_pal_result_t rc = h2_pal_time_get_monotonic_ms(ctx->base_time, out_ms);
  if (rc != H2_PAL_OK || !ctx->guard_active) return rc;
  if (*out_ms < ctx->guard_start_ms) return H2_PAL_ERR_INVALID_STATE;
  if (ctx->guard_timeout_ms &&
      *out_ms - ctx->guard_start_ms >= ctx->guard_timeout_ms)
    return H2_PAL_ERR_TIMEOUT;
  return H2_PAL_OK;
}

static h2_pal_result_t guarded_sleep(void* user, uint32_t ms) {
  gm_h2l_t* ctx = user;
  if (!ctx->guard_active) return h2_pal_time_sleep_ms(ctx->base_time, ms);
  uint64_t now;
  h2_pal_result_t rc = guarded_now(ctx, &now);
  while (rc == H2_PAL_OK && ms) {
    uint32_t slice = ms < GM_CANCEL_POLL_MS ? ms : GM_CANCEL_POLL_MS;
    rc = h2_pal_time_sleep_ms(ctx->base_time, slice);
    if (rc == H2_PAL_OK) rc = guarded_now(ctx, &now);
    ms -= slice;
  }
  return rc;
}

static const h2_pal_time_vtable_t guarded_time_vtable = {
    .get_monotonic_ms = guarded_now,
    .sleep_ms = guarded_sleep,
};

static void guard_clear(gm_h2l_t* ctx) {
  ctx->guard_active = 0;
  ctx->guard_timeout_ms = 0;
}

static h2_pal_result_t guard_begin(gm_h2l_t* ctx, uint32_t timeout_ms) {
  guard_clear(ctx);
  if (ctx_cancelled(ctx)) return H2_PAL_EXIT;
  h2_pal_result_t rc =
      h2_pal_time_get_monotonic_ms(ctx->base_time, &ctx->guard_start_ms);
  if (rc != H2_PAL_OK) return rc;
  ctx->guard_timeout_ms = timeout_ms;
  ctx->guard_active = 1;
  return H2_PAL_OK;
}

static h2_pal_result_t guard_finish(gm_h2l_t* ctx, h2_pal_result_t rc) {
  uint64_t now;
  if (rc == H2_PAL_OK) rc = guarded_now(ctx, &now);
  guard_clear(ctx);
  return rc;
}

static void cancel_clear(gm_h2l_t* ctx) {
  guard_clear(ctx);
  ctx->cancel = NULL;
  ctx->cancel_user = NULL;
}

static void disconnect(gm_h2l_t* ctx) {
  /* Teardown has its own bounded grace/CLOSE deadlines. Do not let an expired
   * operation guard or a borrowed callback interfere with releasing the fd. */
  guard_clear(ctx);
  (void)h2_h2loader_host_serial_disconnect(&ctx->connection);
}

static h2_pal_result_t read_status(gm_h2l_t* ctx) {
  h2_pal_result_t rc = guard_begin(ctx, GM_STATUS_TIMEOUT_MS);
  if (rc == H2_PAL_OK)
    rc = h2_h2loader_host_serial_read_status(ctx->connection, &ctx->status);
  return guard_finish(ctx, rc);
}

static h2_pal_result_t connect_ctx(gm_h2l_t* ctx) {
  if (ctx_cancelled(ctx)) return H2_PAL_EXIT;
  if (ctx->connection) return H2_PAL_OK;
  h2_h2loader_host_serial_connection_config_t config = {
      .serial = &ctx->serial,
      .time = &ctx->time,
      .allocator = h2_h2loader_cli_target_mem(),
      .port_id = ctx->port,
      .baud_rate = H2_H2LOADER_HOST_RELIABLE_SERIAL_BAUD,
      .handshake_timeout_ms = ctx->handshake_timeout_ms,
      .command_timeout_ms = GM_COMMAND_TIMEOUT_MS,
      .on_log = forward_log,
      .log_user = ctx,
      .preserve_control_lines = 1,
  };
  h2_pal_result_t rc = guard_begin(ctx, ctx->handshake_timeout_ms);
  if (rc == H2_PAL_OK)
    rc = h2_h2loader_host_serial_connect(&config, &ctx->connection);
  rc = guard_finish(ctx, rc);
  if (rc == H2_PAL_OK) rc = read_status(ctx);
  if (rc == H2_PAL_OK && ctx->identity[0] &&
      strcmp(ctx->identity, ctx->status.device_uid))
    rc = H2_PAL_ERR_INVALID_STATE; /* a different board answered: fail closed */
  if (rc == H2_PAL_OK && !ctx->identity[0])
    (void)snprintf(ctx->identity, sizeof(ctx->identity), "%s",
                   ctx->status.device_uid);
  if (rc != H2_PAL_OK) disconnect(ctx);
  return rc;
}

int gm_h2l_open(const char* port, uint32_t handshake_timeout_ms,
                gm_h2l_log_fn on_log, void* log_user, gm_h2l_cancel_fn cancel,
                void* cancel_user, gm_h2l_t** out) {
  if (!port || !port[0] || !out || !cancel ||
      strlen(port) >= H2_PAL_SERIAL_HOST_PORT_ID_MAX_LEN)
    return H2_PAL_ERR_INVALID_ARG;
  *out = NULL;
  int rc = gm_h2l_global_start();
  if (rc != H2_PAL_OK) return rc;
  gm_h2l_t* ctx = calloc(1, sizeof(*ctx));
  if (!ctx) return H2_PAL_ERR_NO_MEMORY;
  (void)snprintf(ctx->port, sizeof(ctx->port), "%s", port);
  ctx->handshake_timeout_ms =
      handshake_timeout_ms ? handshake_timeout_ms : 8000u;
  ctx->on_log = on_log;
  ctx->log_user = log_user;
  ctx->cancel = cancel;
  ctx->cancel_user = cancel_user;
  ctx->base_time = h2_h2loader_cli_target_time();
  ctx->time.user = ctx;
  ctx->time.vtable = &guarded_time_vtable;
  gm_serial_pal_init(&ctx->serial, &ctx->time);
  rc = connect_ctx(ctx);
  cancel_clear(ctx);
  if (rc != H2_PAL_OK) {
    free(ctx);
    return rc;
  }
  *out = ctx;
  return H2_PAL_OK;
}

void gm_h2l_close(gm_h2l_t** inout) {
  if (!inout || !*inout) return;
  disconnect(*inout);
  free(*inout);
  *inout = NULL;
}

int gm_h2l_status_json(gm_h2l_t* ctx, char* out, size_t out_cap,
                       gm_h2l_cancel_fn cancel, void* cancel_user) {
  if (!ctx || !out || !out_cap || !cancel) return H2_PAL_ERR_INVALID_ARG;
  ctx->cancel = cancel;
  ctx->cancel_user = cancel_user;
  h2_pal_result_t rc = connect_ctx(ctx);
  if (rc == H2_PAL_OK) rc = read_status(ctx);
  cancel_clear(ctx);
  if (rc != H2_PAL_OK) {
    disconnect(ctx);
    return rc;
  }
  writer_t w = {out, out_cap, 0, 0};
  out[0] = 0;
  w_status(&w, &ctx->status);
  return w_done(&w);
}

int gm_h2l_monitor(gm_h2l_t* ctx, gm_h2l_cancel_fn cancel, void* cancel_user) {
  if (!ctx || !cancel) return H2_PAL_ERR_INVALID_ARG;
  ctx->cancel = cancel;
  ctx->cancel_user = cancel_user;
  h2_pal_result_t rc = connect_ctx(ctx);
  if (rc == H2_PAL_OK) rc = guard_begin(ctx, 0u);
  if (rc == H2_PAL_OK)
    rc = h2_h2loader_host_serial_monitor_logs(ctx->connection, ctx_cancelled,
                                              ctx);
  cancel_clear(ctx);
  if (rc != H2_PAL_OK && rc != H2_PAL_EXIT) disconnect(ctx);
  return rc;
}

typedef struct file_source {
  FILE* file;
} file_source_t;

static int seek64(FILE* f, uint64_t offset) {
#ifdef _WIN32
  return _fseeki64(f, (__int64)offset, SEEK_SET);
#else
  return fseeko(f, (off_t)offset, SEEK_SET);
#endif
}
static h2_pal_result_t file_read(void* user, uint64_t offset, uint8_t* out,
                                 size_t size, size_t* read) {
  file_source_t* source = user;
  *read = 0;
  if (seek64(source->file, offset)) return H2_PAL_ERR_IO;
  size_t n = fread(out, 1, size, source->file);
  if (n < size && ferror(source->file)) return H2_PAL_ERR_IO;
  *read = n;
  return H2_PAL_OK;
}
static FILE* open_binary(const char* path) {
#ifdef _WIN32
  wchar_t wide[4096];
  if (!MultiByteToWideChar(CP_UTF8, MB_ERR_INVALID_CHARS, path, -1, wide, 4096))
    return NULL;
  return _wfopen(wide, L"rb");
#else
  return fopen(path, "rb");
#endif
}
static h2_pal_result_t inspect_file(const char* path, file_source_t* source,
                                    h2_h2loader_host_catalog_entry_t* asset) {
  if (!path || !path[0]) return H2_PAL_ERR_INVALID_ARG;
  source->file = open_binary(path);
  if (!source->file) return H2_PAL_ERR_NOT_FOUND;
  if (seek64(source->file, 0) || fseek(source->file, 0, SEEK_END))
    return H2_PAL_ERR_IO;
#ifdef _WIN32
  __int64 end = _ftelli64(source->file);
#else
  off_t end = ftello(source->file);
#endif
  if (end <= 0 || (uint64_t)end > GM_MAX_PACKAGE_BYTES)
    return H2_PAL_ERR_INVALID_ARG;
  h2_h2loader_host_package_inspect_config_t inspect = {
      .allocator = h2_h2loader_cli_target_mem(),
      .read_payload = file_read,
      .payload_user = source,
      .payload_bytes = (uint64_t)end,
  };
  return h2_h2loader_host_package_inspect(&inspect, asset);
}

int gm_h2l_inspect_json(const char* path, char* out, size_t out_cap) {
  if (!out || !out_cap) return H2_PAL_ERR_INVALID_ARG;
  int rc = gm_h2l_global_start();
  if (rc != H2_PAL_OK) return rc;
  file_source_t source = {0};
  h2_h2loader_host_catalog_entry_t asset = {0};
  rc = inspect_file(path, &source, &asset);
  if (source.file) fclose(source.file);
  if (rc != H2_PAL_OK) return rc;
  writer_t w = {out, out_cap, 0, 0};
  out[0] = 0;
  W_LITERAL(&w, "{\"board\":");
  w_str(&w, asset.board);
  W_LITERAL(&w, ",\"target\":");
  w_str(&w, asset.target);
  W_LITERAL(&w, ",\"version\":");
  w_str(&w, asset.version);
  W_LITERAL(&w, ",\"role\":");
  w_str(&w,
        asset.role == H2_H2LOADER_HOST_ASSET_ROLE_LOADER ? "loader" : "app");
  W_LITERAL(&w, ",\"sha256\":");
  w_str(&w, asset.sha256);
  w_fmt(&w, ",\"bytes\":%llu,\"format\":%u}", (unsigned long long)asset.bytes,
        (unsigned)(asset.package_format ? asset.package_format : 1u));
  return w_done(&w);
}

int gm_h2l_stage(gm_h2l_t* ctx, const char* path, gm_h2l_cancel_fn cancel,
                 void* cancel_user, gm_h2l_progress_fn progress,
                 void* progress_user) {
  if (!ctx || !cancel) return H2_PAL_ERR_INVALID_ARG;
  ctx->cancel = cancel;
  ctx->cancel_user = cancel_user;
  h2_pal_result_t rc = connect_ctx(ctx);
  if (rc == H2_PAL_OK) rc = read_status(ctx);
  file_source_t source = {0};
  h2_h2loader_host_catalog_entry_t asset = {0};
  if (rc == H2_PAL_OK) rc = inspect_file(path, &source, &asset);
  if (rc == H2_PAL_OK && (strcmp(ctx->status.board, asset.board) ||
                          strcmp(ctx->status.target, asset.target) ||
                          !(ctx->status.command_availability &
                            H2_H2LOADER_HOST_COMMAND_AVAILABLE_STAGE_PAYLOAD)))
    rc = H2_PAL_ERR_INVALID_STATE;
  if (rc == H2_PAL_OK) rc = guard_begin(ctx, 0u);
  if (rc == H2_PAL_OK)
    rc = h2_h2loader_host_serial_stage(
        ctx->connection, &asset, file_read, &source, ctx_cancelled, ctx,
        (h2_h2loader_host_progress_fn)progress, progress_user);
  rc = guard_finish(ctx, rc);
  if (source.file) fclose(source.file);
  if (rc == H2_PAL_OK) {
    /* Verify the durable Stage on the same connection; reconnect only after a
     * transport failure and never replay an uncertain transfer. */
    rc = read_status(ctx);
    if (rc != H2_PAL_OK && rc != H2_PAL_EXIT && !ctx_cancelled(ctx)) {
      disconnect(ctx);
      rc = connect_ctx(ctx);
    }
    if (rc == H2_PAL_OK &&
        (!ctx->status.stage.valid ||
         ctx->status.stage.package_size != asset.bytes ||
         strcmp(ctx->status.stage.package_checksum, asset.sha256)))
      rc = H2_PAL_ERR_INVALID_STATE;
  }
  if (rc != H2_PAL_OK) disconnect(ctx);
  cancel_clear(ctx);
  return rc;
}

typedef struct confirmation_wait {
  gm_h2l_t* ctx;
  uint64_t start_ms;
  h2_pal_result_t control_result;
  int deadline_reached;
} confirmation_wait_t;

static int confirmation_wait_done(void* user) {
  confirmation_wait_t* wait = user;
  if (ctx_cancelled(wait->ctx)) {
    wait->control_result = H2_PAL_EXIT;
    return 1;
  }
  uint64_t now_ms = 0u;
  wait->control_result =
      h2_pal_time_get_monotonic_ms(h2_h2loader_cli_target_time(), &now_ms);
  if (wait->control_result != H2_PAL_OK || now_ms < wait->start_ms) {
    wait->control_result = H2_PAL_ERR_INVALID_STATE;
    return 1;
  }
  wait->deadline_reached = now_ms - wait->start_ms >= 500u;
  return wait->deadline_reached;
}

int gm_h2l_reboot(gm_h2l_t* ctx, gm_h2l_reboot_t kind, gm_h2l_cancel_fn cancel,
                  void* cancel_user) {
  if (!ctx || !cancel) return H2_PAL_ERR_INVALID_ARG;
  h2_h2loader_host_command_t command =
      kind == GM_H2L_REBOOT_APP       ? H2_H2LOADER_HOST_COMMAND_REBOOT_APP
      : kind == GM_H2L_REBOOT_LOADER  ? H2_H2LOADER_HOST_COMMAND_REBOOT_LOADER
      : kind == GM_H2L_REBOOT_UPGRADE ? H2_H2LOADER_HOST_COMMAND_REBOOT_UPGRADE
                                      : (h2_h2loader_host_command_t)0;
  if (!command) return H2_PAL_ERR_INVALID_ARG;
  const h2_pal_time_api_t* clock = &ctx->time;
  ctx->cancel = cancel;
  ctx->cancel_user = cancel_user;
  h2_pal_result_t rc = connect_ctx(ctx);
  if (rc == H2_PAL_OK) rc = read_status(ctx);
  h2_h2loader_host_status_t before = ctx->status;
  if (rc == H2_PAL_OK) {
    h2_h2loader_host_command_request_t request = {
        .command = command,
        .status = &before,
        .is_cancelled = ctx_cancelled,
        .cancel_user = ctx,
        .on_output = forward_log,
        .output_user = ctx,
    };
    h2_h2loader_host_command_result_t result = {0};
    rc = guard_begin(ctx, 0u);
    if (rc == H2_PAL_OK)
      rc = h2_h2loader_host_serial_execute_command(ctx->connection, &request,
                                                   &result);
    rc = guard_finish(ctx, rc);
    if (rc == H2_PAL_OK &&
        result.terminal != H2_H2LOADER_HOST_COMMAND_TERMINAL_OK)
      rc = H2_PAL_ERR_INVALID_STATE;
  }
  if (rc != H2_PAL_OK) {
    disconnect(ctx);
    cancel_clear(ctx);
    return rc;
  }
  disconnect(ctx);
  int awaiting = 0;
  rc = H2_PAL_ERR_TIMEOUT;
  for (unsigned failed = 0u; failed < 120u && !ctx_cancelled(ctx);) {
    rc = guard_begin(ctx, 0u);
    if (rc != H2_PAL_OK) break;
    if (awaiting) {
      confirmation_wait_t wait = {.ctx = ctx};
      rc = h2_pal_time_get_monotonic_ms(clock, &wait.start_ms);
      if (rc != H2_PAL_OK) break;
      rc = h2_h2loader_host_serial_monitor_logs(ctx->connection,
                                                confirmation_wait_done, &wait);
      if (wait.control_result != H2_PAL_OK) {
        rc = wait.control_result;
        break;
      }
      if (rc == H2_PAL_EXIT && wait.deadline_reached)
        rc = read_status(ctx);
      else
        rc = H2_PAL_ERR_INVALID_STATE;
    } else {
      rc = h2_pal_time_sleep_ms(clock, 500u);
      rc = guard_finish(ctx, rc);
      if (rc != H2_PAL_OK) break;
      rc = connect_ctx(ctx);
      if (rc == H2_PAL_ERR_INVALID_STATE) break; /* identity changed */
    }
    if (rc == H2_PAL_OK) {
      rc = h2_h2loader_cli_verify_reboot_status(command, &before, &ctx->status);
      if (rc == H2_PAL_OK) break;
      if (h2_h2loader_cli_reboot_awaiting_confirmation(command, &before,
                                                       &ctx->status)) {
        awaiting = 1;
        continue;
      }
    }
    awaiting = 0;
    disconnect(ctx);
    ++failed;
  }
  if (ctx_cancelled(ctx)) rc = H2_PAL_EXIT;
  if (rc != H2_PAL_OK) disconnect(ctx);
  cancel_clear(ctx);
  return rc;
}
