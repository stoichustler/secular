/*
 * ARM SMC calling convention helpers used by Trusty lib/sm.
 * Numeric-only so the same header works in assembly.
 */
#ifndef __INTERFACE_SMC_SMC_DEF_H
#define __INTERFACE_SMC_SMC_DEF_H

#define SMC_NUM_ENTITIES 64
#define SMC_NUM_ARGS 4
#define SMC_NUM_PARAMS (SMC_NUM_ARGS - 1)

#define SMC_IS_FASTCALL(smc_nr) ((smc_nr) & 0x80000000)
#define SMC_IS_SMC64(smc_nr) ((smc_nr) & 0x40000000)
#define SMC_ENTITY(smc_nr) (((smc_nr) & 0x3F000000) >> 24)
#define SMC_FUNCTION(smc_nr) ((smc_nr) & 0x0000FFFF)

#define SMC_NR(entity, fn, fastcall, smc64) \
    ((((fastcall) & 1) << 31) | (((smc64) & 1) << 30) | (((entity) & 0x3F) << 24) | ((fn) & 0xFFFF))

#define SMC_FASTCALL_NR(entity, fn) SMC_NR((entity), (fn), 1, 0)
#define SMC_FASTCALL64_NR(entity, fn) SMC_NR((entity), (fn), 1, 1)
#define SMC_STDCALL_NR(entity, fn) SMC_NR((entity), (fn), 0, 0)
#define SMC_STDCALL64_NR(entity, fn) SMC_NR((entity), (fn), 0, 1)

#define SMC_ENTITY_ARCH 0
#define SMC_ENTITY_CPU 1
#define SMC_ENTITY_SIP 2
#define SMC_ENTITY_OEM 3
#define SMC_ENTITY_STD 4
#define SMC_ENTITY_RESERVED 5
#define SMC_ENTITY_TRUSTED_APP 48
#define SMC_ENTITY_TRUSTED_OS 50
#define SMC_ENTITY_LOGGING 51
#define SMC_ENTITY_SECURE_MONITOR 60

#endif
