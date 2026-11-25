# System

**After this:** you can point at every process, every shared page,
and every doorbell, and say which plane note owns the detail.
**First read:** runtime picture, communication table, SMC classes.
**Prev:** [`fidl.md`](fidl.md). **Next:** [`flows.md`](flows.md).

The other notes are **planes**. This one is the machine they sit
in: who is running, who talks to whom, what is shared DRAM, and
which loops keep the product alive.

Read [`README.md`](README.md) for the five facts and Two configs,
[`glossary.md`](glossary.md) for words, [`flows.md`](flows.md) for
numbered walks.

## Principle

1. **One host microkernel, three guest kernels, one TEE.**
   Savior is not “Linux plus a hypervisor module”. Guests are
   ordinary Linux. The host is Zircon: objects and `zx_*`,
   policy in EL0 ([`microkernel.md`](microkernel.md)). Guests
   `eret` into that host.

2. **Userspace is per-guest.** `guest_manager` is one process.
   Each guest is another process (`file://sos`, `file://uos`,
   tbox). Kernel `Guest` / `Vcpu` die with that process.

3. **Guests never call each other in EL1.** SOS↔UOS is VHM,
   virtio-be, mailbox, vsock, occupancy shm, or a virtual IRQ
   Savior injects. TEE is an SMC Savior forwards. There is no
   guest–guest `eret`.

4. **Almost every “hypercall” is one SMC class.** `HCR.TSC`
   traps it. Savior either handles it, posts a packet to that
   guest’s Vmm, or forwards it to EL3. Guest `hvc` returns
   `x0 = -1`.

## Runtime picture

```text
 EL3   BL31
 S-EL1 BL32 TEE
                              ┌─ idle thread  (may ATF if gate=SUSPEND)
 EL1h  Savior kernel ─────────┼─ VCPU threads  SOS×N  UOS×N  tbox×N
       stage-2, VGIC ICH_LR,  └─ DPC / IRQ threads
       GpuScheduler, occupancy
              │ zx_* / packets
 EL0   sysmgr
         GuestManager                 catalog + GRT WDT reporter
           Vmm  file://sos            lua, VGIC, virtio, VhmService, WDT
           Vmm  file://uos            VhmDevice, GPU client, WDT
           Vmm  tbox                  console, WDT
              │ eret guest EL1h
 EL1h  SOS Linux     Yocto     nebula_sched, acrn_hsm, mali SWA, vtee
       UOS Linux     Android   nebula_sched, mali SWA, virtio FE
       tbox Linux              nebula_sched (PAUSE nop), virtio-console
```

User vmid vs kernel VMID: SOS −1/1, UOS 0/2, tbox 1/3.

## Objects (one Guest)

```text
 GuestManager.Guest[user vmid]
    Vmm process
       machina::Guest
          zx_guest_create_new     kernel Guest + guest VMAR (S2)
          Vcpu[0..n]              zx_vcpu_create, pin, Resume loop
          GicDistributor/GicIts   trap GICD/GICR, passthrough SPIs
          virtio-* / vSMMU / vsock
          VhmServiceImpl          SOS only
          VhmDevice               UOS only
          Watchdog                HC_GUEST_WDTK_KICK
              │
              ▼
 kernel Guest
    gpas_     TYPE_GUEST_PHYS   VTTBR
    traps_    MMIO holes
    vcpus_[8]
    vmid_     1 / 2 / 3
 kernel Vcpu
    thread_   THREAD_FLAG_VCPU, one pCPU
    el2_state_  tpidr_el2 in guest
    gich_state_ bitmap + ICH_LR
    vcpu_group_  WITH_CROSS_VM_SCHED
```

Detail: [`vmm.md`](vmm.md) lifecycle, [`cpu.md`](cpu.md) VCPU,
[`memory.md`](memory.md) S2.

## Communication

How one world reaches another. Numbers: QEMU vs SoC in
[`README.md`](README.md) **Two configs**.

