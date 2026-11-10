/* Exercise the public FFI through the real pinned Host Core, with synthetic
 * serial endpoints, a KCP device peer and a deterministic clock. No device I/O.
 */
#include <assert.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include "gizmoth_serial_core.h"
#include "gizmoth_serial_pal.h"
#include "h2_h2loader_cli_target.h"
#include "h2_h2loader_host.h"
#include "h2_iostreamikcp.h"

static struct {
  uint64_t now;
  int admit;
  int acknowledge;
  int respond;
  int opens;
  int closes;
  int status_commands;
  int reboot_commands;
  uint32_t reboot_delay_ms;
} device;

typedef struct cancellation {
  uint64_t at;
  int borrowed;
  unsigned calls;
  int status_command;
} cancellation_t;

struct h2_pal_serial_host_session {
  h2_pal_uart_io_stream_api_t uart;
  h2_iostreamikcp_filter_t filter;
  h2_iostreamikcp_t* peer;
  uint8_t incoming[32768];
  size_t incoming_len;
  uint64_t reboot_due;
};

static void* mem_alloc(void* user, size_t size) {
  (void)user;
  return malloc(size);
}
static void* mem_realloc(void* user, void* ptr, size_t size) {
  (void)user;
  return realloc(ptr, size);
}
static void mem_free(void* user, void* ptr) {
  (void)user;
  free(ptr);
}
static const h2_pal_mem_vtable_t mem_vtable = {
    .alloc = mem_alloc, .realloc = mem_realloc, .free = mem_free};
static const h2_pal_mem_api_t mem_api = {.vtable = &mem_vtable};

static h2_pal_result_t fake_now(void* user, uint64_t* out) {
  (void)user;
  *out = device.now;
  return H2_PAL_OK;
}
static h2_pal_result_t fake_sleep(void* user, uint32_t ms) {
  (void)user;
  device.now += ms;
  return H2_PAL_OK;
}
static const h2_pal_time_vtable_t time_vtable = {.get_monotonic_ms = fake_now,
                                                 .sleep_ms = fake_sleep};
static const h2_pal_time_api_t time_api = {.vtable = &time_vtable};

/* Only platform selection is replaced. Protocol and all serial wait loops are
 * the production implementations from the pinned Bazel dependency. */
h2_pal_result_t h2_h2loader_cli_target_start(void) { return H2_PAL_OK; }
const h2_pal_mem_api_t* h2_h2loader_cli_target_mem(void) { return &mem_api; }
const h2_pal_time_api_t* h2_h2loader_cli_target_time(void) { return &time_api; }

/* These verification functions otherwise pull the whole CLI application object
 * and its platform entry points into the fixture. No test bypasses status I/O.
 */
h2_pal_result_t h2_h2loader_cli_verify_reboot_status(
    h2_h2loader_host_command_t command, const h2_h2loader_host_status_t* before,
    const h2_h2loader_host_status_t* after) {
  (void)command;
  (void)before;
  (void)after;
  return H2_PAL_OK;
}
int h2_h2loader_cli_reboot_awaiting_confirmation(
    h2_h2loader_host_command_t command, const h2_h2loader_host_status_t* before,
    const h2_h2loader_host_status_t* after) {
  (void)command;
  (void)before;
  (void)after;
  return 0;
}

static void queue(h2_pal_serial_host_session_t* s, const void* data,
                  size_t len) {
  assert(s->incoming_len + len <= sizeof(s->incoming));
  memcpy(s->incoming + s->incoming_len, data, len);
  s->incoming_len += len;
}

static h2_pal_result_t no_input(void* user, void* data, size_t len, size_t* out,
                                uint32_t timeout) {
  (void)user;
  (void)data;
  (void)len;
  (void)timeout;
  *out = 0;
  return H2_PAL_ERR_WOULD_BLOCK;
}
static h2_pal_result_t peer_output(void* user, const void* data, size_t len,
                                   size_t* out, uint32_t timeout) {
  (void)timeout;
  if (device.acknowledge) queue(user, data, len);
  *out = len;
  return H2_PAL_OK;
}
static h2_pal_result_t flush(void* user) {
  (void)user;
  return H2_PAL_OK;
}
static uint32_t peer_now(void* user) {
  (void)user;
  return (uint32_t)device.now;
}

