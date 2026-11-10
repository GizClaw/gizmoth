#include <assert.h>
#include <errno.h>
#include <fcntl.h>
#include <libproc.h>
#include <stdarg.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/file.h>
#include <sys/ioctl.h>
#include <sys/wait.h>
#include <termios.h>
#include <time.h>
#include <unistd.h>

#include "gizmoth_serial_pal.h"

static int exclusive_fd = -1;
static unsigned acquires, releases, control_changes;
static int fail_exclusive;
static int owner_query_fault;

int gm_test_proc_listpidspath(uint32_t type, uint32_t typeinfo, const char* path,
                              uint32_t flags, void* buffer, int size) {
  assert(exclusive_fd >= 0 && fcntl(exclusive_fd, F_GETFD) >= 0);
  if (owner_query_fault == 1) {
    errno = EACCES;
    return -1;
  }
  if (owner_query_fault == 2) return size; /* Possibly truncated. */
  if (owner_query_fault == 3) return 0; /* Cannot see even our own fd. */
  return proc_listpidspath(type, typeinfo, path, flags, buffer, size);
}

/* Only the test transport's syscall symbols are renamed by its cc_library.
 * Successful requests still reach the kernel on the real owned descriptor. */
int gm_test_ioctl(int fd, unsigned long request, ...) {
  if (request == TIOCEXCL) {
    exclusive_fd = fd;
    if (fail_exclusive) {
      errno = ENOTTY;
      return -1;
    }
    int rc = ioctl(fd, request);
    if (!rc) ++acquires;
    return rc;
  }
  if (request == TIOCNXCL) {
    assert(fd == exclusive_fd);
    int rc = ioctl(fd, request);
    if (!rc) ++releases;
    return rc;
  }
  if (request == TIOCMBIC || request == TIOCMBIS) ++control_changes;
  va_list args;
  va_start(args, request);
  void* arg = va_arg(args, void*);
  va_end(args);
  return ioctl(fd, request, arg);
}

static const h2_pal_uart_io_stream_config_t config = {
    .baud_rate = 115200,
    .data_bits = 8,
    .stop_bits = 1,
    .parity = H2_PAL_UART_PARITY_NONE,
};

static int pty(char* path, size_t cap) {
  int master = posix_openpt(O_RDWR | O_NOCTTY | O_CLOEXEC);
  assert(master >= 0);
  assert(!grantpt(master));
  assert(!unlockpt(master));
  assert(ptsname(master));
  assert(strlen(ptsname(master)) < cap);
  strcpy(path, ptsname(master));
  return master;
}

static int ordinary_open(const char* path) {
  return open(path, O_RDWR | O_NOCTTY | O_NONBLOCK | O_CLOEXEC);
}

static void assert_released(const char* path) {
  int fd = ordinary_open(path);
  assert(fd >= 0); /* TIOCEXCL was cleared, including with a live PTY master. */
  assert(!flock(fd, LOCK_EX | LOCK_NB));
  assert(!close(fd));
}

static void test_exclusive_io_and_close(void) {
  char path[512];
  int master = pty(path, sizeof(path));
  int original_fd = ordinary_open(path);
  struct termios original;
  assert(original_fd >= 0);
  assert(!tcgetattr(original_fd, &original));
  assert(!close(original_fd));

  const h2_pal_serial_host_api_t* pal = gm_serial_pal();
  h2_pal_serial_host_session_t* session = NULL;
  unsigned before = acquires;
  assert(h2_pal_serial_host_open(pal, path, &config, &session) == H2_PAL_OK);
  assert(acquires == before + 1 && exclusive_fd >= 0);
  assert(fcntl(exclusive_fd, F_GETFD) & FD_CLOEXEC);
  assert(!control_changes);
  errno = 0;
  int ordinary = ordinary_open(path); /* No advisory lock cooperation. */
  if (ordinary < 0) {
    assert(errno == EBUSY);
    puts("PTY kernel rejects ordinary reopen while exclusive");
  } else {
    /* Darwin PTYs can accept TIOCEXCL without enforcing it. The observation
     * above still proves the production transport acquired it on its own fd. */
    puts("Platform limit: Darwin PTY accepts TIOCEXCL but allows reopen");
    assert(!close(ordinary));
  }
  h2_pal_serial_host_session_t* duplicate = NULL;
  assert(h2_pal_serial_host_open(pal, path, &config, &duplicate) ==
         H2_PAL_ERR_UNAVAILABLE);
  assert(!duplicate);

  const h2_pal_uart_io_stream_api_t* stream;
  assert(h2_pal_serial_host_session_stream(pal, session, &stream) == H2_PAL_OK);
  char buffer[32];
  size_t count;
  assert(h2_pal_uart_io_stream_read(stream, buffer, sizeof(buffer), &count,
                                    10) == H2_PAL_ERR_TIMEOUT);
  assert(!count);
  assert(write(master, "input", 5) == 5);
  assert(h2_pal_uart_io_stream_read(stream, buffer, sizeof(buffer), &count,
                                    100) == H2_PAL_OK);
  assert(count == 5 && !memcmp(buffer, "input", 5));
  assert(h2_pal_uart_io_stream_write(stream, "output", 6, &count, 100) ==
         H2_PAL_OK);
  assert(count == 6);
  assert(read(master, buffer, sizeof(buffer)) == 6);
  assert(!memcmp(buffer, "output", 6));
  assert(h2_pal_uart_io_stream_flush(stream) == H2_PAL_OK);

  unsigned released_before = releases;
  int owned_fd = exclusive_fd;
  assert(h2_pal_serial_host_close(pal, &session) == H2_PAL_OK);
  assert(releases == released_before + 1);
  assert(fcntl(owned_fd, F_GETFD) == -1 && errno == EBADF);
  assert(!session);
  assert(h2_pal_serial_host_close(pal, &session) == H2_PAL_OK);
  assert_released(path);
  int restored_fd = ordinary_open(path);
  struct termios restored;
  assert(!tcgetattr(restored_fd, &restored));
  assert(restored.c_cflag == original.c_cflag);
  assert(restored.c_iflag == original.c_iflag);
  assert(restored.c_lflag == original.c_lflag);
  assert(!close(restored_fd));
  assert(!close(master));
}

