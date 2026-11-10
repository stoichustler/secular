#pragma once

#include <lib/backtrace.h>
#include <kernel/thread.h>

static inline void dump_thread_backtrace(struct thread *thread) {
    (void)thread;
    backtrace_print_current();
}

static inline void dump_backtrace(void) {
    backtrace_print_current();
}
