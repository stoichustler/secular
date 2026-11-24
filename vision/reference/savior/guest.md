# Guest Linux

**After this:** you can name the Linux module that is the actuator.
**First read:** occupancy + diagrams 1–5. **Next:** [`security.md`](security.md).

Savior’s guests are ordinary ARM64 Linux. This note is **what they
run that Savior cares about**: occupancy actuator, ACRN/VHM
frontend, vtee/TIPC, and the Mali arbiter client. CFS, binder,
and Android HAL stay out.

Read [`system.md`](system.md) for the communication mesh,
[`scheduler.md`](scheduler.md) for occupancy + hint peek,
[`vhm.md`](vhm.md) for SOS→UOS, [`tee.md`](tee.md) for SMC,
[`gpu.md`](gpu.md) for SWA, [`devices.md`](devices.md) for virtio.

ACRN HSM and vtee **are** in this tree
(`linux/drivers/virt/acrn/`, `linux/drivers/nebula/`). Occupancy
and the Mali arbiter client also exist as Yocto/ALPS extras
(`nebula_sched`, `mali_kbase_arbiter_swa.c`). The contracts they
implement (shared page, SMC, DTS) are in Savior.

## Principle

1. **Savior never schedules a guest task.** A guest CPU is a
   pinned host thread. Occupancy is “may this Linux CPU run”;
   hint peek is “raise that host thread”. The guest module is an
   actuator, not a second scheduler.

2. **SOS is the only ACRN client.** `/dev/acrn_hsm` issues
   `SMC_SC_NBL_VHM_REQ`. UOS does not. tbox does not.

3. **TEE from a guest is `smc`, not `hvc`.** vtee sets
   `SMC_TEE_SECURE_ID`. HYP FIDs use `SMC_HYP_SECURE_ID` in
   `x7`. Savior overwrites `x7` with VMID on TEE forward.

4. **Doorbells are virtual IRQs into VCPU 0.** Occupancy
   (`sched_irq`), cross-VM lock, GPU GRANT/STOP: the guest
   handler runs on that Linux CPU. The host VCPU thread stays
   READY.

## Framework

```text
 SOS Linux                         UOS Linux              tbox Linux
  CFS / FIFO                        same                   same
  nebula_sched  (occupancy)         nebula_sched           nebula_sched
  mali_kbase + SWA client           mali_kbase + SWA client (tbox: no SWA)
  /dev/acrn_hsm  (VHM FE)           virtio FE of SOS BE    virtio-console
  vtee / TIPC                       vtee                   —
  PSCI cpuidle                      PSCI                   PSCI
         │ SMC / doorbell / virtio
         ▼
 Savior  VCPU thread + stage-2
```

## Diagrams

### 1. What each guest runs that Savior sees

```text
              SOS (Yocto)              UOS (ALPS)              tbox
 occupancy    nebula_sched             nebula_sched            nebula_sched
              PAUSE parks CPU          PAUSE / OFFLINE unplug  PAUSE = nop
 ACRN         /dev/acrn_hsm            —                       —
 Mali SWA     mali_kbase_arbiter_swa   same                    no /arbiter
 TEE          vtee + TIPC              vtee                    —
 virtio       vsock, block, …          FE of SOS BE            hvc0 console
 PSCI         CPU_ON/OFF/SUSPEND       same                    same
              RESET / RESET2           same                    same
```

### 2. Occupancy doorbell (guest view)

Host writes RUN/PAUSE/SHARE/OFFLINE and rings `sched_irq` on
that VM’s VCPU 0 (VIRTUAL). The module parks the **Linux** CPU.
The host VCPU thread is not `thread_sleep`’d.

```text
 Savior  occupancy table changed
    write shm occupancy[this VM][0..7]   (no dmb here; IPI `smp_mb` — [`shm.md`](shm.md))
    Interrupt(sched_irq)  VCPU 0 VIRTUAL
         │
         ▼
 guest  nebula_sched_irq_handler
    nebula_sched_apply_policy
       for each of this VM's CPUs
          RUN / SHARE  →  unpause, schedule here
          PAUSE        →  pause, CFS/RT skip this CPU
          OFFLINE      →  SOS: unpause
                          UOS: unplug
                          tbox: PAUSE is a nop
```

