---
name: zircon-bootstrap
description: >
  Use when changing Secular boot, ATF, Trusty BL32, lkboot BL33, linux-arm64-boot-shim, physboot, userboot, or QEMU -bios/-kernel/-initrd.
---

# Zircon Bootstrap: Firmware to zxsh

Stop at the first `zxsh` prompt.

## When to use

* Editing `tools/secular-boot/`, `sdk/atf`, `sdk/lk/app/lkboot`, `sdk/lk/app/trusty`.
* Changing `linux-arm64-boot-shim`, physboot, `userboot.next`, or `secular.zbi`.
* Debugging a hang before the `zxsh` prompt.

## When not to use

* MMU / TTBR after handoff — `memory.md`.
* Secondary CPU idle / PSCI after `lk_main` — `cpu.md`.

> [!NOTE]
> Console image: Zircon kernel + linux-arm64-boot-shim + BOOTFS + serial
> console + `zxsh`. The kernel is an EL2 VHE host.

## 1. Framework

Three worlds share the machine. Secure world is ATF EL3 plus Trusty
S-EL1. Normal world is lkboot at EL2, then the Zircon host.

```text
 QEMU virt  (secure=true, virtualization=true, GICv3)
   -bios   secular.bios          BL1 + FIP (BL31 / Trusty / lkboot)
   -kernel linux-arm64-boot-shim.bin     fw_cfg only; lkboot copies it
   -initrd secular.zbi                   fw_cfg only; lkboot copies it
              │
              ▼
 ┌────────────────────────────────────────────────────────────────┐
 │ ATF  BL1 → BL2 → BL31          EL3                             │
 │  SPD=trusty: ERET S-EL1 into TEE, SMC back, EL3 exit to BL33   │
 └────────────────────────────┬───────────────────────────────────┘
                              ▼
 ┌────────────────────────────────────────────────────────────────┐
 │ Trusty TEE  BL32   S-EL1  (app/trusty + lib/sm)                │
 │  lk_main → sm_sched_nonsecure  SMC_YC_NS_RETURN → EL3          │
 └────────────────────────────┬───────────────────────────────────┘
                              ▼
 ┌────────────────────────────────────────────────────────────────┐
 │ lkboot  BL33   EL2 VHE  (app/lkboot)                           │
 │  fw_cfg → Image @ 0x40200000, ZBI @ 0x44000000                 │
 │  patch /chosen linux,initrd-*,  br shim  x0 = DTB              │
 └────────────────────────────┬───────────────────────────────────┘
                              ▼
 ┌────────────────────────────────────────────────────────────────┐
 │ linux-arm64-boot-shim   (phys, PIC; MMU off at entry)          │
 │  LinuxbootEntry → _start → PhysMain                            │
 │  DTB → ZBI items (RAM, GIC, UART, PSCI)                        │
 │  identity MMU on, then ZbiBoot: SCTLR.M=0, br physboot         │
 └────────────────────────────┬───────────────────────────────────┘
                              ▼
 ┌────────────────────────────────────────────────────────────────┐
 │ physboot   (ZBI kernel item; MMU on; still phys executable)    │
 │  ArmDropToEl1: stay EL2 VHE on this QEMU                       │
 │  STORAGE_KERNEL BOOTFS → ELF "vmzircon"                        │
 │  load + relocate at kHandoffVirtualAddress                     │
 └────────────────────────────┬───────────────────────────────────┘
                              ▼
 ┌────────────────────────────────────────────────────────────────┐
 │ vmzircon   EL2 VHE (EL1-named sysregs), high VA                │
 │  lk_main → bootstrap2 → userboot_init                          │
 └────────────────────────────┬───────────────────────────────────┘
                              ▼
 ┌────────────────────────────────────────────────────────────────┐
 │ userboot   first user process (static PIE + vDSO)              │
 │  unpack STORAGE_BOOTFS                                         │
 │  userboot.next = libexec/component_manager+--boot              │
 └────────────────────────────┬───────────────────────────────────┘
                              ▼
   component_manager --boot → console-launcher → /boot/bin/sh (zxsh)
```

Three things change along this path. Each **switch node** below is one
of them (sometimes two at once).

| Dimension | Starts as | Ends as |
| --- | --- | --- |
| Exception level | EL3 (ATF) | EL0 (`zxsh`) via S-EL1, then EL2 VHE, then EL0 |
| MMU | off | TTBR1 kernel + TTBR0 user ([`memory.md`](memory.md)) |
| Address space | physical / identity | kernel high VA, then per-process user VA |
| Security state | Secure (EL3 / S-EL1) | Non-secure from lkboot onward |

## 2. Flow

Each box is a **switch node**: exception level, MMU, or address space
changes here.

