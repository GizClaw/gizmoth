/* Downstream transport only; framing, KCP and H2Loader stay in GizOS Host Core.
 * No upstream session is opened or inspected here. */
#include "gizmoth_serial_pal.h"

#include "h2_h2loader_cli_target.h"

#if defined(__APPLE__)
#include <IOKit/serial/ioss.h>
#include <errno.h>
#include <fcntl.h>
#include <limits.h>
#include <libproc.h>
#include <poll.h>
#include <pthread.h>
#include <stdlib.h>
#include <string.h>
#include <sys/file.h>
#include <sys/ioctl.h>
#include <sys/stat.h>
#include <termios.h>
#include <time.h>
#include <unistd.h>

/* This is our own implementation of the public opaque PAL handle. It is never
 * passed to the upstream transport; only enumeration snapshots are delegated.
 */
struct h2_pal_serial_host_session {
  int fd;
  pthread_mutex_t mutex;
  struct termios original;
  const h2_pal_time_api_t* time;
  h2_pal_uart_io_stream_api_t stream;
};

static h2_pal_result_t io_error(int error) {
  switch (error) {
    case EAGAIN:
      return H2_PAL_ERR_WOULD_BLOCK;
    case ETIMEDOUT:
      return H2_PAL_ERR_TIMEOUT;
    case EBADF:
    case EIO:
    case ENODEV:
    case ENXIO:
    case EPIPE:
      return H2_PAL_ERR_CLOSED;
    default:
      return H2_PAL_ERR_IO;
  }
}

static h2_pal_result_t open_error(int error) {
  switch (error) {
    case ENOENT:
    case ENODEV:
    case ENXIO:
      return H2_PAL_ERR_NOT_FOUND;
    case EACCES:
    case EPERM:
    case EBUSY:
    case EAGAIN:
      return H2_PAL_ERR_UNAVAILABLE;
    default:
      return H2_PAL_ERR_IO;
  }
}

static h2_pal_result_t now_ms(uint64_t* out) {
  struct timespec value;
  if (clock_gettime(CLOCK_MONOTONIC, &value)) return H2_PAL_ERR_IO;
  *out = (uint64_t)value.tv_sec * 1000u + (uint64_t)value.tv_nsec / 1000000u;
  return H2_PAL_OK;
}

static h2_pal_result_t session_now(h2_pal_serial_host_session_t* session,
                                   uint64_t* out) {
  return session->time ? h2_pal_time_get_monotonic_ms(session->time, out)
                       : now_ms(out);
}

static h2_pal_result_t wait_fd(h2_pal_serial_host_session_t* session,
                               short events, uint64_t deadline) {
  for (;;) {
    uint64_t now;
    h2_pal_result_t rc = session_now(session, &now);
    if (rc != H2_PAL_OK) return rc;
    uint64_t remaining = now < deadline ? deadline - now : 0u;
    /* Keep checking the guard even if a write was given a long flash timeout.
     */
    int timeout = remaining > 10u ? 10 : (int)remaining;
    struct pollfd poll_fd = {.fd = session->fd, .events = events};
    int ready = poll(&poll_fd, 1, timeout);
    if (ready < 0 && errno == EINTR) {
      if (now >= deadline) return H2_PAL_ERR_TIMEOUT;
      continue;
    }
    if (ready < 0) return io_error(errno);
    if (!ready) {
      if (now >= deadline) return H2_PAL_ERR_TIMEOUT;
      continue;
    }
    if (poll_fd.revents & (POLLERR | POLLHUP | POLLNVAL))
      return H2_PAL_ERR_CLOSED;
    if (poll_fd.revents & events) return H2_PAL_OK;
  }
}

static speed_t standard_speed(uint32_t baud) {
  switch (baud) {
    case 9600u:
      return B9600;
    case 19200u:
      return B19200;
    case 38400u:
      return B38400;
    case 57600u:
      return B57600;
    case 115200u:
      return B115200;
    case 230400u:
      return B230400;
    default:
      return 0;
  }
}