Hint peek is a **different** contract, once at boot:

```text
 guest  HC_GUEST_SET_SCHED_PARAM
    VM_MEM_INFO            PHYS_OFFSET, PAGE_OFFSET, PAGE_END, kimage_voffset
    VM_PREEMPT_COUNT_OFF   offsetof(thread_info, preempt.count)
    VM_POLICY_OFF          offsetof(task_struct, policy)
    COMPLETE
 Savior maps guest RAM RO, AddSchedHint peeks on every exit
```

Layout and host side: [`scheduler.md`](scheduler.md) **nebula_sched**.

If the module is missing: `AddSchedHint` cannot see
`preempt_count` / `policy`; occupancy doorbells are ignored; two
FIFOs can sit on one SHARE core.

**Probe** (`compatible = "grt,nbl_sched"` / `"mediatek,nbl_sched"`):

```text
 reserved-mem ioremap → nebula_sched_info*
 apply_policy once
 request_threaded_irq  ONESHOT  (lua setSchedIrq)
 debugfs /sys/kernel/debug/nebula_sched/{pcpu,policy,period,budget,priority,timeslice,rt_mask}
 misc  nbl_custsched/{period,budget,priority}
 HC_GUEST_SET_SCHED_PARAM  MEM_INFO / PREEMPT_COUNT_OFF / POLICY_OFF / COMPLETE
```

**Shared page** (`nebula-sched.h`, host writes occupancy):

```text
 pcpu_utils[8]
 alps_vcpu_stats[8] / yocto_vcpu_stats[8]
 vcpu_policies[6][8]     RUN=0 PAUSE=1 OFFLINE=2 SHARE=3
 sched_ctrl.policy       NONE / 3_5 / 8_8_DYNAMIC
 sched_ctrl.vm_suspend[6]
 rt_info.bind_pcpu[6][8]
 rt_info.rt_status[6][8]
```

**Apply** (`core_ctl_force_pause_cpu`, Yocto `nebula-sched.h`):

| Policy | Guest CPU |
| --- | --- |
| RUN / SHARE | unpause |
| PAUSE | pause (`cpu_pause_mask`) |
| OFFLINE | unpause only (Yocto). ALPS copy also `remove_cpu` |

Suspend: set `vm_suspend[VM_ID]`, disable IRQ, unpause all.
Resume: apply_policy, clear suspend, enable IRQ.

RT: `update_rt_status` on CFS switch writes `rt_status`;
`get_rt_mask` clears CPUs where **another** VM is FIFO.
`VM_ID` in `common.h` is the kernel VMID of **this** guest
(SOS=1). Debugfs `policy` writes `sched_ctrl.policy` (host
reads it ≥ 8 ms).

### 3. ACRN ioctl → SMC (SOS only)

```text
 userspace  ioctl /dev/acrn_hsm
    CREATE_VM / START_VM / SET_IOREQ / NOTIFY_REQUEST_FINISH / …
         │
         ▼
 linux  drivers/virt/acrn/hsm.c
    acrn_hypercall5(hcall_id, p1..p5)
       r7  = SMC_HYP_SECURE_ID << 16
       smc  SMC_SC_NBL_VHM_REQ, hcall_id, p1..p5, r7
         │  HCR.TSC
         ▼
 Savior  packet VHM_REQ → SOS Vmm     (vhm.md)
```

Kick SPI (QEMU 254 / SoC 287) is a **physical** IRQ into SOS
(`setVhmIrq`). HSM scans PENDING slots on that IRQ.

UOS sees virtio/PCI BARs that SOS emulates; it has no
`/dev/acrn_hsm`.

### 4. Mali arbiter client (SOS / UOS)

Guest DTS node is **injected by the VMM**, not a board file.
tbox does not get `/arbiter`.