```text
 qemu-system-aarch64
   -machine virt-9.2,secure=true,virtualization=true,gic-version=3
   -cpu max,sve=off,sme=off
   -bios secular.bios
   -kernel linux-arm64-boot-shim.bin
   -initrd secular.zbi
         │
         ▼  (1) BL1  EL3, flash
 copy RW to SRAM, load BL2 from FIP @ 0x40000, ERET BL2
         │
         ▼  (2) BL2  EL3, SRAM
 load BL31 / BL32 Trusty / BL33 lkboot from FIP
 patch DTB at 0x40000000 (PSCI nodes)
         │
         ▼  (3) BL31  EL3 runtime
 GIC, PSCI, Trusty SPD. Next NS image is BL33
         │
         ▼  (4) SPD → Trusty  EL3 → S-EL1 → EL3
 ERET BL32 @ 0x0e100000, wait for SMC_YC_NS_RETURN, restore NS
         │
         ▼  (5) EL3 exit → lkboot  EL2h
 SPSR EL2h. x0 = DTB @ 0x40000000. PC = 0x60000000
         │
         ▼  (6) lkboot  EL2 VHE
 fw_cfg copy Image + ZBI, patch /chosen, MMU off, br shim
         │
         ▼  (7) LinuxbootEntry  ("ARM\\x64" header)
 _start   DAIF masked, VBAR=0, .bss cleared, TPIDR/SP
         │
         ▼  (8) PhysMain (shim)
 DTB matchers → append ZBI items; identity MMU on
         │
         ▼  (9) ZbiBoot
 SCTLR.M/C/I = 0,  br physboot,  x0 = data ZBI
         │
         ▼  (10) physboot  EL2 VHE, MMU on
 ArmDropToEl1 stays EL2 (FEAT_VHE). identity TTBR0, then TTBR1
 load vmzircon at 0xffffffff00000000
         │
         ▼  (11) kernel handoff
 drop identity TTBR0, lk_main(PhysHandoff*)
         │
         ▼  (12) lk_main  (still CPU0, not idle yet)
 thread_init_early … kernel_init
 BecomeIdle; bootstrap2 on CPU0
         │
         ▼  (13) userboot_init
 Process "userboot", two handle-only channel msgs, eret EL0
         │
         ▼  (14) userboot → component_manager → zxsh
 BOOTFS → libexec/component_manager --boot
 console-launcher → /boot/bin/sh
         │
         ▼
 zxsh on kernel serial
```

| Node | EL | MMU | Who runs |
| --- | --- | --- | --- |
| (1) BL1 | EL3 | off / ATF tables | firmware ROM |
| (2) BL2 | EL3 | ATF SRAM tables | loader |
| (3)–(4) BL31 + SPD | EL3 ↔ S-EL1 | ATF EL3 tables | BL31, then Trusty |
| (5)–(6) lkboot | EL2 VHE | LK high VA, then off | BL33 bootloader |
| (7) shim `_start` | EL2 | off | PIC phys |
| (8) shim `PhysMain` | EL2 | identity on (TTBR0) | still phys-executable |
| (9) `ZbiBoot` | EL2 | off again | jump into physboot |
| (10) physboot | EL2 VHE | identity TTBR0, then TTBR1 | still phys-executable |
| (11)–(12) `lk_main` | EL2 VHE | TTBR1; TTBR0 `EPD0` | kernel, CPU0 then idle + `bootstrap2` |
| (13) `userboot_init` | EL2 → first `eret` EL0 | TTBR0 = userboot | first process |
| (14) | EL0 | that process's TTBR0 | `component_manager` → `zxsh` |

How each node is **loaded** and **switched**. Load is who copies or maps
the image. Switch is the instruction or syscall that changes EL, MMU, or
aspace.

| Node | Image lives | Who loads it | Switch |
| --- | --- | --- | --- |
| (1) BL1 | flash `0x0` (`bl1.bin` front of `secular.bios`) | CPU reset PC | still EL3 |
| (2) BL2 | FIP @ flash `0x40000` → SRAM | BL1 copy, `ERET` | EL3 → EL3 |
| (3) BL31 | FIP → SRAM | BL2 copy, `ERET` | EL3 runtime stays |
| (4) Trusty | FIP → secure DRAM `0x0e100000` | BL2 copy, SPD `ERET` | EL3 → S-EL1 → EL3 (`SMC_YC_NS_RETURN`) |
| (5) lkboot | FIP → NS DRAM `0x60000000` | BL2 copy, EL3 `ERET` | EL3 → EL2h |
| (6) Image + ZBI | fw_cfg → `0x40200000` / `0x44000000` | lkboot MMIO, `br` | EL2, MMU off |
| (7) `_start` | Linux Image header | PIC, already at PA | same EL2 |
| (8) `PhysMain` | same | identity map | EL2, MMU on |
| (9) physboot | ZBI kernel item | shim `ZbiBoot` `br` | EL2, MMU off |
| (10) `vmzircon` | `STORAGE_KERNEL` ELF | physboot `Load` + `br` high VA | EL2 VHE, TTBR1 |
| (11)–(12) `lk_main` | already at handoff VA | C `bl` | still EL2, TTBR0 off |
| (13) userboot | RODSO in kernel package | kernel maps, `eret` | EL2 → EL0, TTBR0 on |
| (14a) `component_manager` | BOOTFS `libexec/component_manager` | userboot ELF + `zx_process_start` | EL0 → EL1 syscall → EL0 |
| (14b) `console-launcher` | BOOTFS package | component_manager bootstrap realm | same |
| (14c) `zxsh` | `/boot/bin/sh` | `fdio_spawn` from console-launcher | same |