| Path | From → to | Transport | Note |
| --- | --- | --- | --- |
| Guest enter / exit | Vmm VCPU thread ↔ guest EL1h | `HVC #4` / `eret` | [`cpu.md`](cpu.md) F2 |
| PSCI / idle / ON | guest → kernel | SMC | [`cpu.md`](cpu.md) [`pm.md`](pm.md) |
| ACRN `HC_*` | SOS Linux → SOS Vmm | SMC `SMC_SC_NBL_VHM_REQ` → packet `VHM_REQ` | [`vhm.md`](vhm.md) |
| UOS BAR emulate | UOS EL1 → SOS Linux | stage-2 trap → ioreq slot → kick SPI | [`vhm.md`](vhm.md) F7 |
| MSI | SOS → UOS | `HC_INJECT_MSI` → channel → GicIts / GICD | [`interrupt.md`](interrupt.md) |
| Fast IRQ inject | any guest | `SMC_FC_NBL_VHM_REQ` `INJECT_HW/VIRT_IRQ` | kernel, no Vmm |
| GPU SWA | SOS/UOS mali → kernel | same fast SMC `FC_ID_GPU_ARBITER` | [`gpu.md`](gpu.md) |
| Occupancy | kernel → guest `nebula_sched` | shm + `sched_irq` on VCPU 0 VIRTUAL | [`scheduler.md`](scheduler.md) |
| Hint peek | guest RAM → kernel | `SchedParams` mapped RO | [`scheduler.md`](scheduler.md) |
| Cross-VM lock | SOS/UOS/tbox userspace | packed `uint32_t` shm + virtual IRQ | [`guest.md`](guest.md) |
| ipc-mbox | guest ↔ peer VM | 8 KiB TX/RX VMOs + `HC_IPC_*` + virtual SPI | this page **IPC** |
| vsock (GzFS) | SOS Linux ↔ SOS Vmm | virtio-vsock CID 10→2 port 5022 | this page **IPC** |
| vsock (vhost) | UOS FE ↔ SOS vhost | virtio rings + odd/even SPI pairs | this page **IPC** |
| Event IRQ | SOS → UOS/tbox | SMC `HYPERVISER_GUEST_OS_IRQ_EVENT` | this page **IPC** |
| rproc / rpmsg | UOS host ↔ SOS remote | reserved mem + `FC_ID_INJECT_HW_IRQ` | this page **IPC** |
| TIPC / vtee | guest ↔ HEE Trusty / TEE | virtio + `vcpu_kick` / NS IRQ | [`tee.md`](tee.md) |
| vmlog | guest → host log | `FC_ID_NOTIFY_VMLOG` / `_SINK` | [`logging.md`](logging.md) |
| Watchdog kick | guest → its Vmm | `HC_GUEST_WDTK_KICK` (guest-call, not ACRN) | [`vmm.md`](vmm.md) |
| GRT WDT | GuestManager → SoC WDT | reporter 10 s / miss×3 | [`vmm.md`](vmm.md) |
| TEE forward | guest SMC → BL32 | `TeeSmcHandler`, `x7 = vmid` | [`tee.md`](tee.md) |
| Trusty-in-HEE | leftover HYP stdcall | `sm_handle_smc_call` | this page **SMC** |
| virtio-rpmb | guest SG_IO → Vmm → host UFS LU | host virtio id 56, no HMAC in Savior | [`tee.md`](tee.md) F19 |

SOS is the only `/dev/acrn_hsm` client. UOS never issues `HC_*`.

## Shared DRAM

Identity maps unless noted. Not a second interconnect — just
pages both sides agreed on.

| Region | Who | Role |
| --- | --- | --- |
| Guest RAM | that VM’s S2 | Linux + DTB + payloads |
| Occupancy shm | host + all `nebula_sched` | RUN/PAUSE/SHARE, util, RT bits |
| Cross-VM lock VMO | host + SOS/UOS/tbox | 16 packed locks, destroy cleanup |
| VHM ioreq | SOS GPA, UOS Vmm maps it | 4 KiB, 16×256, `processed@136` |
| GPU cmdbuf | IPA `0x88000b000` | 4 KiB paged VMO; guest CACHED, kernel UNCACHED ([`shm.md`](shm.md)) |
| ipc-mbox channels | IPA `0x800000000 + id×8KiB` | 4 KiB TX + 4 KiB RX per way |
| rproc reserved | product `sos_tboxuos_rproc_reserved` / `rproc_reserved_sostouos` | virtio rings; code default name `rproc_reserved` only if pb missing |
| GPUEB shared | ALPS + host | magic on first ALPS ACTIVE |
| gz_nebula | HEE image; optional guest RO | ramconsole live ring; dump window |
| vSMMU CMDQ | guest RAM, Vmm consumes | doorbell trapped |

## SMC classes

One intercept, four doors. Always `next_pc` first.

