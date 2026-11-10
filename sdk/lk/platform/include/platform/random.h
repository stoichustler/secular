#pragma once

#include <rand.h>
#include <stddef.h>
#include <stdint.h>

static inline void platform_random_get_bytes(uint8_t *buf, size_t len) {
    for (size_t i = 0; i < len; i++) {
        buf[i] = (uint8_t)rand();
    }
}