static h2_pal_result_t configure_locked(
    h2_pal_serial_host_session_t* session,
    const h2_pal_uart_io_stream_config_t* config) {
  if (!config || !config->baud_rate || config->data_bits < 5u ||
      config->data_bits > 8u ||
      (config->stop_bits != 1u && config->stop_bits != 2u) ||
      (config->parity != H2_PAL_UART_PARITY_NONE &&
       config->parity != H2_PAL_UART_PARITY_EVEN &&
       config->parity != H2_PAL_UART_PARITY_ODD) ||
      (config->flow_control & ~H2_PAL_UART_FLOW_CONTROL_RTS_CTS))
    return H2_PAL_ERR_INVALID_ARG;
  struct termios before, attributes;
  if (tcgetattr(session->fd, &before)) return io_error(errno);
  attributes = before;
  cfmakeraw(&attributes);
  attributes.c_cflag |= CLOCAL | CREAD;
  attributes.c_cflag &=
      ~(CSIZE | PARENB | PARODD | CSTOPB | CCTS_OFLOW | CRTS_IFLOW);
  const tcflag_t bits[] = {CS5, CS6, CS7, CS8};
  attributes.c_cflag |= bits[config->data_bits - 5u];
  if (config->stop_bits == 2u) attributes.c_cflag |= CSTOPB;
  if (config->parity != H2_PAL_UART_PARITY_NONE) attributes.c_cflag |= PARENB;
  if (config->parity == H2_PAL_UART_PARITY_ODD) attributes.c_cflag |= PARODD;
  if (config->flow_control & H2_PAL_UART_FLOW_CONTROL_CTS)
    attributes.c_cflag |= CCTS_OFLOW;
  if (config->flow_control & H2_PAL_UART_FLOW_CONTROL_RTS)
    attributes.c_cflag |= CRTS_IFLOW;
  attributes.c_cc[VMIN] = 0;
  attributes.c_cc[VTIME] = 0;
  speed_t speed = standard_speed(config->baud_rate);
  if (cfsetispeed(&attributes, speed ? speed : B9600) ||
      cfsetospeed(&attributes, speed ? speed : B9600))
    return io_error(errno);
  /* No flush or DTR/RTS ioctl: preserve input and the existing control lines.
   */
  if (tcsetattr(session->fd, TCSANOW, &attributes)) return io_error(errno);
  if (!speed) {
    speed_t custom = (speed_t)config->baud_rate;
    if (ioctl(session->fd, IOSSIOSPEED, &custom)) {
      int error = errno;
      (void)tcsetattr(session->fd, TCSANOW, &before);
      return error == EINVAL || error == ENOTTY ? H2_PAL_ERR_UNSUPPORTED
                                                : io_error(error);
    }
  }
  return H2_PAL_OK;
}

static h2_pal_result_t stream_configure(
    void* user, const h2_pal_uart_io_stream_config_t* config) {
  h2_pal_serial_host_session_t* session = user;
  pthread_mutex_lock(&session->mutex);
  h2_pal_result_t rc = configure_locked(session, config);
  pthread_mutex_unlock(&session->mutex);
  return rc;
}

static h2_pal_result_t stream_read(void* user, void* data, size_t len,
                                   size_t* out_read, uint32_t timeout_ms) {
  h2_pal_serial_host_session_t* session = user;
  *out_read = 0;
  if (!len) return H2_PAL_OK;
  pthread_mutex_lock(&session->mutex);
  uint64_t start;
  h2_pal_result_t rc = session_now(session, &start);
  if (rc == H2_PAL_OK) rc = wait_fd(session, POLLIN, start + timeout_ms);
  if (rc == H2_PAL_OK) {
    ssize_t count = read(session->fd, data, len);
    if (count > 0)
      *out_read = (size_t)count;
    else
      rc = count == 0       ? H2_PAL_ERR_CLOSED
           : errno == EINTR ? H2_PAL_ERR_WOULD_BLOCK
                            : io_error(errno);
  }
  pthread_mutex_unlock(&session->mutex);
  return rc;
}