```text
 Guest  smc
    HCR.TSC  →  handle_smc_instruction
       │
       ├─ ENTITY_NEBULA && HYP_SECURE_ID
       │     SMC_SC_NBL_VHM_REQ     packet VHM_REQ → that Vmm
       │        x1 ACRN HC_*        SOS VhmServiceImpl
       │        x1 HC_ID_GUEST_CALL Watchdog / sched param / ktrace
       │        x1 HC_ID_IPC        ipc-mbox
       │     SMC_FC_NBL_VHM_REQ     kernel fast path (IRQ / GPU / vmlog)
       │     else                   sm_handle_smc_call  (Trusty-in-HEE)
       │
       ├─ PSCI64_* / HYPERVISER_GUEST_*
       │     CPU_ON/OFF/SUSPEND, RESET/RESET2/OFF, SYSTEM_SUSPEND
       │     GUEST_OS_RESUME / PM_STATE / CPUFREQ
       │     IRQ_EVENT 0xF0000003   inject SPI into target VCPU 0
       │     UNLOCK    0xF0000006   cross-VM lock IRQ
       │
       └─ else  TeeVendor::TeeSmcHandler     F14
```

`SMC_SC_NBL_VHM_REQ` is **one packet** for three userspace
families. Kernel does not parse `x1` beyond posting `VHM_REQ`.
That guest’s Vmm `HandleNebulaHyperCall` switches on
`HC_ID(x1) = (x1 >> 24) & 0xff`:

```text
 x1[31:24]
    0x80  ACRN HC_*          SOS VhmServiceImpl only
          (CREATE/START/NOTIFY/MSI/…)     [`vhm.md`](vhm.md)
    0xA0  HC_IPC_*           IpcService  (F18)
    0xB0  HC_GUEST_*         this Vmm  (watchdog, sched, ktrace)
    else  FXL_CHECK          unhandled
```

UOS has no `VhmServiceImpl`. A UOS `0x80` `HC_*` `FXL_CHECK`s.
UOS **does** handle `0xB0` (its own watchdog / sched param).

Illegal HYP parameters inject a guest IABT, they are not
forwarded.

**FID numbers.** Entity 52, fn 256:
`SMC_SC_NBL_VHM_REQ = 0x34000100`, `SMC_FC_NBL_VHM_REQ = 0xB4000100`.
`x7[31:16]`: 0 TEE, 1 HYP. [`cpu.md`](cpu.md).

**Fast path** `SMC_FC_NBL_VHM_REQ` (`x1 = FC_ID_*`) never
leaves the kernel. `INJECT_HW_IRQ` is `interrupt_set_pending` on
the **host** GIC (passthrough line). `INJECT_VIRT_IRQ` is
`Vcpu::Interrupt` on target VM VCPU 0. Args: HW IRQ `x2`; virt IRQ `x2=vmid,x3=vec`;
GPU `x2=cmd,x3,x4`; vmlog sink `x2=vq`. [`cpu.md`](cpu.md) SMC.

**`HYPERVISER_GUEST_*`** (`0xF0000000+`) are kernel-handled
Savior SMCs (resume / PM / cpufreq / event IRQ / ringbuf /
unlock / tbox boot count / hint budget). Full table:
[`cpu.md`](cpu.md).

### guest-call (`HC_ID` `0xB0`)

Same SMC, this guest’s Vmm. `x0` result; extra outs in `x1`/`x2`.

| `x1` | Name | Action |
| --- | --- | --- |
| `0xB0000000` | `PATCH_DEVICE_TREE` | Vmm patches guest DTB GPA |
| `0xB0000001` | `GET_MEMORY_INFO` | `x1/x2` = phys_base / size |
| `0xB0000002` | `GET_FRAMEBUF_INFO` | `framebuffer` VmemSpec HPA/size |
| `0xB0000003` | `VCPU_SET_TIME_SLICE` | host slice on `cpu_mask` |
| `0xB0000004` | `VCPU_SET_REALTIME` | this VCPU thread FIFO |
| `0xB0000005` | `VCPU_GET_TIME_SLICE` | |
| `0xB0000006` | `START_KTRACE` | license `TRACE_TOOL` then ioctl |
| `0xB0000007` | `VCPU_GET_REALTIME` | |
| `0xB0000008` | `WDTK_KICK` | `Watchdog.Heartbeat` |
| `0xB0000009` | `WDTK_SET_RST_STATUS` | GRT WDT ioctl |
| `0xB000000A` / `B` | `WDTK_SUSPEND` / `RESUME` | pause timeout + ioctl |
| `0xB000000C` / `D` | `SET/GET_SCHED_PARAM` | hint peek offsets ([`scheduler.md`](scheduler.md)) |
| `0xB000000E` | `START_TEST_WDT` | |
| `0xB000000F` | `START_SMMU` | host SMMU ioctl |
| `0xB0000010` | `CVMD_CMD` | cross-VM dump |
| `0xB0000011` / `12` | `SET/GET_RTC` | SOS may `zx_clock_adjust` |
| `0xB0000013` | `DEINIT` | ramfb notify + passthrough disp IRQ |
| `0xB0000014` / `18` | `STOP_SPI_DMA` / `STOP_I2C_DMA` | host driver ioctl |
| `0xB0000015` | `HEART_BEAT` | defined; unused in switch |
| `0xB0000016` | `KTRACE_SNAPSHORT_CTL` | |
| `0xB0000017` | `PATCH_DEVICE_TREE_MD` | metadata DTB |