From (13) onward every process uses the same EL2 → EL0 machinery
(`ThreadDispatcher::Start` → `arch_enter_uspace` → `eret`). Only
userboot is special: the kernel maps it. Everyone else is loaded in EL0
via VMO + ELF.

## 3. Switch nodes

#### (1) Reset → BL1

`make run` ([`tools/secular-boot/run-qemu.sh`](../../../tools/secular-boot/run-qemu.sh))
execs `run-zircon` with:

```text
-machine virt-9.2,highmem-ecam=off,gic-version=3,virtualization=true,secure=true
-cpu     max,sve=off,sme=off
-bios    out/secular-boot-arm64/firmware/arm-trusted-firmware-qemu-bios/secular.bios
-kernel  linux-arm64-boot-shim.bin
-initrd  secular.zbi
-m 1024  -s 8
```

`sve=off,sme=off` is required: Trusty SPD cannot context-switch NS
SVE/SME (`CTX_INCLUDE_FPREGS`). Plain `-cpu max` traps in `vmzircon`.

`secular.bios` is BL1 concatenated with the FIP at 256 KiB
([`sdk/atf/plat/qemu/qemu/platform.mk`](../../../sdk/atf/plat/qemu/qemu/platform.mk)):

```text
cp bl1.bin secular.bios
dd if=fip.bin of=secular.bios bs=64k seek=4
```

The FIP holds BL31, Trusty `lk.bin` (BL32), and lkboot `lk.bin` (BL33).
QEMU `-kernel` / `-initrd` are **not** in the FIP; they live on fw_cfg
(`0x09020000`) for lkboot to copy.

```text
BL1_RO_BASE     = 0x00000000     flash  128 KiB
PLAT_QEMU_FIP   = 0x00040000     rest of flash0
SEC_SRAM        = 0x0e000000     1 MiB   (BL1 RW / BL2 / BL31)
SEC_DRAM        = 0x0e100000     15 MiB  (BL32 Trusty)
```

BL1 programs early GIC / UART, copies its `.data` into SRAM, loads BL2
just below BL31, and `ERET`s to BL2 still in EL3.

#### (2) BL2: load the three payload images

BL2 walks [`qemu_bl2_mem_params_desc.c`](../../../sdk/atf/plat/qemu/common/qemu_bl2_mem_params_desc.c).
With `NEED_BL32=yes` and `SPD=trusty` the chain is:

```text
BL31  →  BL32 (Trusty TEE)  →  BL33 (lkboot)
```

| Image | PA | Size window |
| --- | --- | --- |
| BL31 | top of secure SRAM ≈ `0x0e090000` | 448 KiB |
| BL32 Trusty | `BL32_BASE` = `0x0e100000` | 14 MiB (`MEMSIZE`) |
| BL33 lkboot | `NS_IMAGE_OFFSET` = `0x60000000` | from FIP |
| DTB | `PLAT_QEMU_DT_BASE` = `0x40000000` | 1 MiB window |

BL2 also `update_dt()`: opens the QEMU DTB at DRAM base, injects PSCI
CPU enable methods, packs it. `ARM_LINUX_KERNEL_AS_BL33=1` does **not**
mean the Linux Image is BL33. It only sets BL33's entry registers to
the Linux ARM64 protocol so lkboot sees `x0` = DTB.

#### (3) BL31: EL3 runtime

BL31 is linked at the top of trusted SRAM. `bl31_main`: platform setup
(console, GIC), `runtime_svc_init` (PSCI + Trusty SPD), then the SPD's
`bl32_init` hook, then EL3 exit to BL33.

BL31 stays resident for the rest of the boot: PSCI `CPU_ON` for
secondary Zircon CPUs, SMC into Trusty, FIQ forwarding. Zircon phys
does **not** run at EL3 (`AddressSpace::ArchInstall` panics; `ArmDropToEl1`
`udf`s).

#### (4) SPD → Trusty, then back

[`trusty_setup`](../../../sdk/atf/services/spd/trusty/trusty.c) peeks the
first instruction of BL32 (`MRS SCTLR_EL1` means AArch64), sets SPSR =
`EL1h` DAIF masked, `x0` = `TSP_SEC_MEM_SIZE`, and registers
`trusty_init` as `bl32_init`. On CPU0:

