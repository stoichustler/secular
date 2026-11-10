/*
 * Copyright (c) 2008 Travis Geiselbrecht
 *
 * Use of this source code is governed by a MIT-style
 * license that can be found in the LICENSE file or at
 * https://opensource.org/licenses/MIT
 */
#pragma once

#include <lk/compiler.h>
#include <sys/types.h>

/* Routines to read the current system time since an arbitrary point in the
 * past. Usually system boot time.
 */

__BEGIN_CDECLS

/* Time in units of milliseconds */
lk_time_t current_time(void);

/* Time in units of microseconds */
lk_bigtime_t current_time_hires(void);

/* Time in units of nanoseconds (Trusty overlay). */
static inline lk_time_ns_t current_time_ns(void) {
    return current_time_hires() * 1000ULL;
}

/* spin the cpu for a period of (short) time */
void spin(uint32_t usecs);

__END_CDECLS