static h2_pal_result_t stream_write(void* user, const void* data, size_t len,
                                    size_t* out_written, uint32_t timeout_ms) {
  h2_pal_serial_host_session_t* session = user;
  *out_written = 0;
  if (!len) return H2_PAL_OK;
  pthread_mutex_lock(&session->mutex);
  uint64_t start;
  h2_pal_result_t rc = session_now(session, &start);
  while (rc == H2_PAL_OK && *out_written < len) {
    rc = wait_fd(session, POLLOUT, start + timeout_ms);
    if (rc != H2_PAL_OK) break;
    ssize_t count = write(session->fd, (const uint8_t*)data + *out_written,
                          len - *out_written);
    if (count > 0)
      *out_written += (size_t)count;
    else if (count == 0)
      rc = H2_PAL_ERR_CLOSED;
    else if (errno != EINTR && errno != EAGAIN)
      rc = io_error(errno);
  }
  pthread_mutex_unlock(&session->mutex);
  return rc;
}

static h2_pal_result_t stream_flush(void* user) {
  h2_pal_serial_host_session_t* session = user;
  pthread_mutex_lock(&session->mutex);
  uint64_t start, now;
  h2_pal_result_t rc = session_now(session, &start);
  while (rc == H2_PAL_OK) {
    int pending = 0;
    if (ioctl(session->fd, TIOCOUTQ, &pending)) {
      rc = errno == ENOTTY ? H2_PAL_ERR_UNSUPPORTED : io_error(errno);
      break;
    }
    if (!pending) break;
    rc = session_now(session, &now);
    if (rc != H2_PAL_OK) break;
    if (now - start >= H2_PAL_SERIAL_HOST_FLUSH_TIMEOUT_MS) {
      rc = H2_PAL_ERR_TIMEOUT;
      break;
    }
    const struct timespec delay = {.tv_nsec = 1000000};
    (void)nanosleep(&delay, NULL);
  }
  pthread_mutex_unlock(&session->mutex);
  return rc;
}

static const h2_pal_uart_io_stream_vtable_t stream_vtable = {
    .configure = stream_configure,
    .read = stream_read,
    .write = stream_write,
    .flush = stream_flush,
};

/* TIOCEXCL only blocks later opens. Check for descriptors that predate it,
 * including callers that do not cooperate with flock. A full or failed query
 * cannot establish ownership and must fail closed. Requiring our PID for the
 * opened path also catches a query that cannot even see our descriptor.
 *
 * This is an OS-visible snapshot, not protection against root bypassing
 * TIOCEXCL, hidden descriptors or drivers that ignore terminal exclusivity.
 */
static h2_pal_result_t check_path_owners(const char* path, int require_self) {
  pid_t owners[256];
  int bytes = proc_listpidspath(PROC_ALL_PIDS, 0, path,
                                PROC_LISTPIDSPATH_EXCLUDE_EVTONLY, owners,
                                sizeof(owners));
  if (bytes < 0 || bytes >= (int)sizeof(owners) ||
      bytes % (int)sizeof(owners[0]))
    return H2_PAL_ERR_UNAVAILABLE;
  int saw_self = 0;
  pid_t self = getpid();
  for (int i = 0; i < bytes / (int)sizeof(owners[0]); ++i) {
    if (owners[i] == self)
      saw_self = 1;
    else if (owners[i] > 0)
      return H2_PAL_ERR_UNAVAILABLE;
  }
  return !require_self || saw_self ? H2_PAL_OK : H2_PAL_ERR_UNAVAILABLE;
}

static h2_pal_result_t check_existing_owners(int fd, const char* port) {
  char path[PATH_MAX];
  struct stat opened, named;
  if (!realpath(port, path) || fstat(fd, &opened) || stat(path, &named) ||
      opened.st_dev != named.st_dev || opened.st_ino != named.st_ino)
    return H2_PAL_ERR_UNAVAILABLE;
  h2_pal_result_t rc = check_path_owners(path, 1);
  if (rc != H2_PAL_OK) return rc;

  /* Darwin exposes callout/dialin nodes for the same serial endpoint. They are
   * different vnodes, so querying only the node we opened misses the other. */
  const char* suffix = NULL;
  const char* prefix = NULL;
  if (!strncmp(path, "/dev/cu.", 8)) {
    suffix = path + 8;
    prefix = "/dev/tty.";
  } else if (!strncmp(path, "/dev/tty.", 9)) {
    suffix = path + 9;
    prefix = "/dev/cu.";
  }
  if (!suffix) return H2_PAL_OK;
  char paired[PATH_MAX];
  size_t prefix_len = strlen(prefix), suffix_len = strlen(suffix);
  if (prefix_len + suffix_len >= sizeof(paired))
    return H2_PAL_ERR_UNAVAILABLE;
  memcpy(paired, prefix, prefix_len);
  memcpy(paired + prefix_len, suffix, suffix_len + 1u);
  if (stat(paired, &named))
    return errno == ENOENT ? H2_PAL_OK : H2_PAL_ERR_UNAVAILABLE;
  return check_path_owners(paired, 0);
}