1. Save NS EL1 sysregs + SIMD.
2. `cm_init_my_context(SECURE)` with BL32 `ep_info`.
3. `ERET` S-EL1 into Trusty at `0x0e100000`.
4. Trusty `lk_main` (`LK_TRUSTY_TEE=1`, `WITH_SMP=1`, 8 CPUs). `lib/sm`
   starts `idle-ns-switch` threads. At `LK_INIT_LEVEL_LAST` CPU0's
   idle thread `sm_wait_for_smcall` issues `SMC_YC_NS_RETURN`
   (`0x3C000000`).
5. SPD sees a secure-world yield SMC, restores NS context.
   `trusty_init` returns to `bl31_main`.

Trusty stays resident in S-EL1. Later NS SMCs (UUID, PSCI finish on
APs) re-enter it. It must not program `CNTV` or steal NS IRQs as S-EL1
FIQs: those are Zircon's timer/IPI.

Secondary CPUs take the same Trusty path on `CPU_ON`
(`trusty_cpu_on_finish_handler`). lkboot itself is `WITH_SMP=0`; only
CPU0 runs BL33.

#### (5) EL3 exit → lkboot

`bl31_prepare_next_image_entry` picks the NS image: BL33 at
`NS_IMAGE_OFFSET`. `ARM_LINUX_KERNEL_AS_BL33=1` fills the Linux
protocol registers. EL2 is implemented, so SPSR is EL2h.

| Register / fact | Value |
| --- | --- |
| `x0` | physical address of the DTB (`0x40000000`) |
| `x1`–`x3` | 0 |
| PC | `0x60000000` (lkboot `lk.bin`) |
| SPSR | EL2h, DAIF masked |
| MMU / caches | EL3 tables left behind; NS starts MMU-off |

#### (6) lkboot: fw_cfg → Image + ZBI, chain-load

[`app/lkboot/lkboot.c`](../../../sdk/lk/app/lkboot/lkboot.c). `arm64_el2_boot`
stays at EL2 with VHE (`HCR_EL2.{RW,E2H,TGE}=1`, `ICC_SRE_EL2`), then
LK turns its own MMU on at a high VA.

`boot_linux_from_firmware()` is the only fw_cfg consumer
([`firmware.c`](../../../sdk/lk/app/lkboot/firmware.c), selectors
`KERNEL_SIZE/DATA` `0x08`/`0x11`, `INITRD_SIZE/DATA` `0x0b`/`0x12`):

```text
0x40000000  DTB (from ATF, 64 KiB used by lkboot)
0x40200000  linux-arm64-boot-shim     16 MiB max
0x44000000  secular.zbi               64 MiB max
0x60000000  lkboot itself
```

It writes `/chosen` `linux,initrd-start/end` to the ZBI PA, checks
Image magic `0x644d5241` at offset `0x38`, panics if `CurrentEL != 2`,
then `lkbootloader` → identity-map trampoline → `arm64_chain_load`:
SCTLR.M/C/I off, `x0` = DTB, `x1`–`x3` = 0, `br` shim.

QEMU may also dump `-kernel` into RAM at its own default
(`DRAM+0x80000`). The boot path ignores that copy. The shim runs from
the lkboot PA and finds the ZBI through `/chosen`, not through QEMU's
initrd placement.

#### (7) Linux Image header → `_start`

