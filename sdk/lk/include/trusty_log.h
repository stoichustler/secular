#pragma once

#include <debug.h>

#ifndef TLOG_TAG
#define TLOG_TAG "trusty"
#endif

#define TLOG(fmt, x...) dprintf(ALWAYS, "%s: %d: " fmt, TLOG_TAG, __LINE__, ##x)
#define TLOGD(fmt, x...) dprintf(SPEW, "%s: %d: " fmt, TLOG_TAG, __LINE__, ##x)
#define TLOGI(fmt, x...) dprintf(SPEW, "%s: %d: " fmt, TLOG_TAG, __LINE__, ##x)
#define TLOGW(fmt, x...) dprintf(INFO, "%s: %d: " fmt, TLOG_TAG, __LINE__, ##x)
#define TLOGE(fmt, x...) dprintf(CRITICAL, "%s: %d: " fmt, TLOG_TAG, __LINE__, ##x)
#define TLOGC(fmt, x...) dprintf(CRITICAL, "%s: %d: " fmt, TLOG_TAG, __LINE__, ##x)