static h2_pal_result_t serial_open(void* user, const char* port,
                                   const h2_pal_uart_io_stream_config_t* config,
                                   h2_pal_serial_host_session_t** out) {
  *out = NULL;
  int fd = open(port, O_RDWR | O_NOCTTY | O_NONBLOCK | O_CLOEXEC);
  if (fd < 0) return open_error(errno);
  /* Both locks belong to the descriptor Host Core will actually use. Do not
   * pre-open a second guard: TIOCEXCL would reject the core's own open. */
  if (flock(fd, LOCK_EX | LOCK_NB)) {
    h2_pal_result_t rc = open_error(errno);
    (void)close(fd);
    return rc;
  }
  if (ioctl(fd, TIOCEXCL)) {
    h2_pal_result_t rc = open_error(errno);
    (void)flock(fd, LOCK_UN);
    (void)close(fd);
    return rc;
  }
  h2_pal_result_t rc = check_existing_owners(fd, port);
  if (rc != H2_PAL_OK) {
    (void)ioctl(fd, TIOCNXCL);
    (void)flock(fd, LOCK_UN);
    (void)close(fd);
    return rc;
  }
  rc = H2_PAL_ERR_NO_MEMORY;
  h2_pal_serial_host_session_t* session = calloc(1, sizeof(*session));
  if (session) {
    session->fd = fd;
    session->time = user;
    session->stream.user = session;
    session->stream.vtable = &stream_vtable;
    if (!pthread_mutex_init(&session->mutex, NULL)) {
      rc = tcgetattr(fd, &session->original) ? io_error(errno) : H2_PAL_OK;
      if (rc == H2_PAL_OK) rc = configure_locked(session, config);
      if (rc == H2_PAL_OK) {
        *out = session;
        return H2_PAL_OK;
      }
      pthread_mutex_destroy(&session->mutex);
    } else {
      rc = H2_PAL_ERR_IO;
    }
    free(session);
  }
  (void)ioctl(fd, TIOCNXCL);
  (void)flock(fd, LOCK_UN);
  (void)close(fd);
  return rc;
}

static h2_pal_result_t serial_stream(void* user,
                                     h2_pal_serial_host_session_t* session,
                                     const h2_pal_uart_io_stream_api_t** out) {
  (void)user;
  *out = &session->stream;
  return H2_PAL_OK;
}

static h2_pal_result_t serial_set_lines(void* user,
                                        h2_pal_serial_host_session_t* session,
                                        uint32_t mask, uint32_t asserted) {
  (void)user;
  int set = 0, clear = 0;
  if (mask & H2_PAL_SERIAL_HOST_CONTROL_DTR)
    *(asserted & H2_PAL_SERIAL_HOST_CONTROL_DTR ? &set : &clear) |= TIOCM_DTR;
  if (mask & H2_PAL_SERIAL_HOST_CONTROL_RTS)
    *(asserted & H2_PAL_SERIAL_HOST_CONTROL_RTS ? &set : &clear) |= TIOCM_RTS;
  pthread_mutex_lock(&session->mutex);
  h2_pal_result_t rc = H2_PAL_OK;
  if ((clear && ioctl(session->fd, TIOCMBIC, &clear)) ||
      (set && ioctl(session->fd, TIOCMBIS, &set)))
    rc = errno == ENOTTY ? H2_PAL_ERR_UNSUPPORTED : io_error(errno);
  pthread_mutex_unlock(&session->mutex);
  return rc;
}

