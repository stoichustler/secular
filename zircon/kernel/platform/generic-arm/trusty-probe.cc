// Copyright 2026 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#include <inttypes.h>
#include <lib/console.h>
#include <lk/init.h>
#include <stdio.h>

#include <arch/arm64/smccc.h>

namespace {

constexpr uint32_t kSmcFcGetUuid = 0xbf00ff01u;
constexpr uint32_t kSmcYcNop = 0x3c000001u;
constexpr uint64_t kTrustySel1Alive = 0;              // lib/sm SMC_SC_LOCKED_NOP
constexpr uint64_t kTrustySel1AliveLegacy = 0x54454521ull;  // old trusty-tee stub
constexpr uint64_t kSmcUnk = 0xffffffffull;

constexpr uint32_t kTrustyUuidWord0 = 0xf025ee40;
constexpr uint32_t kTrustyUuidWord1 = 0x4c30bca2;
constexpr uint32_t kTrustyUuidWord2 = 0x73a14c8c;
constexpr uint32_t kTrustyUuidWord3 = 0xf18a7dc5;

bool TrustyUuidMatches(const arm_smccc_result_t& r) {
  return static_cast<uint32_t>(r.x0) == kTrustyUuidWord0 &&
         static_cast<uint32_t>(r.x1) == kTrustyUuidWord1 &&
         static_cast<uint32_t>(r.x2) == kTrustyUuidWord2 &&
         static_cast<uint32_t>(r.x3) == kTrustyUuidWord3;
}

int ProbeTrusty(int argc, const cmd_args* argv, uint32_t flags) {
  (void)argc;
  (void)argv;
  (void)flags;

  const arm_smccc_result_t uuid = arm_smccc_smc(kSmcFcGetUuid, 0, 0, 0, 0, 0, 0, 0);
  if (uuid.x0 == kSmcUnk || !TrustyUuidMatches(uuid)) {
    printf("TEE: dead (spd) uuid=0x%" PRIx64 " 0x%" PRIx64 " 0x%" PRIx64 " 0x%" PRIx64 "\n",
           uuid.x0, uuid.x1, uuid.x2, uuid.x3);
    return -1;
  }

  const arm_smccc_result_t nop = arm_smccc_smc(kSmcYcNop, 0, 0, 0, 0, 0, 0, 0);
  if (nop.x0 != kTrustySel1Alive && nop.x0 != kTrustySel1AliveLegacy) {
    printf("TEE: dead (s-el1) nop=0x%" PRIx64 "\n", nop.x0);
    return -1;
  }

  printf("TEE: alive\n");
  return 0;
}

void TrustyProbeInit(uint level) {
  (void)level;
  (void)ProbeTrusty(0, nullptr, 0);
}

LK_INIT_HOOK(trusty_probe, TrustyProbeInit, LK_INIT_LEVEL_THREADING)

STATIC_COMMAND_START
STATIC_COMMAND("trusty", "probe Trusty TEE (SPD UUID + S-EL1 NOP)", &ProbeTrusty)
STATIC_COMMAND_END(trusty)

}  // namespace
