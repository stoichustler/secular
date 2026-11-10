#pragma once

#include <interface/smc/smc_def.h>
#include <stdint.h>

#define SMC_SERVICE_PORT "com.android.trusty.kernel.smc"
#define SMC_NUM_PARAMS 4

struct smc_msg {
    uint64_t params[SMC_NUM_PARAMS];
};

struct smc_response {
    struct smc_msg msg;
    int32_t rc;
};