static h2_pal_result_t serial_get_lines(void* user,
                                        h2_pal_serial_host_session_t* session,
                                        uint32_t* out) {
  (void)user;
  *out = 0;
  int bits;
  pthread_mutex_lock(&session->mutex);
  h2_pal_result_t rc = H2_PAL_OK;
  if (ioctl(session->fd, TIOCMGET, &bits))
    rc = errno == ENOTTY ? H2_PAL_ERR_UNSUPPORTED : io_error(errno);
  else {
    if (bits & TIOCM_DTR) *out |= H2_PAL_SERIAL_HOST_CONTROL_DTR;
    if (bits & TIOCM_RTS) *out |= H2_PAL_SERIAL_HOST_CONTROL_RTS;
  }
  pthread_mutex_unlock(&session->mutex);
  return rc;
}

static h2_pal_result_t serial_close(void* user,
                                    h2_pal_serial_host_session_t** inout) {
  (void)user;
  h2_pal_serial_host_session_t* session = *inout;
  if (!session) return H2_PAL_OK;
  *inout = NULL;
  pthread_mutex_lock(&session->mutex);
  h2_pal_result_t rc = H2_PAL_OK;
  if (tcsetattr(session->fd, TCSANOW, &session->original) && errno != ENXIO &&
      errno != ENODEV && errno != EIO)
    rc = io_error(errno);
  /* Release even when restoring attributes or the device has failed. PTYs can
   * retain TIOCEXCL while their master is alive, so close alone is
   * insufficient. */
  if (ioctl(session->fd, TIOCNXCL) && errno != ENXIO && errno != ENODEV &&
      errno != EIO)
    rc = io_error(errno);
  (void)flock(session->fd, LOCK_UN);
  if (close(session->fd) && rc == H2_PAL_OK) rc = io_error(errno);
  pthread_mutex_unlock(&session->mutex);
  pthread_mutex_destroy(&session->mutex);
  free(session);
  return rc;
}

static h2_pal_result_t scan(void* user, h2_pal_serial_host_snapshot_t** out) {
  (void)user;
  return h2_pal_serial_host_scan(h2_h2loader_cli_target_serial(), out);
}
static h2_pal_result_t snapshot_count(
    void* user, const h2_pal_serial_host_snapshot_t* snapshot, size_t* out) {
  (void)user;
  return h2_pal_serial_host_snapshot_count(h2_h2loader_cli_target_serial(),
                                           snapshot, out);
}
static h2_pal_result_t snapshot_get(
    void* user, const h2_pal_serial_host_snapshot_t* snapshot, size_t index,
    h2_pal_serial_host_port_info_t* out) {
  (void)user;
  return h2_pal_serial_host_snapshot_get(h2_h2loader_cli_target_serial(),
                                         snapshot, index, out);
}
static h2_pal_result_t snapshot_destroy(void* user,
                                        h2_pal_serial_host_snapshot_t** inout) {
  (void)user;
  return h2_pal_serial_host_snapshot_destroy(h2_h2loader_cli_target_serial(),
                                             inout);
}

static const h2_pal_serial_host_vtable_t serial_vtable = {
    .scan = scan,
    .snapshot_count = snapshot_count,
    .snapshot_get = snapshot_get,
    .snapshot_destroy = snapshot_destroy,
    .open = serial_open,
    .session_stream = serial_stream,
    .set_control_lines = serial_set_lines,
    .get_control_lines = serial_get_lines,
    .close = serial_close,
};
static const h2_pal_serial_host_api_t serial_api = {.vtable = &serial_vtable};
#endif

const h2_pal_serial_host_api_t* gm_serial_pal(void) {
#if defined(__APPLE__)
  return &serial_api;
#else
  return h2_h2loader_cli_target_serial();
#endif
}

void gm_serial_pal_init(h2_pal_serial_host_api_t* out,
                        const h2_pal_time_api_t* time) {
  *out = *gm_serial_pal();
#if defined(__APPLE__)
  out->user = (void*)time;
#else
  (void)time;
#endif
}