### ipc-mbox cmds (`HC_ID` `0xA0`)

`x2` = channel id, `x6` = way (`0xFFFF0000` = self, else peer
map). [`flows.md`](flows.md) F18.

| `x1` | Name |
| --- | --- |
| `0xA0000000` / `1` | `SET_RX_READY` / `SET_TX_DONE` |
| `0xA0000002` / `3` | `GET_*_STATUS` |
| `0xA0000004` / `5` | `CLEAR_*_STATUS` |
| `0xA0000006`–`9` | `VM_CALL` / `WRITE` / `SET_VM_SERVER_STATE` / `READ` |
| `0xA000000A`–`C` | mailbox suspend / resume / power state |

## IPC (mailbox / event / vsock / rproc)

Four different “talk to the other VM” stories. Do not mix them
with VHM ioreq.

**ipc-mbox.** lua `setIpcMbox(rx_ready, tx_done)`. DTS interrupt
order is **tx_done then rx_ready**. Channel GPA
`0x800000000 + id × 8 KiB` (4 KiB TX + 4 KiB RX). Doorbells are
`HC_IPC_*` on `SMC_SC_NBL_VHM_REQ`. UOS has no lua pairs: it is
an `IpcClient` of SOS `IpcServiceImpl` (user vmid 0 → way 0).

```text
 guest A  write TX page
    SMC  HC_IPC_SET_RX_READY / VM_WRITE
       packet VHM_REQ → IpcService
       inject rx_ready VIRTUAL → peer
 guest B  IRQ, copy RX
    SMC  HC_IPC_SET_TX_DONE
       inject tx_done VIRTUAL → A
```

Swimlane: [`flows.md`](flows.md) F18.

**GzFS vsock** (SOS fuse → SOS Vmm CID 10→2 port **5022**, host
`/data`). Framing (`gzfs_vsock_proto.h`):

```text
 request   opcode, seq, length
 response  opcode, seq, result, length
 OP_GETATTR / READDIR / OPEN / READ / STATFS
 path ≤ 1024, READ data ≤ 64 KiB
```

Not a Linux virtio-fs. Guest mounts a fuse that talks this
protocol over vsock.

| | QEMU SOS | SoC SOS | SoC tbox | SoC UOS |
| --- | --- | --- | --- | --- |
| ways | 150/151 | 284/285, 277/278, 279/280 | 284/285 | from SOS `GetIrqInfo` |

**Event IRQ.** lua `setEventIrq`. Guest
`arm_smccc_smc(HYPERVISER_GUEST_OS_IRQ_EVENT, from, to, irq)`
(`0xF0000003`) → `handle_irq_event` → target VCPU 0 VIRTUAL.
SoC: SOS sends **989, 990** to UOS and tbox (`powerkey`,
`dispshow` / `nebula_event_test`). QEMU: not configured.

**vsock, two planes.**

1. **GzFS.** SOS Linux fuse ↔ SOS Vmm `GzFsVsockService`. Host
   CID 2, guest CID 10, port 5022. Serves host `/data`.
2. **vhost.** UOS virtio-vsock FE ↔ SOS `vhost_vsock`. SoC IRQ
   pairs: SOS odd **181,183,…191** (BE kick), UOS even
   **182,184,…192** (FE kick). QEMU SOS only **380, 382**. tbox
   QEMU has no vsock.

**rproc / rpmsg.** UOS is host (user vmid 0), SOS is remote
(−1). Kick is `FC_ID_INJECT_HW_IRQ`. Loaded from
`/system/data/rproc.pb.txt`. Product indep (two devices):

