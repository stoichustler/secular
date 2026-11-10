/*
 * Copyright (c) 2008-2013 Travis Geiselbrecht
 *
 * Use of this source code is governed by a MIT-style
 * license that can be found in the LICENSE file or at
 * https://opensource.org/licenses/MIT
 */
#pragma once

#include <stdio.h>

/* Clang C prints __PRETTY_FUNCTION__ as "_Bool foo(uint32_t)"; use __func__. */
#if defined(__cplusplus)
#define TRACE_FN __PRETTY_FUNCTION__
#else
#define TRACE_FN __func__
#endif

/* trace routines */
#define TRACE_ENTRY printf("%s: entry\n", TRACE_FN)
#define TRACE_EXIT printf("%s: exit\n", TRACE_FN)
#define TRACE_ENTRY_OBJ printf("%s: entry obj %p\n", TRACE_FN, this)
#define TRACE_EXIT_OBJ printf("%s: exit obj %p\n", TRACE_FN, this)
#define TRACE printf("%s:%d\n", TRACE_FN, __LINE__)
#define TRACEF(str, x...) do { printf("%s:%d: " str, TRACE_FN, __LINE__, ## x); } while (0)

/* trace routines that work if LOCAL_TRACE is set */
#define LTRACE_ENTRY do { if (LOCAL_TRACE) { TRACE_ENTRY; } } while (0)
#define LTRACE_EXIT do { if (LOCAL_TRACE) { TRACE_EXIT; } } while (0)
#define LTRACE do { if (LOCAL_TRACE) { TRACE; } } while (0)
#define LTRACEF(x...) do { if (LOCAL_TRACE) { TRACEF(x); } } while (0)
#define LTRACEF_LEVEL(level, x...) do { if (LOCAL_TRACE >= (level)) { TRACEF(x); } } while (0)