```text
 /arbiter  compatible = "mali,sw-arbiter"
    GRANT / STOP / LOST / RCVR     virtual
    MALI-JOB/MMU/GPU/EVT/PWR       physical, only while granted

 guest  mali_kbase_arbiter_swa.c
    REQUEST GPU
       arm_smccc_smc(SMC_FC_NBL_VHM_REQ, FC_ID_GPU_ARBITER=0x3,
                     ARB_CMD_GPU_REQUEST, arg0, arg1, 0, 0, r7)
         │
         ▼
    Savior GpuScheduler::HandleSmc     (gpu.md F9)
         │
         ▼  GRANT IRQ (virtual)
    arbiter_grant_irq_handler
       queue grant_kwork on RT kthread_worker
       program GPU, SMC ACTIVE
    STOP IRQ
       drain, SMC STOPPED
```

SMC is `smc`, despite the source comment saying HVC. Guest `hvc`
returns `x0 = -1`.

**IRQ bottom halves** (`mali_kbase_arbiter_swa.c`):

```text
 GRANT/STOP  hard IRQ  →  kthread_queue_work  mali_arb_irq
                          SCHED_FIFO 49  (below default IRQ thread 50)
                          same worker ⇒ hyp send order preserved
 RCVR        hard IRQ  →  queue_work mali_arb_wq HIGHPRI
                          + arb_vm_recovery_arm

 grant_kwork_fn
    gpueb_swa_grant_setup     open GPUEB IPI
    arb_vm_gpu_granted        KBase takes MMIO
 stop_kwork_fn
    swa_commit_stop           drain, then SMC STOPPED
 recovery_worker
    gpueb_swa_grant_setup     same IPI as GRANT (else gpufreq no-op)
    arb_vm_gpu_recovery
    flush vm_recovery_work
    later  SMC RECOVERY_DONE
```

Fault inject (`MALI_SWA_FAULT_INJECT`) can drop GRANT/STOP/RCVR
IRQs or outbound HVCs. Not on product builds.

### 5. Cross-VM lock (userspace mutex, not LK)

Shared DRAM mutexes for SOS/UOS/tbox userspace (modem, display,
…), **not** the LK scheduler. Layout: 16 packed `uint32_t`s
(`LOCK_ID_START..MAX-1`).

```text
 bits  7:0   lock_id
      15:8   cnt     LOCK=1 / UNLOCK=0
     23:16   create_vmid  bitmask  YOCTO=1 ALPS=2 TBOX=4
     31:24   current_vmid  kernel VMID of holder
```

Take / release is guest `ldaxr`/`stlxr`. Host does not
participate except destroy-cleanup.

```text
 guest A  ldaxr slot
    empty?  stlxr  cnt=LOCK, current_vmid=A
    held by A  (recursive)  or fail
 guest B  waits / retries in userspace
    unlock  stlxr  cnt=UNLOCK
         │
         ▼  VM A destroyed
 Savior  cross_vm_lock_check(vmid)
    for id 1..15
       ldar slot
       holder == dying?  UNLOCK, drop bit from create_vmid
       else create_vmid has bit?  clear the bit only
       stlxr
    inject cvm_lock_irq into remaining creators  (VCPU 0, VIRTUAL)
```

VMM: `zx_guest_set_cvm_lock_vmo` (one shared VMO) and
`zx_guest_set_cvm_lock_irq` (SoC SPI **985** all three guests).
Missing VMO → `cross_vm_lock_check` returns; missing IRQ →
inject logs and skips.

### 6. PSCI the guest issues

```text
 guest  smc  PSCI64_*
    CPU_ON      missing VCPU → STARTUP packet → Vmm StartVcpu
                already OFF  → ResumeThread + ResetEl2State
    CPU_OFF     park this VCPU thread
    CPU_SUSPEND s2idle → WaitForSuspend  (pm.md)
    SYSTEM_RESET     ZX_ERR_CANCELED → Vmm relaunch
    SYSTEM_RESET2    GUEST_VCPU_STOP → same relaunch
    SYSTEM_OFF       ZX_ERR_UNAVAILABLE → no relaunch
    SYSTEM_SUSPEND   STR  (pm.md)
```

## Occupancy (`nebula_sched`)