`linuxboot-header.S` sits in `.boot.header`, first bytes of the file
(`phys.ld`). Layout is the [Linux ARM64 Image header](
https://www.kernel.org/doc/html/latest/arm64/booting.html):

```text
offset 0     ccmp … / "MZ"   EFI magic lookalike, then b _start
offset 8     load offset     0
offset 16    image size      LINUXBOOT_SIZE  (_end − load, includes .bss)
offset 24    flags           0x4  (place anywhere, 2 MiB aligned)
offset 56    magic           "ARM\x64"
```

`_start` (`arch/arm64/phys/start.S`): sample ticks, `DAIFSet` all, zero
`VBAR_ELx`, invalidate D-cache over `.data`…`_end`, clear `.bss`
(`_edata`…`_end`, 16-byte), `TPIDR_EL1` = boot thread pointer, `SPSel=1`,
`sp` = `boot_stack + BOOT_STACK_SIZE`, `bl PhysMain`. MMU still off.

#### (8) Shim `PhysMain`: DTB → ZBI, identity MMU on

`linux-arm64-boot-shim.cc` (`enable_mmu = true`): `BootShimHelper`
walks the FDT (`x0`) and appends ZBI driver items the kernel cannot
discover by itself:

| ZBI / shim item | Source |
| --- | --- |
| RAM / `MEM_CONFIG` | `/memory` + reserved-memory |
| GICv3 | `/intc` (`0x08000000` GICD, `0x080A0000` GICR) |
| UART | `/chosen stdout-path` or PL011 `0x09000000` |
| PSCI | `/psci` (BL2 already added CPU enable methods) |
| CPU topology / timer / SMMU / watchdog | matching DTB nodes |
| data ZBI | `/chosen linux,initrd-*` → `0x44000000` |

`InitMemory` identity-maps RAM and UART (`ArchSetUpAddressSpace`) so
later appends can run with the MMU on. The data ZBI is the ramdisk
lkboot placed, plus those items.

#### (9) `ZbiBoot`: MMU off, jump into physboot

`arch-boot-zbi.cc` `BootZbi::ZbiBoot`:

- clean D-cache over the next kernel image, invalidate I-cache
- `SCTLR_ELx.M/C/I = 0` (EL1 or EL2 matching `CurrentEL`; here EL2)
- `ISB`, clear `fp`/`lr`/`sp`
- `x0` = data ZBI physical address
- `br entry` into the ZBI kernel (physboot)

This is the **ZBI boot protocol**. The next image is itself a phys
executable (same `_start`).

#### (10) physboot: stay EL2 VHE, MMU on, load `vmzircon`

`physload.cc` calls `ArchPhysloadBeforeInitMemory` → `ArmDropToEl1`
**before** building page tables. On this QEMU (`FEAT_VHE`) that
function does **not** drop: it sets `HCR_EL2.{RW,E2H,TGE}` and
returns, so physboot and `vmzircon` run as an EL2 VHE host (EL1-named
sysregs). Without VHE it would `eret` EL1h.

Then `physboot.cc` `BootZircon`:

- open `STORAGE_KERNEL` BOOTFS, ELF name `"vmzircon"`
- `kernel.Load(..., kHandoffVirtualAddress)` with
  `kHandoffVirtualAddress = 0xffff'ffff'0000'0000`
- code-patch + PIE relocate
- `HandoffPrep::DoHandoff` builds `PhysHandoff` and the first page tables

MMU enable (`phys/address-space.cc`): identity-map RAM and UART on
**TTBR0**, then kernel image / physmap on **TTBR1** ([`memory.md`](memory.md)).
Physboot itself still runs at a physical (or identity) address until
the final jump.

#### (11) Kernel handoff

`HandoffPrep` maps (`handoff-prep-vm.cc`):

```text
KERNEL_ASPACE_BASE + KERNEL_ASPACE_SIZE
        other first-class maps          ↑
        hole (1 page)
        kernel memory image             (load VA 0xffffffff00000000)
        hole (1 page)
        permanent hand-off              ↓
        kernel load − 1 GiB
        temporary hand-off              ↓
KERNEL_ASPACE_BASE + PhysmapSize()
        physmap  PA [0, last RAM)
KERNEL_ASPACE_BASE  (0xffff000000000000)
```

Then jump to the kernel ELF entry at the **high VA**. `lk_main(PhysHandoff*)`
(`top/main.cc`) is now EL2 VHE looking like EL1, TTBR1 live, identity
TTBR0 later `EPD0`'d ([`cpu.md`](cpu.md) secondary.S does the same on APs).

#### (12) `lk_main` → idle + `bootstrap2`

Single-threaded on CPU0 until:

```text
thread_init_early
dlog / constructors / "printing enabled"  (serial: `[cpu N] printing enabled`)
arch_early_init, platform_early_init, DriverHandoffEarly   (UART, GIC early)
vm_init_preheap, heap_init, vm_init
DriverHandoffPostVm                                        (map GICD/GICR)
topology_init, kernel_init
"welcome to secular"
Scheduler::SetCurrCpuActive(true)
Thread::Create("bootstrap2")  affinity CPU0
Thread::Current::BecomeIdle()  ← CPU0 is now IdlePowerThread (cpu.md)
```

`bootstrap2` finishes arch/platform, `mp_signal_curr_cpu_ready`,
`EndHandoff`, `userboot_init`. Secondary CPUs are PSCI `CPU_ON`'d from
platform init (SMC to BL31, which also re-enters Trusty on each AP)
and enter as idle ([`cpu.md`](cpu.md) SMP).

#### (13) `userboot_init`: first EL0 process

`lib/userabi/userabi.rs`:

- `ProcessDispatcher::create` name `"userboot"` under the root job
- root-job observer: if this process dies with no siblings, reboot
- map userboot ELF (static PIE, vDSO only) into its VMAR
- channel with **two handle-only messages**
  1. process capability: log, process, root VMAR, thread, loaded VMAR
  2. system capability: root job, MMIO/IRQ/SMC/SYSTEM resources, ZBI VMO,
     vDSO VMOs, extra hand-off VMOs
- `userboot.start` → `eret` to EL0 ([`cpu.md`](cpu.md) uspace_entry)

Userboot is packed in `STORAGE_KERNEL`, **not** in BOOTFS (it is the thing
that unpacks BOOTFS).

#### (14) userboot → `component_manager` → `zxsh`

[`userspace/secular/bin/userboot/main.rs`](../../../userspace/secular/bin/userboot/main.rs):

- read ZBI `CMDLINE`; product sets
  `userboot.next=libexec/component_manager+--boot`
  (`+` splits extra argv; the source default is still
  `bin/component_manager+--boot`, unused on this image)
- extract `STORAGE_BOOTFS`
- `launch_program` of `libexec/component_manager --boot`

`component_manager --boot` starts the bootstrap realm. On the console
path:

```text
console            eager   kernel debuglog → PTY
console-launcher   eager   console.shell=true → fdio_spawn /boot/bin/sh
zxsh               EL0     process name "sh:console"
```

`console.shell=true` and `kernel.enable-serial-syscalls=true` come from
[`products/secular/BUILD.gn`](../../../products/secular/BUILD.gn). Console
I/O is kernel serial syscalls, not a userspace UART driver.
`guest_manager` (eager) and `vmm_launcher` (lazy) are siblings in the
same realm; they are not on the path to the prompt. The two userboot
channel messages are capability IPC ([`ipc.md`](ipc.md)).

## 4. Linker scripts: physical load image

Two families. **Phys** images are one RWX segment, PIC, link address 0.
**vmzircon** is a PIE ELF linked at 0 and **relocated** to
`kHandoffVirtualAddress`; its script describes **virtual** layout after
handoff, not the QEMU load address.

#### QEMU virt physical map (hardware, not a linker script)

Firmware and lkboot place the images. This is **not** QEMU's default
`-kernel` dump at `DRAM+0x80000`.

```text
PA
0x00000000  flash0 / secular.bios
              BL1 @ 0x0
              FIP @ 0x40000     BL31 + Trusty BL32 + lkboot BL33
0x08000000  GICv3 GICD     (0x10000)
0x080A0000  GICv3 GICR     (0x20000 × NCPU)
0x09000000  PL011 UART
0x09020000  fw_cfg         kernel + initrd selectors
0x0e000000  secure SRAM    ATF BL1 RW / BL2 / BL31
0x0e100000  secure DRAM    Trusty BL32  (14 MiB)
0x40000000  DRAM  (default 1 GiB, -m 1024)
              │
              ├─ 0x40000000  DTB           x0; ATF-patched, 1 MiB window
              ├─ 0x40200000  Image         linux-arm64-boot-shim (lkboot copy)
              ├─ 0x44000000  initrd ZBI    secular.zbi (lkboot copy, 4 KiB padded)
              ├─ 0x60000000  lkboot BL33   dead after chain-load → later PMM
              └─ rest free RAM             → PMM after handoff
0x80000000  DRAM end
```

The initrd is padded so its last page is only ZBI (`kDataZbi`); see
[`memory.md`](memory.md) **Last page of the initrd**.
`paddr_to_physmap(0x40000000) = 0xffff000040000000` once TTBR1 is on.

Device MMIO is **not** DRAM. The kernel maps GIC / UART only where a
driver or the UART path needs them. RAM is tracked by phys `memalloc`
from DTB memory nodes plus special ranges (shim, DTB, initrd), then
handed to PMM.

#### Phys executables — `phys.ld` + friends

Used by the **shim** and by **physboot**. Invocation order for BFD ld:
`-T phys-end.ld -T phys.ld` (and `-T zbi-header.ld` for ZBI kernels).
`linuxboot-pic.ld` sets `PHYS_LOAD_ADDRESS = 0`.

```text
PHYS_LOAD_ADDRESS = 0          (PIC; debug file only)
__executable_start
┌─────────────────────────────────────────────────────────────┐
│ .boot.header                                                │
│   shim:  Linux Image header  (linuxboot-header.S)           │
│   ZBI:   container + item headers  (zbi-header.S)           │
│          then zbi_kernel_t:                                 │
│            entry  = _start − __executable_start             │
│            reserve_memory_size = PHYS_RESERVE_MEMORY_SIZE   │
├─────────────────────────────────────────────────────────────┤
│ .text                                                       │
│ .rodata   kLinkTimeLoadAddress = PHYS_LOAD_ADDRESS          │
│ .dynamic / .rela.dyn / GOT (≤ 4 slots)                      │
│ .data     __data_start                                      │
│ PHYS_LOAD_END                 ← end of the file on disk     │
│                               (phys-end.ld INSERT BEFORE    │
│                                .bss; 8-byte ZBI pad)        │
├─────────────────────────────────────────────────────────────┤
│ .bss      _edata … ALIGN(16) _end                           │
│           start.S zeros this; bootloader must reserve it    │
└─────────────────────────────────────────────────────────────┘
LINUXBOOT_SIZE            = _end − PHYS_LOAD_ADDRESS
PHYS_RESERVE_MEMORY_SIZE  = _end − PHYS_LOAD_END
PHDRS  load  PT_LOAD  PF_R|PF_W|PF_X     (no MMU yet)
ENTRY(_start)
```

No static constructors (`ASSERT .init_array empty`). One segment, RWX:
phys runs with the MMU off, or with an identity map.

The Linux header's size field is `LINUXBOOT_SIZE`, **including `.bss`**, so
the bootloader leaves headroom after the file. The ZBI
`reserve_memory_size` is the same idea for the ZBI protocol.

#### Kernel proper — `kernel.ld`

Linked at 0; physboot relocates every segment to
`0xffff'ffff'0000'0000 + offset`. Page-aligned **virtual** segments:

```text
VA  kHandoffVirtualAddress + offset
0
__executable_start / __rodata_start
  ELF headers, .note.gnu.build-id, .interp, .dynamic, .rodata
  .kcounter.desc          page-aligned VMO for user kcounter
__rodata_end / __code_start          page
  .text
__code_end
__relro_start                        page
  .init_array, .data.rel.ro (lk_init, commands)
__relro_end
__data_start                         page
  .data
.bss  __bss_start                    16-byte
  kcounters_arena                    page-aligned VMO
  remaining .bss
_end                                 page
```

`PT_GNU_RELRO` covers the “writable until relocated” bits; after physboot
relocates, those pages are RO. `__code_start`…`__code_end` is the only
executable span. Physboot refuses a memory image larger than
`KERNEL_IMAGE_MAX_SIZE`.

## 5. References

Read in this order.

| File | Why |
| --- | --- |
| [`tools/secular-boot/run-qemu.sh`](../../../tools/secular-boot/run-qemu.sh) | `-bios` / `-kernel` / `-initrd`, 1 GiB, 8 CPUs, GICv3 |
| [`zircon/scripts/run-zircon`](../../../zircon/scripts/run-zircon) | `secure=true`, `virtualization=true`, `sve=off,sme=off` |
| [`tools/secular-boot/build-atf.sh`](../../../tools/secular-boot/build-atf.sh) | `PLAT=qemu SPD=trusty`, Trusty BL32 + lkboot BL33 |
| [`tools/secular-boot/build-lkboot.sh`](../../../tools/secular-boot/build-lkboot.sh) | `qemu-virt-arm64-lkboot` + `qemu-virt-arm64-trusty` |
| [`sdk/atf/plat/qemu/qemu/platform.mk`](../../../sdk/atf/plat/qemu/qemu/platform.mk) | `secular.bios` = BL1 + FIP @ 256 KiB |
| [`sdk/atf/services/spd/trusty/trusty.c`](../../../sdk/atf/services/spd/trusty/trusty.c) | SPD `ERET` S-EL1, `SMC_YC_NS_RETURN` |
| [`sdk/lk/app/trusty/`](../../../sdk/lk/app/trusty/) | BL32 Trusty OS |
| [`sdk/lk/lib/sm/sm.c`](../../../sdk/lk/lib/sm/sm.c) | first yield to NS |
| [`sdk/lk/app/lkboot/lkboot.c`](../../../sdk/lk/app/lkboot/lkboot.c) | fw_cfg → Image + ZBI, chain-load |
| [`sdk/lk/arch/arm64/asm.S`](../../../sdk/lk/arch/arm64/asm.S) | `arm64_el2_boot`, `arm64_chain_load` |
| [`zircon/kernel/arch/arm64/phys/linuxboot-header.S`](../../../zircon/kernel/arch/arm64/phys/linuxboot-header.S) | Linux Image header |
| [`zircon/kernel/phys/linuxboot-pic.ld`](../../../zircon/kernel/phys/linuxboot-pic.ld) | `PHYS_LOAD_ADDRESS=0`, `LINUXBOOT_SIZE` |
| [`zircon/kernel/phys/phys.ld`](../../../zircon/kernel/phys/phys.ld) | phys segment order, `_end`, reserve |
| [`zircon/kernel/phys/phys-end.ld`](../../../zircon/kernel/phys/phys-end.ld) | `PHYS_LOAD_END` before `.bss` |
| [`zircon/kernel/phys/zbi-header.ld`](../../../zircon/kernel/phys/zbi-header.ld) | `zbi_kernel_t` entry + reserve |
| [`zircon/kernel/arch/arm64/phys/start.S`](../../../zircon/kernel/arch/arm64/phys/start.S) | `_start`, DAIF, `.bss`, `PhysMain` |
| [`zircon/kernel/arch/arm64/phys/boot-shim/linux-arm64-boot-shim.cc`](../../../zircon/kernel/arch/arm64/phys/boot-shim/linux-arm64-boot-shim.cc) | DTB → ZBI |
| [`zircon/kernel/arch/arm64/phys/arch-boot-zbi.cc`](../../../zircon/kernel/arch/arm64/phys/arch-boot-zbi.cc) | MMU-off jump, `x0` = ZBI |
| [`zircon/kernel/lib/arch/arm64/drop-el.S`](../../../zircon/kernel/lib/arch/arm64/drop-el.S) | `ArmDropToEl1`: stay EL2 VHE |
| [`zircon/kernel/phys/physboot.cc`](../../../zircon/kernel/phys/physboot.cc) | load `vmzircon` at handoff VA |
| [`zircon/kernel/phys/handoff-prep-vm.cc`](../../../zircon/kernel/phys/handoff-prep-vm.cc) | physmap / kernel / hand-off maps |
| [`zircon/kernel/kernel.ld`](../../../zircon/kernel/kernel.ld) | vmzircon VA segments |
| [`zircon/kernel/top/main.cc`](../../../zircon/kernel/top/main.cc) | `lk_main`, `bootstrap2`, `userboot_init` |
| [`zircon/kernel/lib/userabi/userabi.rs`](../../../zircon/kernel/lib/userabi/userabi.rs) | first process + two channel msgs |
| [`userspace/secular/lib/userboot/README.md`](../../../userspace/secular/lib/userboot/README.md) | userboot protocol |
| [`userspace/secular/bin/userboot/main.rs`](../../../userspace/secular/bin/userboot/main.rs) | BOOTFS → `component_manager --boot` |
| [`products/secular/BUILD.gn`](../../../products/secular/BUILD.gn) | `userboot.next`, `console.shell` |
| [`userspace/secular/bin/console-launcher/console_launcher.cc`](../../../userspace/secular/bin/console-launcher/console_launcher.cc) | spawn `/boot/bin/sh` |

Suggested first pass: **`run-qemu.sh` → `secular.bios` FIP → Trusty
`SMC_YC_NS_RETURN` → lkboot fw_cfg → Linux header → `ZbiBoot` →
`physboot.cc` Load → `lk_main` → `userboot` `next`**. That is enough to
see serial print `"TEE: sm initialized"` / `"lkboot: jumping to secular
[EL2]"`, then `"printing enabled"` (prefixed `[cpu N]`), then
`"welcome to secular"`, then a `zxsh` prompt.

## Anti-patterns

* **ATF jumps to the Linux Image.** BL33 is lkboot; the Image is fw_cfg.
* **Trust QEMU's default `-kernel` dump at `DRAM+0x80000`.** lkboot copies to `0x40200000`.
* **`userboot.next=bin/component_manager`.** This image uses `libexec/component_manager+--boot`.

## First-pass checklist

- [ ] `run-qemu.sh` → `secular.bios` FIP (Trusty + lkboot)
- [ ] Trusty `SMC_YC_NS_RETURN` then lkboot fw_cfg copy
- [ ] `ZbiBoot` → physboot `Load` → `lk_main` → `userboot.next`
- [ ] Serial: `"welcome to secular"` then a `zxsh` prompt

## Related

* [`bootstrap.md`](bootstrap.md)
* [`memory.md`](memory.md)
* [`cpu.md`](cpu.md)
* [`interrupt.md`](interrupt.md)
* [`ipc.md`](ipc.md)
* [`async.md`](async.md)
* [`sync.md`](sync.md)

## 6. Terminology

| Term | Meaning in this note |
| --- | --- |
| ATF | ARM Trusted Firmware. QEMU `-bios` (`secular.bios` = BL1 + FIP). EL3 runtime stays for PSCI / SPD. |
| FIP | Firmware Image Package at flash `0x40000`. BL31 + Trusty BL32 + lkboot BL33. |
| BL32 / Trusty | Secure EL1 TEE (`qemu-virt-arm64-trusty`, `app/trusty` + `lib/sm`) at `0x0e100000`. |
| BL33 / lkboot | NS EL2 bootloader (`qemu-virt-arm64-lkboot`, `app/lkboot`) at `0x60000000`. |
| fw_cfg | QEMU firmware config at `0x09020000`. Holds `-kernel` and `-initrd`; only lkboot reads it. |
| Linux Image | `linux-arm64-boot-shim.bin`. File with an ARM64 Image header; QEMU `-kernel`. |
| ZBI | Zircon Boot Image. `secular.zbi` is `-initrd`: kernel item + BOOTFS. |
| Phys executable | PIC, one RWX segment, link address 0. Shim and physboot. MMU off or identity. |
| physboot | ZBI kernel item. Stays EL2 VHE, turns the MMU on, loads ELF `vmzircon`. |
| `vmzircon` | The kernel ELF. Relocated to `0xffffffff00000000`. |
| VHE | `HCR_EL2.E2H`. Host runs at EL2 with EL1-named sysregs. This QEMU path stays VHE from lkboot through `vmzircon`. |
| Handoff | `PhysHandoff` plus the first TTBR1 maps (physmap, image, hand-off VMOs). |
| `lk_main` | Kernel C entry. CPU0 until `BecomeIdle`; `bootstrap2` finishes bring-up. |
| userboot | First user process. Static PIE + vDSO. Unpacks BOOTFS. Not itself in BOOTFS. |
| BOOTFS | `STORAGE_BOOTFS` in the ZBI. `component_manager`, `zxsh`, drivers, … |
| `userboot.next` | CMDLINE: product `libexec/component_manager+--boot`. |
| Switch node | A step where EL, MMU, or aspace changes. Numbered (1)–(14) in **Flow**. |

