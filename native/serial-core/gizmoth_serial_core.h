/* GizMoth serial daemon <-> GizOS Host Core boundary.
 *
 * One context owns one H2Loader serial connection. A context is used from a
 * single worker thread; different contexts may run concurrently on different
 * ports. Only gm_h2l_global_start() touches process-wide state. */
#ifndef GIZMOTH_SERIAL_CORE_H
#define GIZMOTH_SERIAL_CORE_H

#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

#define GM_H2L_ABI_VERSION 2

typedef struct gm_h2l gm_h2l_t;

/* Device bytes proven not to belong to a protocol frame (text logs). */
typedef void (*gm_h2l_log_fn)(void* user, const uint8_t* data, size_t len);
/* Nonzero requests cooperative cancellation at the next I/O boundary. */
typedef int (*gm_h2l_cancel_fn)(void* user);
typedef void (*gm_h2l_progress_fn)(void* user, uint64_t acknowledged,
                                   uint64_t total);

typedef enum gm_h2l_reboot {
  GM_H2L_REBOOT_APP = 0,
  GM_H2L_REBOOT_LOADER = 1,
  GM_H2L_REBOOT_UPGRADE = 2,
} gm_h2l_reboot_t;

/* Results are h2_pal_result_t values; 0 is success. */
int gm_h2l_abi_version(void);
int gm_h2l_global_start(void);
const char* gm_h2l_result_name(int result);

/* Cancellation callbacks are borrowed only for the duration of each call.
 * Admission keeps handshake_timeout_ms; every status read has a 4s deadline. */
int gm_h2l_open(const char* port, uint32_t handshake_timeout_ms,
                gm_h2l_log_fn on_log, void* log_user, gm_h2l_cancel_fn cancel,
                void* cancel_user, gm_h2l_t** out);
void gm_h2l_close(gm_h2l_t** inout);

/* Refreshes authoritative status and writes it as JSON. Returns
 * H2_PAL_ERR_NO_MEM style failure when out_cap is too small. */
int gm_h2l_status_json(gm_h2l_t* ctx, char* out, size_t out_cap,
                       gm_h2l_cancel_fn cancel, void* cancel_user);
/* Passively forwards device logs until cancel() returns nonzero. */
int gm_h2l_monitor(gm_h2l_t* ctx, gm_h2l_cancel_fn cancel, void* cancel_user);
/* Validates a package on disk without a device; writes metadata JSON. */
int gm_h2l_inspect_json(const char* path, char* out, size_t out_cap);
/* Validates, checks board/target, transfers and verifies the durable Stage. */
int gm_h2l_stage(gm_h2l_t* ctx, const char* path, gm_h2l_cancel_fn cancel,
                 void* cancel_user, gm_h2l_progress_fn progress,
                 void* progress_user);
/* Issues a reboot command, reconnects and verifies the resulting state. */
int gm_h2l_reboot(gm_h2l_t* ctx, gm_h2l_reboot_t kind, gm_h2l_cancel_fn cancel,
                  void* cancel_user);

#ifdef __cplusplus
}
#endif

#endif