See diagram 2. Host writes RUN/PAUSE/SHARE/OFFLINE into shared
DRAM and rings `sched_irq` on that VM’s VCPU 0. Detail and
layout: [`scheduler.md`](scheduler.md) **nebula_sched**.

## Cross-VM lock

See diagram 5.

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

See diagram 4. Protocol: [`gpu.md`](gpu.md). tbox does not get
this node.

## guest-call (`HC_GUEST_*`)

Same `SMC_SC_NBL_VHM_REQ` as ACRN, `HC_ID` `0xB0`. Handled in
**this** guest’s Vmm (`HandleGuestCall`), not `VhmServiceImpl`.
Full table: [`system.md`](system.md) **guest-call**.

What the Linux modules actually issue:

| Guest | Typical `HC_GUEST_*` |
| --- | --- |
| All with WDT | `WDTK_KICK` / `SUSPEND` / `RESUME` |
| `nebula_sched` | `SET_SCHED_PARAM` (hint offsets) |
| SOS HSM | ACRN `0x80`, not this family |
| SOS RTC | `SET_RTC` (`zx_clock_adjust`, SOS only) |

## PSCI the guest issues

| Guest call | Savior |
| --- | --- |
| `CPU_ON` / `OFF` / `SUSPEND` | [`cpu.md`](cpu.md) |
| `SYSTEM_RESET` / `RESET2` | [`vmm.md`](vmm.md) |
| `SYSTEM_OFF` | all VMs `ZX_ERR_UNAVAILABLE` (VMM SHUTDOWN) |
| cpuidle s2idle | [`pm.md`](pm.md) once all VMs park |

## virtio frontends

Ordinary Linux virtio. Backends in that guest’s Vmm (or SOS for
UOS). Console `hvc0` is tbox. vsock IRQs 380/382 are SOS lua.
Block/RPMB/cluster/eint/rtc are feature bits, **off on QEMU**
(`vmm.cc` `is_qemu_platform()` skips several).

## Pitfalls

- **No `nebula_sched`.** Occupancy and hint peek degrade silently.
- **ACRN ioctl from UOS.** There is no handler; UOS is the FE
  of SOS, not a hypervisor client.
- **`hvc` to Savior.** `x0 = -1`. Use `smc`.
- **Mali without `/arbiter`.** Driver never SMCs; GPU stays
  Ungranted for that VM.
- **Occupancy PAUSE ignored.** Guest still `eret`s; Savior will
  run it. Occupancy is cooperative.

## Terminology

| Term | Meaning here |
| --- | --- |
| Actuator | Guest module that parks a CPU or yields the GPU |
| HSM | ACRN Hypervisor Service Module in SOS |
| Injected DTS | VMM writes `/arbiter` into the guest tree at launch |
| `SMC_HYP_SECURE_ID` | r7 for VHM/GPU/vmlog SMCs |
| Doorbell | Virtual IRQ into VCPU 0 (occupancy, lock, GRANT) |

## Code

| Step | File |
| --- | --- |
| Occupancy contract | [`scheduler.md`](scheduler.md), `zircon/kernel/include/kernel/sched.h` |
| Occupancy IRQ | `yocto/.../nebula_sched/nebula_sched.c` `nebula_sched_irq_handler` |
| Cross-VM lock | `zircon/kernel/arch/arm64/hypervisor/cross_vm_lock.cpp` |
| ACRN HSM | `linux/drivers/virt/acrn/` |
| Legacy VHM | `linux/drivers/vhm/` |
| SMC wrapper | `linux/arch/arm64/include/asm/acrn.h` |
| vtee | `linux/drivers/nebula/vtee.c` |
| TIPC in guest | `linux/drivers/nebula/trusty-ipc.c` |
| Arbiter DTS inject | `garnet/bin/guest/vmm.cc`, `main_uos.cc` `add_arbiter_dts` |
| Arbiter client | `mali_kbase_arbiter_swa.c` `arbiter_hvc_call` |
| virtio BE | `garnet/bin/guest/vmm.cc` `Vmm::Initialize` |

---

Prev: [`tee.md`](tee.md) · Next: [`security.md`](security.md) · [`README.md`](README.md)

Everyday's a Hustle.
