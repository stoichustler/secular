# Guest Linux

Savior’s guests are ordinary ARM64 Linux. This note is **what they
run that Savior cares about**: occupancy actuator, ACRN/VHM
frontend, vtee/TIPC, and the Mali arbiter client. CFS, binder,
and Android HAL stay out.

Read [`scheduler.md`](scheduler.md) for occupancy + Policy1,
[`vhm.md`](vhm.md) for SOS→UOS, [`tee.md`](tee.md) for SMC,
[`gpu.md`](gpu.md) for SWA, [`devices.md`](devices.md) for virtio.

The **module source** for `nebula_sched` and `mali_arb` is not
in this tree's `linux/drivers/` (Yocto/ALPS extras). The
contracts they implement **are** in Savior (shared page, SMC,
DTS). ACRN HSM and vtee **are** here.

## Principle

1. **Savior never schedules a guest task.** A guest CPU is a
   pinned host thread. Occupancy is “may this Linux CPU run”;
   Policy1 is “raise that host thread”. The guest module is an
   actuator, not a second scheduler.

2. **SOS is the only ACRN client.** `/dev/acrn_hsm` issues
   `SMC_SC_NBL_VHM_REQ`. UOS does not. tbox does not.

3. **TEE from a guest is `smc`, not `hvc`.** vtee sets
   `SMC_TEE_SECURE_ID`. HYP FIDs use `SMC_HYP_SECURE_ID` in
   `x7`. Savior overwrites `x7` with VMID on TEE forward.

## Framework

```text
 SOS Linux                         UOS Linux              tbox Linux
  CFS / FIFO                        same                   same
  nebula_sched  (occupancy)         nebula_sched           nebula_sched
  mali_kbase + mali_arb             mali_kbase + mali_arb  (no SWA)
  /dev/acrn_hsm  (VHM FE)           virtio FE of SOS BE    virtio-console
  vtee / TIPC                       vtee                   —
  PSCI cpuidle                      PSCI                   PSCI
         │ SMC / doorbell / virtio
         ▼
 Savior  VCPU thread + stage-2
```

## Occupancy (`nebula_sched`)

Host writes RUN/PAUSE/SHARE/OFFLINE into shared DRAM and rings
`sched_irq` on that VM’s VCPU 0. The guest module parks the
Linux CPU. The **host VCPU thread stays READY**. Detail and
layout: [`scheduler.md`](scheduler.md) **nebula_sched**.

If the module is missing: Policy1 cannot see `preempt_count` /
`policy`; occupancy doorbells are ignored; two FIFOs can sit on
one SHARE core.

## Cross-VM lock

Shared DRAM mutexes for SOS/UOS/tbox userspace (modem, display,
…), **not** the LK scheduler. Layout: 16 packed `uint32_t`s
(`LOCK_ID_START..MAX-1`).

```text
 bits  7:0   lock_id
      15:8   cnt     LOCK=1 / UNLOCK=0
     23:16   create_vmid  bitmask  YOCTO=1 ALPS=2 TBOX=4
     31:24   current_vmid  kernel VMID of holder
```

VMM: `zx_guest_set_cvm_lock_vmo` (one shared VMO, mapped host +
all guests) and `zx_guest_set_cvm_lock_irq` (virtual IRQ per VM).

On guest destroy / teardown, Savior `cross_vm_lock_check(vmid)`:

```text
 for id 1..15
    ldar slot
    if holder == dying VM  →  UNLOCK, drop that bit from create_vmid
    else if create_vmid has that bit  →  clear the bit only
    stlxr
 inject cvm_lock_irq into remaining creators  (VCPU 0, VIRTUAL)
```

Guests take/release with `ldaxr`/`stlxr` in their own code. Host
does not participate except destroy-cleanup. Missing VMO →
`cross_vm_lock_check` returns; missing IRQ → inject logs and
skips.

## VHM frontend (SOS)

