/* Exercise real serial startup without opening or enumerating any devices.
 * The CLI platform's Darwin system-event init used to start a route monitor
 * whose own RTM_GET replies repeatedly woke it, consuming an entire CPU core.
 */
#include <assert.h>
#include <mach/mach.h>
#include <stdio.h>

#include "gizmoth_serial_core.h"
#include "h2_h2loader_cli_target.h"

static mach_msg_type_number_t thread_count(void) {
  thread_act_array_t threads;
  mach_msg_type_number_t count;
  assert(task_threads(mach_task_self(), &threads, &count) == KERN_SUCCESS);
  for (mach_msg_type_number_t i = 0; i < count; ++i)
    assert(mach_port_deallocate(mach_task_self(), threads[i]) == KERN_SUCCESS);
  assert(vm_deallocate(mach_task_self(), (vm_address_t)threads,
                       count * sizeof(*threads)) == KERN_SUCCESS);
  return count;
}

int main(void) {
  mach_msg_type_number_t before = thread_count();
  assert(gm_h2l_global_start() == H2_PAL_OK);
  assert(gm_h2l_global_start() == H2_PAL_OK);

  /* Serial sessions still need these stateless services, even though they do
   * not need the CLI's filesystem mounts, system events or network monitor. */
  const h2_pal_mem_api_t* mem = h2_h2loader_cli_target_mem();
  assert(mem != NULL);
  void* block = h2_pal_mem_alloc(mem, 32u);
  assert(block != NULL);
  h2_pal_mem_free(mem, block);
  uint64_t now;
  assert(h2_pal_time_get_monotonic_ms(h2_h2loader_cli_target_time(), &now) ==
         H2_PAL_OK);

  mach_msg_type_number_t after = thread_count();
  printf("serial startup threads: %u -> %u\n", before, after);
  assert(after == before);
  return 0;
}
