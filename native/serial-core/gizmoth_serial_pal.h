#ifndef GIZMOTH_SERIAL_PAL_H
#define GIZMOTH_SERIAL_PAL_H

#include "h2/pal/os/h2_pal_serial_host.h"
#include "h2/pal/os/h2_pal_time.h"

/* macOS sessions own their descriptor and acquire kernel exclusivity. Other
 * platforms retain the pinned target PAL (including Windows zero sharing). */
const h2_pal_serial_host_api_t* gm_serial_pal(void);
/* Borrow the per-context guarded clock for bounded macOS I/O waits. */
void gm_serial_pal_init(h2_pal_serial_host_api_t* out,
                        const h2_pal_time_api_t* time);

#endif
