/*
 * Copyright (c) 2013 Google, Inc.
 *
 * Use of this source code is governed by a MIT-style
 * license that can be found in the LICENSE file or at
 * https://opensource.org/licenses/MIT
 */
#include <lib/version.h>

#include <lk/debug.h>
#include <stdio.h>
#include <string.h>
#include <lk/init.h>
#include <lk/console_cmd.h>

/* generated for us */
#include <buildid.h>

/* ARCH, PLATFORM, TARGET, PROJECT should be defined by the build system */

/* BUILDID is optional, and may be defined anywhere */
#ifndef BUILDID
#define BUILDID ""
#endif

const lk_version_t lk_version = {
    .struct_version = VERSION_STRUCT_VERSION,
    .arch = ARCH,
    .platform = PLATFORM,
    .target = TARGET,
    .project = PROJECT,
    .buildid = BUILDID
};

static int from_b36(char c) {
    if (c >= '0' && c <= '9')
        return c - '0';
    if (c >= 'A' && c <= 'Z')
        return 10 + (c - 'A');
    return -1;
}

int lk_buildid_format(const char *buildid, char *out, size_t out_len) {
    const char *src;
    size_t n;
    int local = 0;
    char formatted[LK_BUILDID_FORMATTED_MAX];
    int written;

    if (!out || out_len == 0)
        return -1;
    if (!buildid)
        buildid = "";

    src = buildid;
    n = strlen(src);
    if (n >= 6 && !strcmp(src + n - 6, "_LOCAL")) {
        local = 1;
        n -= 6;
    }

    if (n == 5 && src[0] >= 'A' && src[0] <= 'Z') {
        unsigned year = 2011u + (unsigned)(src[0] - 'A');
        int month = from_b36(src[1]);
        int day = from_b36(src[2]);
        int hour = from_b36(src[3]);
        int min2 = from_b36(src[4]);

        if (month >= 1 && month <= 12 && day >= 1 && day <= 31 &&
            hour >= 0 && hour <= 23 && min2 >= 0 && min2 <= 29) {
            written = snprintf(formatted, sizeof(formatted),
                               "%04u-%02d-%02d %02d:%02dZ%s", year, month, day,
                               hour, min2 * 2, local ? " local" : "");
            if (written > 0 && (size_t)written < sizeof(formatted)) {
                if ((size_t)written + 1 > out_len)
                    return -1;
                memcpy(out, formatted, (size_t)written + 1);
                return written;
            }
        }
    }

    n = strlen(buildid);
    if (n + 1 > out_len)
        return -1;
    memcpy(out, buildid, n + 1);
    return (int)n;
}

const char *lk_buildid_string(void) {
    static char buf[LK_BUILDID_FORMATTED_MAX];
    static int ready;

    if (!ready) {
        if (lk_buildid_format(lk_version.buildid, buf, sizeof(buf)) < 0)
            return lk_version.buildid;
        ready = 1;
    }
    return buf;
}

void print_version(void) {
    printf("version:\n");
    printf("\tarch:     %s\n", lk_version.arch);
    printf("\tplatform: %s\n", lk_version.platform);
    printf("\ttarget:   %s\n", lk_version.target);
    printf("\tproject:  %s\n", lk_version.project);
    printf("\tbuildid:  %s\n", lk_buildid_string());
}

static int cmd_version(int argc, const console_cmd_args *argv) {
    print_version();
    return 0;
}

STATIC_COMMAND_START
STATIC_COMMAND_MASKED("version", "print version", &cmd_version, CMD_AVAIL_ALWAYS)
STATIC_COMMAND_END(version);

#if LK_DEBUGLEVEL > 0
// print the version string if any level of debug is set
LK_INIT_HOOK(version, (void *)&print_version, LK_INIT_LEVEL_HEAP - 1);
#endif
