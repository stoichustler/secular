/*
 * Copyright (c) 2022 Travis Geiselbrecht
 *
 * Use of this source code is governed by a MIT-style
 * license that can be found in the LICENSE file or at
 * https://opensource.org/licenses/MIT
 */
#include "dev/power/psci.h"

#if LK_TRUSTY_TEE
#include <arch/arm64.h>
#endif
#include <lk/debug.h>
#include <lk/init.h>

#if WITH_LIB_CONSOLE
#include <lk/console_cmd.h>
#endif

// PSCI function numbers as of PSCI 1.3 (DEN 0022 F.b)
enum psci_function {
    PSCI_VERSION = 0x84000000,
    PSCI_CPU_SUSPEND = 0x84000001,
    PSCI_CPU_OFF = 0x84000002,
    PSCI_CPU_ON = 0x84000003,
    PSCI_AFFINITY_INFO = 0x84000004,
    PSCI_MIGRATE = 0x84000005,
    PSCI_MIGRATE_INFO_TYPE = 0x84000006,
    PSCI_MIGRATE_INFO_UP_CPU = 0x84000007,
    PSCI_SYSTEM_OFF = 0x84000008,
    PSCI_SYSTEM_RESET = 0x84000009,
    PSCI_SYSTEM_RESET2 = 0x84000012,
    PSCI_FEATURES = 0x8400000A,
    PSCI_CPU_FREEZE = 0x8400000B,
    PSCI_CPU_DEFAULT_SUSPEND = 0x8400000C,
    PSCI_NODE_HW_STATE = 0x8400000D,
    PSCI_SYSTEM_SUSPEND = 0x8400000E,
    PSCI_SET_SUSPEND_MODE = 0x8400000F,
    PSCI_STAT_RESIDENCY = 0x84000010,
    PSCI_STAT_COUNT = 0x84000011,
    PSCI_MEM_PROTECT = 0x84000013,
    PSCI_MEM_PROTECT_RANGE = 0xC4000014,
    PSCI_SYSTEM_OFF2 = 0xC4000015,

    PSCI_SMC64_FLAG = 0x40000000,
};

/* low level ASM routine to make the raw PSCI call */
int psci_call(ulong arg0, ulong arg1, ulong arg2, ulong arg3);

// wrap the function number in the SMC64 flag if we're on aarch64
static inline uint32_t psci_function_64(uint32_t func) {
#if __aarch64__
    return func | PSCI_SMC64_FLAG;
#else
    return func;
#endif
}

uint32_t psci_version(void) {
    return psci_call(PSCI_VERSION, 0, 0, 0);
}

int psci_cpu_on(ulong mpidr, ulong entrypoint, uint32_t cpu_num) {
    return psci_call(psci_function_64(PSCI_CPU_ON), mpidr, entrypoint, cpu_num);
}

void psci_system_off(void) {
    psci_call(PSCI_SYSTEM_OFF, 0, 0, 0);
}

void psci_system_reset(void) {
    psci_call(PSCI_SYSTEM_RESET, 0, 0, 0);
}

void psci_detect(uint level) {
    (void)level;
#if LK_TRUSTY_TEE
    /*
     * Boot chain (ATF SPD=trusty):
     *
     *   EL3  ATF
     *     --enter BL32-->          S-EL1 Trusty (this image)
     *     --SMC_YC_NS_RETURN-->    EL3
     *     --exit to NS-->          NS EL2 lkboot (BL33)
     *
     * CurrentEL encodes only the exception level, not Secure vs NS.
     * EL3 entered this payload with SCR_EL3.NS=0, so EL1 here is S-EL1.
     *
     * PSCI (ARM DEN0022) is a Normal-World firmware service. ATF's
     * psci_smc_handler() returns SMC_UNK (-1) unless the caller is
     * non-secure:
     *
     *   if (!is_caller_non_secure(flags))
     *       return SMC_UNK;   // 0xffffffff -> "ffff:ffff"
     *
     * An SMC PSCI_VERSION from S-EL1 is therefore not a version probe,
     * it is an illegal client. CPU on/off/suspend for TEE is driven by
     * EL3 through psci_register_spd_pm_hook(), not by this PSCI client.
     */
    char el_name[ARM64_EL_NAME_BUF];

    dprintf(INFO, "\nTEE: booting Trusty OS at %s\n",
            arm64_current_el_name(el_name, sizeof(el_name)));
#else
    uint32_t ver = psci_version();
    if (ver == 0) {
        // no psci support
        return;
    }
    dprintf(INFO, "PSCI: detected version %x:%x\n", ver >> 16, ver & 0xffff);
#endif
}

LK_INIT_HOOK(psci, psci_detect, LK_INIT_LEVEL_PLATFORM_EARLY);

#if WITH_LIB_CONSOLE

static int cmd_psci_version(int argc, const console_cmd_args *argv) {
    uint32_t ret = psci_version();
    printf("PSCI VERSION: 0x%x\n", ret);
    return 0;
}

STATIC_COMMAND_START
STATIC_COMMAND_MASKED("psci_version", "show psci version", &cmd_psci_version, CMD_AVAIL_ALWAYS)
STATIC_COMMAND_END(psci);
#endif