```text
 linux/drivers/virt/acrn/          (HSM)
 linux/drivers/vhm/                (legacy wrap)
 linux/include/uapi/linux/acrn.h
 linux/arch/arm64/include/asm/acrn.h   acrn_hypercall5
    r7 = SMC_HYP_SECURE_ID << 16
    SMC_SC_NBL_VHM_REQ
```

Ioctls `ACRN_IOCTL_CREATE_VM` / `START_VM` / `SET_IOREQ` /
`NOTIFY_REQUEST_FINISH` map 1:1 to `HC_*` ([`vhm.md`](vhm.md)).
Kick SPI 254 is a **physical** IRQ into SOS (`setVhmIrq`).

UOS sees virtio/PCI BARs that SOS emulates; it has no `/dev/acrn_hsm`.

## vtee / TIPC

```text
 linux/drivers/nebula/vtee.c         SMC_TEE_SECURE_ID
 linux/drivers/nebula/vtee-virtio.c
 linux/drivers/nebula/trusty-*.c     host-side glue in guest
 lua  setTipcVqNotifierIrq(245)
      setVteeNotifierIrq(513)
```

AHYP: Savior `trusty::vcpu_kick` the VCPU that posted. TEE
bootmode: NS IRQ instead. Wrong kick = hung IPC.

## Mali arbiter client

Guest DTS node is **injected by the VMM**, not a board file:

```text
 /arbiter  compatible = "mali,sw-arbiter"
    GRANT / STOP / LOST / RCVR     virtual
    MALI-JOB/MMU/GPU/EVT/PWR       physical, only while granted
```

`mali_arb` SMCs `SMC_FC_NBL_VHM_REQ` / `FC_ID_GPU_ARBITER`.
Protocol: [`gpu.md`](gpu.md). tbox does not get this node.

## PSCI the guest issues

| Guest call | Savior |
| --- | --- |
| `CPU_ON` / `OFF` / `SUSPEND` | [`cpu.md`](cpu.md) |
| `SYSTEM_RESET` / `RESET2` | [`vmm.md`](vmm.md) |
| `SYSTEM_OFF` | SOS → ATF; others canceled |
| cpuidle s2idle | [`pm.md`](pm.md) once all VMs park |

## virtio frontends

Ordinary Linux virtio. Backends in that guest’s Vmm (or SOS for
UOS). Console `hvc0` is tbox. vsock IRQs 380/382 are SOS lua.
Block/RPMB/cluster/eint/rtc are feature bits, **off on QEMU**
(`vmm.cc` `is_qemu_platform()` skips several).

## Pitfalls

- **No `nebula_sched`.** Occupancy and Policy1 degrade silently.
- **ACRN ioctl from UOS.** There is no handler; UOS is the FE
  of SOS, not a hypervisor client.
- **`hvc` to Savior.** `x0 = -1`. Use `smc`.
- **Mali without `/arbiter`.** Driver never SMCs; GPU stays
  Ungranted for that VM.

## Terminology

| Term | Meaning here |
| --- | --- |
| Actuator | Guest module that parks a CPU or yields the GPU |
| HSM | ACRN Hypervisor Service Module in SOS |
| Injected DTS | VMM writes `/arbiter` into the guest tree at launch |
| `SMC_HYP_SECURE_ID` | r7 for VHM/GPU/vmlog SMCs |

## Code

| Step | File |
| --- | --- |
| Occupancy contract | [`scheduler.md`](scheduler.md), `zircon/kernel/include/kernel/sched.h` |
| Cross-VM lock | `zircon/kernel/arch/arm64/hypervisor/cross_vm_lock.cpp` |
| ACRN HSM | `linux/drivers/virt/acrn/` |
| Legacy VHM | `linux/drivers/vhm/` |
| SMC wrapper | `linux/arch/arm64/include/asm/acrn.h` |
| vtee | `linux/drivers/nebula/vtee.c` |
| TIPC in guest | `linux/drivers/nebula/trusty-ipc.c` |
| Arbiter DTS inject | `garnet/bin/guest/vmm.cc`, `main_uos.cc` `add_arbiter_dts` |
| virtio BE | `garnet/bin/guest/vmm.cc` `Vmm::Initialize` |

---

Everyday's a Hustle.