static h2_pal_result_t frame_received(void* user,
                                      const h2_iostreamikcp_frame_t* frame) {
  h2_pal_serial_host_session_t* s = user;
  if (frame->flags == H2_IOSTREAMIKCP_FRAME_FLAG_SESSION_OPEN ||
      frame->flags == H2_IOSTREAMIKCP_FRAME_FLAG_SESSION_CLOSE) {
    if (!device.admit) return H2_PAL_OK;
    uint8_t encoded[64];
    size_t len;
    h2_iostreamikcp_frame_t ack = *frame;
    ack.flags = H2_IOSTREAMIKCP_FRAME_FLAG_SESSION_ACK;
    assert(h2_iostreamikcp_frame_encode(&ack, encoded, sizeof(encoded), &len) ==
           H2_PAL_OK);
    queue(s, encoded, len);
    if (!s->peer && frame->flags == H2_IOSTREAMIKCP_FRAME_FLAG_SESSION_OPEN) {
      h2_iostreamikcp_config_t config = {
          .io = {.user = s,
                 .read = no_input,
                 .write = peer_output,
                 .flush = flush},
          .allocator = &mem_api,
          .now_ms = peer_now,
          .conv = frame->conv,
          .rx_buffer_size = 8192,
          .write_timeout_ms = 120000,
      };
      assert(h2_iostreamikcp_open(&config, &s->peer) == H2_PAL_OK);
    }
    return H2_PAL_OK;
  }
  assert(s->peer);
  return h2_iostreamikcp_input_frame(s->peer, frame);
}

static const char status_line[] =
    "H2_LOADER_STATUS board=devkit target=esp32s3 chip=esp32s3 "
    "device_uid=102030405060 capabilities=0x00000001 "
    "command_availability=0x0008080b active_role=app active_version=v1 "
    "active_checksum="
    "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa "
    "active_image_size=4096 running_partition=2 next_partition=2 "
    "boot_intent=auto "
    "stage_valid=0 stage_package_checksum=- stage_package_size=0 "
    "stage_image_checksum=- stage_image_size=0 stage_role=unknown "
    "stage_version=- stage_board=- stage_target=- "
    "partition_1_valid=0 partition_1_package_checksum=- "
    "partition_1_package_size=0 "
    "partition_1_image_checksum=- partition_1_image_size=0 "
    "partition_1_role=unknown "
    "partition_1_version=- partition_1_board=- partition_1_target=- "
    "partition_2_valid=0 partition_2_package_checksum=- "
    "partition_2_package_size=0 "
    "partition_2_image_checksum=- partition_2_image_size=0 "
    "partition_2_role=unknown "
    "partition_2_version=- partition_2_board=- partition_2_target=- "
    "last_result=0 mfg_mode=1 mfg_steps=0000000000000000000000\n";

static void pump_peer(h2_pal_serial_host_session_t* s) {
  if (!s->peer) return;
  assert(h2_iostreamikcp_update(s->peer, (uint32_t)device.now) == H2_PAL_OK);
  for (;;) {
    uint8_t command[1024];
    size_t len;
    h2_pal_result_t rc =
        h2_iostreamikcp_read(s->peer, command, sizeof(command), &len);
    if (rc == H2_PAL_ERR_WOULD_BLOCK) break;
    assert(rc == H2_PAL_OK && len);
    if (len == strlen("h2loader status\n") &&
        !memcmp(command, "h2loader status\n", len)) {
      ++device.status_commands;
      if (device.respond)
        assert(h2_iostreamikcp_write(s->peer, (const uint8_t*)status_line,
                                     strlen(status_line)) == H2_PAL_OK);
    } else {
      assert(len == strlen("h2loader reboot loader\n"));
      assert(!memcmp(command, "h2loader reboot loader\n", len));
      ++device.reboot_commands;
      s->reboot_due = device.now + device.reboot_delay_ms;
    }
  }
  if (s->reboot_due && device.now >= s->reboot_due) {
    static const char accepted[] =
        "H2_LOADER_REBOOT target=loader result=accepted\n";
    s->reboot_due = 0;
    assert(h2_iostreamikcp_write(s->peer, (const uint8_t*)accepted,
                                 strlen(accepted)) == H2_PAL_OK);
  }
  assert(h2_iostreamikcp_update(s->peer, (uint32_t)device.now) == H2_PAL_OK);
}

static h2_pal_result_t uart_read(void* user, void* data, size_t len,
                                 size_t* out, uint32_t timeout) {
  h2_pal_serial_host_session_t* s = user;
  device.now += timeout;
  pump_peer(s);
  *out = s->incoming_len < len ? s->incoming_len : len;
  memcpy(data, s->incoming, *out);
  s->incoming_len -= *out;
  memmove(s->incoming, s->incoming + *out, s->incoming_len);
  return *out ? H2_PAL_OK : H2_PAL_ERR_TIMEOUT;
}
static h2_pal_result_t uart_write(void* user, const void* data, size_t len,
                                  size_t* out, uint32_t timeout) {
  (void)timeout;
  h2_pal_serial_host_session_t* s = user;
  assert(h2_iostreamikcp_filter_input(&s->filter, data, len, frame_received,
                                      s) == H2_PAL_OK);
  pump_peer(s);
  *out = len;
  return H2_PAL_OK;
}
static const h2_pal_uart_io_stream_vtable_t uart_vtable = {
    .read = uart_read, .write = uart_write, .flush = flush};