static void test_failed_configuration_releases_both_locks(void) {
  char path[512];
  int master = pty(path, sizeof(path));
  const h2_pal_serial_host_api_t* pal = gm_serial_pal();
  h2_pal_serial_host_session_t* session = NULL;
  h2_pal_uart_io_stream_config_t invalid = config;
  invalid.data_bits = 9;
  /* Exercise failure after acquisition; the public inline validator normally
   * rejects this before entering the transport. */
  assert(pal->vtable->open(pal->user, path, &invalid, &session) ==
         H2_PAL_ERR_INVALID_ARG);
  assert(!session);
  assert_released(path);
  assert(h2_pal_serial_host_open(pal, path, &config, &session) == H2_PAL_OK);
  assert(h2_pal_serial_host_close(pal, &session) == H2_PAL_OK);
  assert_released(path);
  assert(!close(master));
}

static void test_flock_failure_does_not_change_exclusivity(void) {
  char path[512];
  int master = pty(path, sizeof(path));
  int owner = ordinary_open(path);
  assert(owner >= 0 && !flock(owner, LOCK_EX | LOCK_NB));
  h2_pal_serial_host_session_t* session = NULL;
  assert(h2_pal_serial_host_open(gm_serial_pal(), path, &config, &session) ==
         H2_PAL_ERR_UNAVAILABLE);
  assert(!session);
  int observer = ordinary_open(path);
  assert(observer >= 0); /* A failed acquisition did not leave TIOCEXCL. */
  assert(!close(observer));
  assert(!close(owner));
  assert_released(path);
  assert(!close(master));
}

static void test_non_tty_fails_closed(void) {
  char path[] = "/tmp/gizmoth-pal-XXXXXX";
  int file = mkstemp(path);
  assert(file >= 0);
  h2_pal_serial_host_session_t* session = NULL;
  assert(h2_pal_serial_host_open(gm_serial_pal(), path, &config, &session) !=
         H2_PAL_OK);
  assert(!session);
  assert(!flock(file, LOCK_EX | LOCK_NB));
  assert(!close(file));
  assert(!unlink(path));
}

static void test_exclusive_ioctl_failure_is_fatal(void) {
  char path[512];
  int master = pty(path, sizeof(path));
  h2_pal_serial_host_session_t* session = NULL;
  fail_exclusive = 1;
  unsigned before = acquires;
  assert(h2_pal_serial_host_open(gm_serial_pal(), path, &config, &session) !=
         H2_PAL_OK);
  assert(!session && acquires == before);
  assert(fcntl(exclusive_fd, F_GETFD) == -1 && errno == EBADF);
  fail_exclusive = 0;
  assert_released(path);
  assert(!close(master));
}

static uint64_t monotonic_ms(void) {
  struct timespec now;
  assert(!clock_gettime(CLOCK_MONOTONIC, &now));
  return (uint64_t)now.tv_sec * 1000u + (uint64_t)now.tv_nsec / 1000000u;
}
static h2_pal_result_t interruptible_now(void* user, uint64_t* out) {
  *out = monotonic_ms();
  return *out >= *(uint64_t*)user ? H2_PAL_EXIT : H2_PAL_OK;
}