| | tbox↔UOS | SOS↔UOS |
| --- | --- | --- |
| `host_vmid` / `remote_vmid` | 1 / 0 | 1 / −1 |
| `ctrl_irq` | 226 | 221 |
| `local_irqs` | 227×20 | 222×2 + 201×10 |
| `remote_irqs` | 247×20 | 224×2 + 211×10 |
| `resv_mem` | `sos_tboxuos_rproc_reserved` | `rproc_reserved_sostouos` |
| IPI affinity | both true | both true |

FIDL `RprocDeviceSvc.GetInfo` returns shm VMO + those IRQs.
Code defaults (ctrl 200, 201–240 / 241–280, `rproc_reserved`)
apply only if the pb is missing.

**Cross-VM lock** is userspace mutexes on a shared VMO (SoC IRQ
**985** all three guests), not this mailbox. [`guest.md`](guest.md).

**TIPC vs vtee IRQs** (do not swap stacks):

| | QEMU SOS | SoC SOS | SoC UOS | SoC tbox |
| --- | --- | --- | --- | --- |
| TIPC vq (HEE) | 245 | 286 | — | — |
| vtee (BL32) | 513 | 174 | 179 | 173 |

**Occupancy doorbell** (`sched_irq`, VCPU 0 VIRTUAL): QEMU none;
SoC SOS/UOS **283**; SoC tbox **199**.

## Control loops

Five loops run for the life of the product. They do not share a
thread.

| Loop | Period / trigger | What it decides |
| --- | --- | --- |
| `zx_vcpu_resume` | every intercept | next `eret`, or a packet to Vmm | [`cpu.md`](cpu.md) F2 |
| Hint + occupancy | every exit / ~8 ms | host prio/slice; who may sit | [`scheduler.md`](scheduler.md) |
| GPU SWA | SMC + timers | exclusive Mali owner | [`gpu.md`](gpu.md) F9 |
| Host PM | last VM s2idle | freeze devices, idle ATF | [`pm.md`](pm.md) F11 |
| Watchdogs | 30 s VM / 10 s GRT | relaunch VM or SoC reset | [`vmm.md`](vmm.md) |

If a guest looks “dead”, ask which loop stopped: the VCPU thread
(no `eret`), occupancy (paused in shm), GPU (Ungranted), PM
(parked), or watchdog (Stop + relaunch).

## From reset to 3OS

One sitting, then the plane notes.

```text
 F1   reset → ATF → shim → lk_main → sysmgr → SOS eret
 F2   that eret is zx_vcpu_resume  (inner loop of everything)
 F8   SOS acrn_hsm CREATE/START → file://uos → UOS eret
 F13  lua / product setVmem filled S2 before either eret
 F5   passthrough SPIs already bound (lua setVgicIrq)
 F9   first mali REQUEST → GRANT  (SoC only; QEMU SWA off)
 F11  later: all VMs s2idle → host gate → idle ATF
 F12  guest reboot is Vmm relaunch, not BL2
```

Secondary CPUs: F10. TEE: F14. UOS BAR: F7.

## Trusty-in-HEE vs BL32

Two secure-looking worlds. Do not treat them as one.

| | HEE Trusty | BL32 TEE |
| --- | --- | --- |
| Where | TAs inside Savior (EL0/EL1 host) | S-EL1, started by SPD |
| Guest path | leftover `sm_handle_smc_call` after VHM/GPU | `TeeSmcHandler` (not HYP, not PSCI) |
| TIPC kick | `trusty::vcpu_kick` on AHYP | NS IRQ on TEE bootmode |
| `SOS_STATE` | — | SOS `El2State.guest_state` PA published at VCPU create |

[`tee.md`](tee.md).

## How to keep going

| Question | Open |
| --- | --- |
| Why didn’t we reach SOS? | F1, [`bootstrap.md`](bootstrap.md) |
| What happens on this `smc`? | this page **SMC**, then F3 |
| Why is this IPA a fault? | F4, [`memory.md`](memory.md) [`devices.md`](devices.md) |
| IRQ storm / lost SPI | [`interrupt.md`](interrupt.md) HW bit |
| SOS stole the core | [`scheduler.md`](scheduler.md) |
| UOS never appeared | F8, [`vhm.md`](vhm.md) |
| GPU black | F9, [`gpu.md`](gpu.md) |
| Guest reboot, no expdb | F12, [`logging.md`](logging.md) |
| Idle ATF killed GPUEB | F11, [`pm.md`](pm.md) |
| Isolation actually enforced | [`security.md`](security.md) |

---

Prev: [`fidl.md`](fidl.md) · Next: [`flows.md`](flows.md) · [`README.md`](README.md)

Everyday's a Hustle.