static h2_pal_result_t pal_open(void* user, const char* port,
                                const h2_pal_uart_io_stream_config_t* config,
                                h2_pal_serial_host_session_t** out) {
  (void)user;
  (void)port;
  assert(config->baud_rate == H2_H2LOADER_HOST_RELIABLE_SERIAL_BAUD);
  *out = calloc(1, sizeof(**out));
  assert(*out);
  (*out)->uart.user = *out;
  (*out)->uart.vtable = &uart_vtable;
  h2_iostreamikcp_filter_init(&(*out)->filter);
  ++device.opens;
  return H2_PAL_OK;
}
static h2_pal_result_t pal_stream(void* user, h2_pal_serial_host_session_t* s,
                                  const h2_pal_uart_io_stream_api_t** out) {
  (void)user;
  *out = &s->uart;
  return H2_PAL_OK;
}
static h2_pal_result_t pal_close(void* user,
                                 h2_pal_serial_host_session_t** inout) {
  (void)user;
  if (!*inout) return H2_PAL_OK;
  h2_iostreamikcp_close((*inout)->peer);
  free(*inout);
  *inout = NULL;
  ++device.closes;
  return H2_PAL_OK;
}
static const h2_pal_serial_host_vtable_t pal_vtable = {
    .open = pal_open, .session_stream = pal_stream, .close = pal_close};
static const h2_pal_serial_host_api_t pal_api = {.vtable = &pal_vtable};
const h2_pal_serial_host_api_t* gm_serial_pal(void) { return &pal_api; }
void gm_serial_pal_init(h2_pal_serial_host_api_t* out,
                        const h2_pal_time_api_t* time) {
  (void)time;
  *out = pal_api;
}

static int cancelled(void* user) {
  cancellation_t* cancel = user;
  assert(cancel->borrowed); /* No stored callback dereference after return. */
  ++cancel->calls;
  return device.now >= cancel->at ||
         (cancel->status_command > 0 &&
          device.status_commands >= cancel->status_command);
}
static void log_bytes(void* user, const uint8_t* data, size_t len) {
  (void)user;
  (void)data;
  (void)len;
}
static cancellation_t borrow(uint64_t at) {
  cancellation_t result = {.at = at, .borrowed = 1};
  return result;
}
static void reset(void) {
  assert(device.opens == device.closes);
  memset(&device, 0, sizeof(device));
  device.now = 1000;
  device.admit = device.acknowledge = device.respond = 1;
}
static gm_h2l_t* open_session(void) {
  cancellation_t cancel = borrow(UINT64_MAX);
  gm_h2l_t* ctx = NULL;
  int rc = gm_h2l_open("fake", 8000, log_bytes, NULL, cancelled, &cancel, &ctx);
  if (rc != H2_PAL_OK)
    fprintf(stderr, "fake open: %s time=%llu status_commands=%d\n",
            gm_h2l_result_name(rc), (unsigned long long)device.now,
            device.status_commands);
  assert(rc == H2_PAL_OK);
  assert(ctx && cancel.calls);
  cancel.borrowed = 0;
  return ctx;
}

static void test_cancel_during_admission(void) {
  reset();
  device.admit = 0;
  cancellation_t cancel = borrow(device.now + 100);
  gm_h2l_t* ctx = NULL;
  uint64_t start = device.now;
  assert(gm_h2l_open("fake", 8000, log_bytes, NULL, cancelled, &cancel, &ctx) ==
         H2_PAL_EXIT);
  assert(!ctx && device.now - start <= 110);
  assert(device.opens == 1 && device.closes == 1);
}

static void test_initial_status_bounds(int acknowledge, int cancel_early) {
  reset();
  device.respond = 0;
  device.acknowledge = acknowledge;
  cancellation_t cancel = borrow(cancel_early ? device.now + 100 : UINT64_MAX);
  gm_h2l_t* ctx = NULL;
  uint64_t start = device.now;
  int rc = gm_h2l_open("fake", 8000, log_bytes, NULL, cancelled, &cancel, &ctx);
  assert(rc == (cancel_early ? H2_PAL_EXIT : H2_PAL_ERR_TIMEOUT));
  assert(!ctx && device.status_commands);
  assert(device.now - start < (cancel_early ? 1000u : 5000u));
  assert(device.opens == 1 && device.closes == 1);
}

