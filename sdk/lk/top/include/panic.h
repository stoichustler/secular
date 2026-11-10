#pragma once

#include <lk/debug.h>

#ifndef PANIC_UNIMPLEMENTED
#define PANIC_UNIMPLEMENTED panic("%s unimplemented\n", __PRETTY_FUNCTION__)
#endif
