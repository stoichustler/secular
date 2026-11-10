// Copyright 2026 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.kernel/cpp/wire.h>
#include <lib/component/incoming/cpp/protocol.h>
#include <lib/zx/profile.h>
#include <lib/zx/thread.h>
#include <stdio.h>
#include <zircon/status.h>
#include <zircon/syscalls.h>
#include <zircon/syscalls/profile.h>
#include <zircon/syscalls/smc.h>

#include <cinttypes>

namespace {

// ATF Trusty SPD UUID: 40ee25f0-a2bc-304c-8c4c-a173c57d8af1
// Packed as SMCCC UUID words (byte 0 in the low-order bits of each word).
constexpr uint32_t kTrustyUuidWord0 = 0xf025ee40;
constexpr uint32_t kTrustyUuidWord1 = 0x4c30bca2;
constexpr uint32_t kTrustyUuidWord2 = 0x73a14c8c;
constexpr uint32_t kTrustyUuidWord3 = 0xf18a7dc5;

// SMCCC Trusted OS UID query (fast, SMC32, entity 63, function 0xFF01).
constexpr uint32_t kSmcFcGetUuid = 0xbf00ff01u;
// Yielding NOP into S-EL1 (entity 60, function 1).
constexpr uint32_t kSmcYcNop = 0x3c000001u;
// lib/sm SMC_SC_LOCKED_NOP returns 0. The old trusty-tee stub returned 'TEE!'.
constexpr uint64_t kTrustySel1Alive = 0;
constexpr uint64_t kTrustySel1AliveLegacy = 0x54454521ull;  // 'TEE!'

constexpr uint64_t kSmcUnk = 0xffffffffull;

void PrintUuid(uint32_t w0, uint32_t w1, uint32_t w2, uint32_t w3) {
  uint8_t b[16] = {
      static_cast<uint8_t>(w0),
      static_cast<uint8_t>(w0 >> 8),
      static_cast<uint8_t>(w0 >> 16),
      static_cast<uint8_t>(w0 >> 24),
      static_cast<uint8_t>(w1),
      static_cast<uint8_t>(w1 >> 8),
      static_cast<uint8_t>(w1 >> 16),
      static_cast<uint8_t>(w1 >> 24),
      static_cast<uint8_t>(w2),
      static_cast<uint8_t>(w2 >> 8),
      static_cast<uint8_t>(w2 >> 16),
      static_cast<uint8_t>(w2 >> 24),
      static_cast<uint8_t>(w3),
      static_cast<uint8_t>(w3 >> 8),
      static_cast<uint8_t>(w3 >> 16),
      static_cast<uint8_t>(w3 >> 24),
  };
  printf("%02x%02x%02x%02x-%02x%02x-%02x%02x-%02x%02x-%02x%02x%02x%02x%02x%02x", b[0], b[1], b[2],
         b[3], b[4], b[5], b[6], b[7], b[8], b[9], b[10], b[11], b[12], b[13], b[14], b[15]);
}

zx_status_t SmcCall(zx_handle_t smc, uint32_t func_id, zx_smc_result_t* out) {
  zx_smc_parameters_t params = {
      .func_id = func_id,
  };
  return zx_smc_call(smc, &params, out);
}

// Yielding SMCs switch to this CPU's S-EL1 context. ATF CPU_ON cold-boots
// Trusty on each core, so NOP is valid on every online CPU.
zx_status_t PinCurrentThreadToCpu(uint32_t cpu) {
  zx::result client = component::Connect<fuchsia_kernel::ProfileResource>();
  if (client.is_error()) {
    return client.status_value();
  }
  const fidl::WireResult got = fidl::WireCall(client.value())->Get();
  if (!got.ok()) {
    return got.status();
  }
  const zx_profile_info_t info = {
      .flags = ZX_PROFILE_INFO_FLAG_CPU_MASK,
      .cpu_affinity_mask = {.mask = {UINT64_C(1) << cpu}},
  };
  zx::profile profile;
  zx_status_t status = zx::profile::create(got.value().resource, 0u, &info, &profile);
  if (status != ZX_OK) {
    return status;
  }
  return zx::thread::self()->set_profile(profile, 0u);
}

}  // namespace

int main(int argc, char** argv) {
  (void)argc;
  (void)argv;

  zx::result client = component::Connect<fuchsia_kernel::SmcResource>();
  if (client.is_error()) {
    fprintf(stderr, "TEE: connect SmcResource: %s\n", client.status_string());
    printf("TEE: dead (no smc resource)\n");
    return 1;
  }

  const fidl::WireResult got = fidl::WireCall(client.value())->Get();
  if (!got.ok()) {
    fprintf(stderr, "TEE: SmcResource.Get: %s\n", got.status_string());
    printf("TEE: dead (no smc resource)\n");
    return 1;
  }
  const zx::resource& smc = got.value().resource;

  zx_smc_result_t uuid_res = {};
  zx_status_t status = SmcCall(smc.get(), kSmcFcGetUuid, &uuid_res);
  if (status != ZX_OK) {
    fprintf(stderr, "TEE: GET_UUID smc: %s\n", zx_status_get_string(status));
    printf("TEE: dead (spd)\n");
    return 1;
  }
  if (uuid_res.arg0 == kSmcUnk) {
    printf("TEE: GET_UUID -> SMC_UNK\n");
    printf("TEE: dead (spd)\n");
    return 1;
  }

  const auto w0 = static_cast<uint32_t>(uuid_res.arg0);
  const auto w1 = static_cast<uint32_t>(uuid_res.arg1);
  const auto w2 = static_cast<uint32_t>(uuid_res.arg2);
  const auto w3 = static_cast<uint32_t>(uuid_res.arg3);
  printf("TEE: spd uuid ");
  PrintUuid(w0, w1, w2, w3);
  printf("\n");
  if (w0 != kTrustyUuidWord0 || w1 != kTrustyUuidWord1 || w2 != kTrustyUuidWord2 ||
      w3 != kTrustyUuidWord3) {
    printf("TEE: dead (unexpected uuid)\n");
    return 1;
  }

  const uint32_t num_cpus = zx_system_get_num_cpus();
  uint32_t live = 0;
  for (uint32_t cpu = 0; cpu < num_cpus; cpu++) {
    zx_status_t pin_status = PinCurrentThreadToCpu(cpu);
    if (pin_status != ZX_OK) {
      fprintf(stderr, "TEE: pin cpu %u: %s\n", cpu, zx_status_get_string(pin_status));
      printf("TEE: dead (pin cpu %u)\n", cpu);
      return 1;
    }

    zx_smc_result_t nop_res = {};
    status = SmcCall(smc.get(), kSmcYcNop, &nop_res);
    if (status != ZX_OK) {
      fprintf(stderr, "TEE: NOP smc cpu %u: %s\n", cpu, zx_status_get_string(status));
      printf("TEE: dead (S-EL1 cpu %u)\n", cpu);
      return 1;
    }
    printf("TEE: s-el1 nop cpu %u -> 0x%" PRIx64 "\n", cpu, nop_res.arg0);
    if (nop_res.arg0 != kTrustySel1Alive && nop_res.arg0 != kTrustySel1AliveLegacy) {
      printf("TEE: dead (S-EL1 cpu %u)\n", cpu);
      return 1;
    }
    live++;
  }

  printf("TEE: alive (%u cpus)\n", live);
  return 0;
}