static void test_refresh_cancel_reconnect_and_callback_lifetime(void) {
  reset();
  gm_h2l_t* ctx = open_session();
  device.respond = 0;
  cancellation_t cancel = borrow(device.now + 100);
  char json[8192];
  uint64_t start = device.now;
  assert(gm_h2l_status_json(ctx, json, sizeof(json), cancelled, &cancel) ==
         H2_PAL_EXIT);
  assert(device.now - start < 1000 && device.opens == device.closes);
  cancel.borrowed = 0;
  device.respond = 1;
  cancellation_t next = borrow(UINT64_MAX);
  assert(gm_h2l_status_json(ctx, json, sizeof(json), cancelled, &next) ==
         H2_PAL_OK);
  assert(strstr(json, "\"deviceUid\":\"102030405060\""));
  next.borrowed = 0;
  gm_h2l_close(&ctx); /* Teardown must not call either expired callback. */
  assert(!ctx && device.opens == device.closes);
}

static void test_stage_and_reboot_preflight_cancel(void) {
  reset();
  gm_h2l_t* ctx = open_session();
  device.respond = 0;
  cancellation_t cancel = borrow(device.now + 100);
  assert(gm_h2l_stage(ctx, "/no-file-needed-before-preflight", cancelled,
                      &cancel, NULL, NULL) == H2_PAL_EXIT);
  assert(device.opens == device.closes);
  cancel.borrowed = 0;
  gm_h2l_close(&ctx);

  reset();
  ctx = open_session();
  device.respond = 0;
  cancellation_t reboot_cancel = borrow(device.now + 100);
  assert(gm_h2l_reboot(ctx, GM_H2L_REBOOT_LOADER, cancelled, &reboot_cancel) ==
         H2_PAL_EXIT);
  reboot_cancel.borrowed = 0;
  gm_h2l_close(&ctx);
  assert(device.opens == device.closes);
}

static void test_context_guards_are_independent(void) {
  reset();
  gm_h2l_t* a = open_session();
  gm_h2l_t* b = open_session();
  char json[8192];
  cancellation_t stop_a = borrow(device.now);
  assert(gm_h2l_status_json(a, json, sizeof(json), cancelled, &stop_a) ==
         H2_PAL_EXIT);
  stop_a.borrowed = 0;
  cancellation_t keep_b = borrow(UINT64_MAX);
  assert(gm_h2l_status_json(b, json, sizeof(json), cancelled, &keep_b) ==
         H2_PAL_OK);
  keep_b.borrowed = 0;
  gm_h2l_close(&a);
  gm_h2l_close(&b);
  assert(device.opens == device.closes);
}

static void test_long_command_keeps_transfer_deadline(void) {
  reset();
  gm_h2l_t* ctx = open_session();
  device.reboot_delay_ms = 6000; /* Exceeds the 4s status guard. */
  cancellation_t cancel = borrow(UINT64_MAX);
  uint64_t start = device.now;
  assert(gm_h2l_reboot(ctx, GM_H2L_REBOOT_LOADER, cancelled, &cancel) ==
         H2_PAL_OK);
  assert(device.reboot_commands == 1 && device.now - start >= 6000);
  cancel.borrowed = 0;
  gm_h2l_close(&ctx);
  assert(device.opens == device.closes);
}

static void test_reboot_verification_status_is_cancellable(void) {
  reset();
  gm_h2l_t* ctx = open_session();
  cancellation_t cancel = borrow(UINT64_MAX);
  cancel.status_command =
      device.status_commands + 2; /* Preflight, then verify. */
  assert(gm_h2l_reboot(ctx, GM_H2L_REBOOT_LOADER, cancelled, &cancel) ==
         H2_PAL_EXIT);
  assert(device.reboot_commands == 1);
  assert(device.status_commands == cancel.status_command);
  cancel.borrowed = 0;
  gm_h2l_close(&ctx);
  assert(device.opens == device.closes);
}

int main(void) {
  assert(gm_h2l_abi_version() == 2);
  test_cancel_during_admission();
  test_initial_status_bounds(0, 0); /* serial_wait_delivery */
  test_initial_status_bounds(1, 0); /* serial_read_until */
  test_initial_status_bounds(0, 1);
  test_initial_status_bounds(1, 1);
  test_refresh_cancel_reconnect_and_callback_lifetime();
  test_stage_and_reboot_preflight_cancel();
  test_context_guards_are_independent();
  test_long_command_keeps_transfer_deadline();
  test_reboot_verification_status_is_cancellable();
  puts("FFI status guards: 10 fake PAL/clock/device scenarios passed");
  return 0;
}
