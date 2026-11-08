// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <stdint.h>
#include <string.h>
#include <zircon/status.h>
#include <zircon/syscalls/object.h>

#include <zxtest/zxtest.h>

#include "src/sys/bin/psutils/ps_internal.h"

static task_entry_t kFullKoidEntries[] = {
    {
        .type = 'j',
        .koid_str = "123456",
        .parent_koid_str = "0",
        .depth = 0,
        .name = "root",
        .state_str = "",
        .pss_bytes = 1234 * 1024 * 1024,
        .private_bytes = 1234 * 1024 * 1024,
        .shared_bytes = 1234 * 1024 * 1024,
    },
    {
        .type = 'j',
        .koid_str = "7894567",
        .parent_koid_str = "123456",
        .depth = 1,
        .name = "my-job",
        .state_str = "",
        .pss_bytes = 1234 * 1024 * 1024,
        .private_bytes = 1234 * 1024 * 1024,
        .shared_bytes = 1234 * 1024 * 1024,
    },
    {
        .type = 'p',
        .koid_str = "123456789",
        .parent_koid_str = "7894567",
        .depth = 2,
        .name = "my-proc",
        .state_str = "",
        .pss_bytes = 1234 * 1024 * 1024,
        .private_bytes = 1234 * 1024 * 1024,
        .shared_bytes = 1234 * 1024 * 1024,
    },
    {
        .type = 't',
        .koid_str = "1",
        .parent_koid_str = "123456789",
        .depth = 3,
        .name = "main",
        .state_str = "running",
        .cpu = 0,
    },
    {
        .type = 't',
        .koid_str = "2",
        .parent_koid_str = "123456789",
        .depth = 3,
        .name = "worker",
        .state_str = "blocked",
        .cpu = 1,
    },
    {
        .type = 't',
        .koid_str = "3",
        .parent_koid_str = "123456789",
        .depth = 3,
        .name = "unstarted",
        .state_str = "new",
        .cpu = ZX_INFO_INVALID_CPU,
    },
};

// Last character koid of deepest entry was getting dropped, see
// https://bugs.fuchsia.dev/p/fuchsia/issues/detail?id=7014#c48.
TEST(PsUtilsPrintTable, FullKoid) {
  char buf[4096];
  FILE* f = fmemopen(buf, sizeof(buf), "w");
  ASSERT_TRUE(f != NULL);

  ps_options_t options = {.also_show_threads = false, .only_show_jobs = false, .format_unit = 0};
  task_table_t table = {.entries = kFullKoidEntries, .num_entries = 6, .capacity = 6};
  print_table(&table, &options, f);
  fclose(f);
  EXPECT_STREQ(buf,
               "TASK                 PSS PRIVATE  SHARED   STATE CPU NAME\n"
               "j: 123456          1234M   1234M                     root\n"
               "  j: 7894567       1234M   1234M                     my-job\n"
               "    p: 123456789   1234M   1234M   1234M             my-proc\n"
               "TASK                 PSS PRIVATE  SHARED   STATE CPU NAME\n");
}

TEST(PsUtilsPrintTable, WithThreads) {
  char buf[4096];
  FILE* f = fmemopen(buf, sizeof(buf), "w");
  ASSERT_TRUE(f != NULL);

  ps_options_t options = {.also_show_threads = true, .only_show_jobs = false, .format_unit = 0};
  task_table_t table = {.entries = kFullKoidEntries, .num_entries = 6, .capacity = 6};
  print_table(&table, &options, f);
  fclose(f);
  EXPECT_STREQ(buf,
               "TASK                 PSS PRIVATE  SHARED   STATE CPU NAME\n"
               "j: 123456          1234M   1234M                     root\n"
               "  j: 7894567       1234M   1234M                     my-job\n"
               "    p: 123456789   1234M   1234M   1234M             my-proc\n"
               "      t: 1                               running   0 main\n"
               "      t: 2                               blocked   1 worker\n"
               "      t: 3                                   new   - unstarted\n"
               "TASK                 PSS PRIVATE  SHARED   STATE CPU NAME\n");
}

TEST(PsUtilsPrintTable, OnlyJobs) {
  char buf[4096];
  FILE* f = fmemopen(buf, sizeof(buf), "w");
  ASSERT_TRUE(f != NULL);

  ps_options_t options = {.also_show_threads = false, .only_show_jobs = true, .format_unit = 0};
  task_table_t table = {.entries = kFullKoidEntries, .num_entries = 2, .capacity = 6};
  print_table(&table, &options, f);
  fclose(f);
  EXPECT_STREQ(buf,
               "TASK             PSS PRIVATE   STATE CPU NAME\n"
               "j: 123456      1234M   1234M             root\n"
               "  j: 7894567   1234M   1234M             my-job\n"
               "TASK             PSS PRIVATE   STATE CPU NAME\n");
}

TEST(PsUtilsPs, PrintAll) {
  ps_options_t options = {.also_show_threads = false, .only_show_jobs = false, .format_unit = 0};
  ASSERT_OK(show_all_jobs(&options));
}

TEST(PsUtilsPs, PrintAllThreads) {
  ps_options_t options = {.also_show_threads = true, .only_show_jobs = false, .format_unit = 0};
  ASSERT_OK(show_all_jobs(&options));
}

TEST(PsUtilsPs, PrintAllJobs) {
  ps_options_t options = {.also_show_threads = false, .only_show_jobs = true, .format_unit = 0};
  ASSERT_OK(show_all_jobs(&options));
}
