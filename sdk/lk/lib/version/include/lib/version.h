/*
 * Copyright (c) 2013 Google, Inc.
 *
 * Use of this source code is governed by a MIT-style
 * license that can be found in the LICENSE file or at
 * https://opensource.org/licenses/MIT
 */
#pragma once

#include <lk/compiler.h>
#include <stddef.h>

__BEGIN_CDECLS

#define VERSION_STRUCT_VERSION 0x1

/* "YYYY-MM-DD HH:MMZ local" plus NUL */
#define LK_BUILDID_FORMATTED_MAX 32

typedef struct {
    unsigned int struct_version;
    const char *arch;
    const char *platform;
    const char *target;
    const char *project;
    const char *buildid;
} lk_version_t;

extern const lk_version_t lk_version;

void print_version(void);

/**
 * lk_buildid_format - Decode a compact buildid.sh stamp into UTC text.
 *
 * Compact form is five chars (year since 2011, month, day, hour, minute/2
 * in base 36) with an optional `_LOCAL` suffix. The formatted result is
 * `YYYY-MM-DD HH:MMZ` or `YYYY-MM-DD HH:MMZ local`.
 *
 * Unrecognized stamps are copied through unchanged.
 *
 * Returns the number of bytes written excluding NUL, or -1 if @out_len is
 * too small.
 */
int lk_buildid_format(const char *buildid, char *out, size_t out_len);

/** Format `lk_version.buildid` into a static buffer. */
const char *lk_buildid_string(void);

__END_CDECLS