static void test_guard_interrupts_blocked_io(void) {
  char path[512];
  int master = pty(path, sizeof(path));
  uint64_t stop_at = UINT64_MAX;
  const h2_pal_time_vtable_t clock_vtable = {.get_monotonic_ms =
                                                 interruptible_now};
  const h2_pal_time_api_t clock = {.user = &stop_at, .vtable = &clock_vtable};
  h2_pal_serial_host_api_t pal;
  gm_serial_pal_init(&pal, &clock);
  h2_pal_serial_host_session_t* session = NULL;
  assert(h2_pal_serial_host_open(&pal, path, &config, &session) == H2_PAL_OK);
  const h2_pal_uart_io_stream_api_t* stream;
  assert(h2_pal_serial_host_session_stream(&pal, session, &stream) ==
         H2_PAL_OK);
  char byte;
  size_t count;
  uint64_t start = monotonic_ms();
  stop_at = start + 50;
  assert(h2_pal_uart_io_stream_read(stream, &byte, 1, &count, 120000) ==
         H2_PAL_EXIT);
  assert(!count && monotonic_ms() - start < 1000);
  char* full = calloc(1, 1024 * 1024);
  assert(full);
  start = monotonic_ms();
  stop_at = start + 50;
  assert(h2_pal_uart_io_stream_write(stream, full, 1024 * 1024, &count,
                                     120000) == H2_PAL_EXIT);
  assert(count < 1024 * 1024 && monotonic_ms() - start < 1000);
  assert(h2_pal_uart_io_stream_flush(stream) == H2_PAL_EXIT);
  free(full);
  assert(h2_pal_serial_host_close(&pal, &session) == H2_PAL_OK);
  assert_released(path);
  assert(!close(master));
}

static void test_non_cooperating_process_is_rejected(void) {
  char path[512];
  int master = pty(path, sizeof(path));
  int initial_fd = ordinary_open(path);
  struct termios original, after;
  assert(initial_fd >= 0 && !tcgetattr(initial_fd, &original));
  assert(!close(initial_fd));
  int ready[2], done[2];
  assert(!pipe(ready) && !pipe(done));
  pid_t owner = fork();
  assert(owner >= 0);
  if (!owner) {
    close(master);
    close(ready[0]);
    close(done[1]);
    int fd = ordinary_open(path); /* Intentionally no flock or TIOCEXCL. */
    char byte = 'r';
    if (fd < 0 || write(ready[1], &byte, 1) != 1 ||
        read(done[0], &byte, 1) != 1)
      _exit(1);
    close(fd);
    _exit(0);
  }
  assert(!close(ready[1]) && !close(done[0]));
  char byte;
  assert(read(ready[0], &byte, 1) == 1);
  h2_pal_serial_host_session_t* session = NULL;
  unsigned before = acquires, released_before = releases;
  assert(h2_pal_serial_host_open(gm_serial_pal(), path, &config, &session) ==
         H2_PAL_ERR_UNAVAILABLE);
  assert(!session && acquires == before + 1 && releases == released_before + 1);
  assert(fcntl(exclusive_fd, F_GETFD) == -1 && errno == EBADF);
  int observer = ordinary_open(path);
  assert(observer >= 0 && !tcgetattr(observer, &after));
  assert(original.c_cflag == after.c_cflag);
  assert(original.c_iflag == after.c_iflag);
  assert(original.c_lflag == after.c_lflag);
  assert(!close(observer));
  assert(write(done[1], "d", 1) == 1);
  int status;
  assert(waitpid(owner, &status, 0) == owner);
  assert(WIFEXITED(status) && WEXITSTATUS(status) == 0);
  assert(!close(ready[0]) && !close(done[1]));
  assert_released(path);
  assert(h2_pal_serial_host_open(gm_serial_pal(), path, &config, &session) ==
         H2_PAL_OK);
  assert(h2_pal_serial_host_close(gm_serial_pal(), &session) == H2_PAL_OK);
  assert(!close(master));
}

static void test_owner_query_fails_closed(int fault) {
  char path[512];
  int master = pty(path, sizeof(path));
  h2_pal_serial_host_session_t* session = NULL;
  owner_query_fault = fault;
  unsigned released_before = releases;
  assert(h2_pal_serial_host_open(gm_serial_pal(), path, &config, &session) ==
         H2_PAL_ERR_UNAVAILABLE);
  assert(!session && releases == released_before + 1);
  assert(fcntl(exclusive_fd, F_GETFD) == -1 && errno == EBADF);
  owner_query_fault = 0;
  assert_released(path);
  assert(!close(master));
}

int main(void) {
  test_exclusive_io_and_close();
  test_guard_interrupts_blocked_io();
  test_exclusive_ioctl_failure_is_fatal();
  test_failed_configuration_releases_both_locks();
  test_flock_failure_does_not_change_exclusivity();
  test_non_tty_fails_closed();
  test_non_cooperating_process_is_rejected();
  test_owner_query_fails_closed(1);
  test_owner_query_fails_closed(2);
  test_owner_query_fails_closed(3);
  puts("macOS exclusive serial PAL: 10 synthetic PTY/file tests passed");
  return 0;
}
