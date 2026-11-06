# Bootstrap

Crook is a type-1 hypervisor on `qemu-system-aarch64` (virt, arm64). ATF
is EL3 firmware. Crook itself is the **HEE** image: a Zircon kernel that
drops to EL1, then re-enters EL2 only to run guests. This note is the
map of that path: exception-level and MMU switches from reset to SOS
and tbox, and the **physical** layouts the linker scripts publish so a
bootloader can place the images.

The product stays:

```text
ATF (BL1/BL2/BL31) + Crook HEE (boot-shim + Zircon)
  + guest_manager + SOS (Linux) + tbox (Linux)
```

Read [`memory.md`](memory.md) for TTBR / physmap / stage-2,
[`cpu.md`](cpu.md) for VCPU / idle / SMP,
[`interrupt.md`](interrupt.md) for GICv3 / VGIC.
This note stops when SOS and tbox have taken their first `eret` into
guest EL1. The host kernel → userspace switch (`lk_main` → first
`eret` EL0t → `sysmgr`) is (9)–(11).

## Bootstrap

### Framework

Three worlds share one DRAM. ATF carves them from `/reserved-memory`
before any of them runs.

```text
 QEMU virt
   ATF  BL1 → BL2 → BL31          EL3
   FIP  BL32 TEE / BL33 HEE / REE kernel / REE rootfs
              │
              ▼
 ┌────────────────────────────────────────────────────────────────┐
 │ BL31  EL3 runtime (PSCI, SPD)                                  │
 │  SPD: ERET S-EL1 into TEE, SMC back, then EL3 exit to HEE      │
 └────────────────────────────┬───────────────────────────────────┘
                              ▼
 ┌────────────────────────────────────────────────────────────────┐
 │ Crook boot-shim   Linux Image header, MMU off                  │
 │  DTB → BOOTDATA (RAM, GIC, UART, vcpu-phyp, cmdline)           │
 │  br kernel _start                                              │
 │    x0 = BOOTDATA  x1 = bootargs  x2 = dram_range               │
 └────────────────────────────┬───────────────────────────────────┘
                              ▼
 ┌────────────────────────────────────────────────────────────────┐
 │ Crook kernel   EL2 → EL1h, MMU on at KERNEL_BASE, eret EL0t    │
 │  lk_main → userboot → devmgr → appmgr → sysmgr                 │
 └────────────────────────────┬───────────────────────────────────┘
                              ▼
 ┌────────────────────────────────────────────────────────────────┐
 │ guest_manager   (sysmgr, only if kernel.bootmode=ahyp)         │
 │  auto  SOS  vmid=-1                                            │
 │  on demand / delayed  tbox  vmid=1                             │
 └────────────────────────────┬───────────────────────────────────┘
                              ▼
   SOS Linux EL1h (stage-2)     tbox Linux EL1h (stage-2)
```

Three things change along this path. Each **switch node** below is one
of them (sometimes two at once).

| Dimension | Starts as | Ends as |
| --- | --- | --- |
| Exception level | EL3 (ATF) | host EL1 + host EL0 + guest EL1h under EL2 |
| MMU | off | TTBR1 host kernel, TTBR0 per process, then VTTBR stage-2 |
| Address space | physical | host high VA + per-process user VA, then per-guest IPA |

### Flow

Each box is a **switch node**: exception level, MMU, or address space
changes here.

```text
 reset
         │
         ▼  (1) BL1  EL3, ROM
 copy RW to SRAM, load BL2, ERET BL2
         │
         ▼  (2) BL2  EL3, SRAM
 load BL31 / BL32 / BL33 / extra1 / extra2 from FIP
 FDT reserved-memory → image base + entry
         │
         ▼  (3) BL31  EL3 runtime
 GIC, PSCI, SPD. Next image is NON_SECURE
         │
         ▼  (4) SPD → TEE  EL3 → S-EL1 → EL3
 ERET BL32, wait for first SMC, restore NS context
         │
         ▼  (5) EL3 exit → Crook shim
 SPSR EL2h (AHYP). x0 = DTB at hee-region base
         │
         ▼  (6) Linux Image header → boot_shim
 MZ / "ARM\\x64", DTB → BOOTDATA, KERNEL_ALIGN combo
         │
         ▼  (7) kernel _start  EL2 → EL1h
 arm64_elX_to_el1: HCR, VBAR_EL2, VTTBR=0, eret EL1h
         │
         ▼  (8) MMU on
 identity 1 GiB trampoline (TTBR0) + kernel + physmap (TTBR1)
 br KERNEL_BASE, then TCR EPD0 (drop trampoline)
         │
         ▼  (9) lk_main  EL1, TTBR1 only
 thread_init_early … kernel_init
 Thread "bootstrap2"; lk_main becomes idle
 bootstrap2 → LK_INIT_LEVEL_USER → userboot_init
         │
         ▼  (10) kernel → userboot  first EL0
 map RODSO + vDSO into TYPE_USER aspace
 thread_resume → TTBR0 on → eret EL0t
 BOOTFS → bin/devmgr + lib/ld.so.1
 zx_process_start  (same EL1→EL0 path)
         │
         ▼  (11) devmgr → appmgr → sysmgr
 fshost mounts /boot + /system
 launchpad /system/bin/appmgr
 appmgr CreateApplication("sysmgr")
 sysmgr LaunchService GuestManager  (AHYP only)
         │
         ▼  (12) guest_manager
 ScheduleAutoLaunch: SOS now, tbox delayed / Launch(vmid)
         │
         ▼  (13) SOS VMM
 guest_create + stage-2 maps + vcpu EL1h, eret guest
         │
         ▼  (14) SOS Linux protocol
 x0 = DTB, PC = Image (DTB+64 KiB), EL1h DAIF masked
         │
         ▼  (15) tbox VMM  (same Create/Run path)
 reserved RAM identity-mapped, own DTB + Image
         │
         ▼
 SOS and tbox running under Crook EL2
```

| Node | EL | MMU | Who runs |
| --- | --- | --- | --- |
| (1) BL1 | EL3 | off / ATF tables | firmware ROM |
| (2) BL2 | EL3 | ATF SRAM tables | loader |
| (3)–(4) BL31 + SPD | EL3 ↔ S-EL1 | ATF EL3 tables | BL31, then TEE |
| (5)–(6) shim | EL2 | off | PIC phys |
| (7) `_start` | EL2 → EL1h | still off | kernel phys |
| (8) MMU on | EL1 | identity TTBR0, then TTBR1 | still phys-executable until `br` |
| (9) `lk_main` | EL1 | TTBR1; TTBR0 `EPD0` | kernel (bootstrap, then idle) |
| (10) userboot | EL1h → EL0t | TTBR0 walks on (`MMU_TCR_FLAGS_USER`) | first user process |
| (11) devmgr … sysmgr | EL0 (syscall round-trips EL1) | that process's TTBR0 | each launched by the previous |
| (12) guest_manager | EL0 | its TTBR0 | sysmgr singleton |
| (13)–(15) VCPU | host EL1 ↔ EL2 ↔ guest EL1h | host TTBR1 + guest VTTBR | VMM thread / guest |

How each node is **loaded** and **switched**. Load is who copies or maps the image. Switch is the instruction or syscall that changes EL, MMU, or aspace.

| Node | Image lives | Who loads it | Switch |
| --- | --- | --- | --- |
| (1) BL1 | ROM `0x0` | CPU reset PC | still EL3 |
| (2) BL2 | FIP → SRAM | BL1 copy, `ERET` | EL3 → EL3 |
| (3) BL31 | FIP → SRAM | BL2 copy, `ERET` | EL3 runtime stays |
| (4) TEE | FDT `tee-region` | BL2 copy, SPD `ERET` | EL3 → S-EL1 → EL3 |
| (5) shim | FDT `hee-region` | BL2 copy, EL3 `ERET` | EL3 → EL2h |
| (6) `boot_shim` | combo file | PIC, MMU off | same EL2 |
| (7) kernel `_start` | combo + `KERNEL_ALIGN` | shim `br` | EL2 → EL1h |
| (8) MMU on | `kernel.ld` high VA | `br KERNEL_BASE` | EL1, TTBR1 |
| (9) `lk_main` | already at `KERNEL_BASE` | C `bl` | still EL1, TTBR0 off |
| (10) userboot | RODSO in kernel `.rodata` | kernel maps, `eret` | EL1h → EL0t, TTBR0 on |
| (11a) devmgr | BOOTFS `bin/devmgr` | userboot ELF + `zx_process_start` | EL0 → EL1 syscall → EL0 |
| (11b) appmgr | `/system/bin/appmgr` | `launchpad` from devmgr | same |
| (11c) sysmgr | ApplicationLoader `"sysmgr"` | appmgr `CreateApplication` | same |
| (12) guest_manager | `services.config` | sysmgr `LaunchService` | EL0 |
| (13)–(15) guests | reserved RAM + payloads | VMM `HVC` + `eret` | host EL1 ↔ EL2 ↔ guest EL1h |

From (10) onward every process uses the same EL1 → EL0 machinery (`ThreadDispatcher::Start` → `arch_enter_uspace` → `eret`). Only userboot is special: the kernel maps it. Everyone else is loaded in EL0 via VMO + ELF.

### Switch nodes

#### (1) Reset → BL1

BL1 is linked into secure ROM. Entry `bl1_entrypoint`. QEMU presents
flash at `0x00000000`; the FIP lives in flash1.

```text
BL1_RO_BASE  = 0x00000000     ROM  128 KiB
BL1_RW       = top of SRAM    copied at runtime
```

BL1 programs early GIC / UART, copies its `.data` into SRAM, loads BL2
just below BL31, and `ERET`s to BL2 still in EL3.

#### (2) BL2: load the four payload images

BL2 walks a descriptor table. Each node is one image plus the next
handoff. With the Crook SPD the chain is:

```text
BL31  →  BL32 (TEE)  →  BL33 (HEE / Crook)  →  BL33 extra1 (REE kernel)
                                              →  BL33 extra2 (REE rootfs)
```

Before each load, BL2 reads the matching FDT node and **overrides**
the compile-time base:

```text
BL32        /reserved-memory/tee-region     TEE
BL33        /reserved-memory/hee-region     Crook HEE
BL33 extra1 /reserved-memory/ree1-region    SOS kernel + DTB
BL33 extra2 /reserved-memory/ree2-region    SOS rootfs
```

`pc = image_base = reg[0] + offset`. After load, BL2 copies the DTB
into each region's base, sets `/memory.reg` to that region, and leaves
`x0` = DTB address in `ep_info.args.arg0`. Extra2 additionally writes
`/chosen linux,initrd-{start,end}` and strips HEE/TEE-only nodes from
the REE tree (and REE-only nodes from the HEE/TEE trees).

Fallback when the FDT nodes are absent (QEMU Crook SPD defaults):

```text
TEE   BL32_BASE        = 0x40010000    window 0x40000000 + 1 GiB
HEE   NS_IMAGE_OFFSET  = 0x80010000
REE   extra1           = 0x80000000 + 0x10000
rootfs extra2          = 0x88000000    128 MiB
```

The SOS bootloader later consumes the same numbers as GPA offsets
(`kernel @ 0x80000000`, `rootfs @ 0x84000000` on QEMU).

BL33 SPSR is EL2h when EL2 is implemented (AHYP). If the firmware is
told to boot TEE-only, SPSR is EL1h and BL31 hands off to **extra1**
instead of Crook — no guest_manager, no SOS/tbox split.

#### (3) BL31: EL3 runtime

BL31 is linked at the top of trusted SRAM. `bl31_main`: platform
setup (console, GIC), `runtime_svc_init` (PSCI + SPD), then either
the SPD's `bl32_init` hook or a direct EL3 exit.

```text
BL31_BASE  = BL_RAM top − 0x30000     ≈ 0x0e020000
BL31_LIMIT = 0x0e050000               SEC_SRAM 0x0e000000 + 0x50000
```

`cm_init_my_context(next_image_info)` + `cm_prepare_el3_exit` program
`ELR_EL3` / `SPSR_EL3` / the NS or S GPRs. The CPU `ERET`s. Crook
does **not** run at EL3 (`arm64_elX_to_el1` would have to drop from
EL3; the AHYP path never does).

#### (4) SPD → TEE, then back

The SPD registers `trusty_init` as `bl32_init`. On the primary CPU:

1. Save NS EL1 sysregs + FP.
2. `cm_init_my_context(SECURE)` with BL32 `ep_info`.
3. SPSR = `EL1h`, DAIF masked. `x0` = TEE DTB (region base, 2 MiB aligned).
4. `ERET` S-EL1 into TEE. TEE runs until the first SMC.
5. SMC helper restores NS context. `bl31_main` continues.

Secondary CPUs take the same path on `CPU_ON` (`svc_on_finish`). After
this point BL31 stays resident: PSCI, SMC to TEE, FIQ forwarding.

#### (5) EL3 exit → Crook boot-shim

`bl31_prepare_next_image_entry` picks the NS image. For AHYP that is
BL33 (HEE), not extra1. `arg0` is already the HEE DTB — it is **not**
overwritten with the QEMU DTB at `0x40000000`.

| Register / fact | Value |
| --- | --- |
| `x0` | physical address of the HEE DTB (`hee-region` base) |
| `x3` | bootmode (shim saves this; `ahyp` vs `tee`) |
| PC | `hee-region.reg[0] + offset` (Linux Image entry) |
| SPSR | EL2h, DAIF masked |
| MMU / caches | EL3 tables left behind; NS starts MMU-off |

Linux ARM64 protocol: Image anywhere 2 MiB-aligned, `x1`–`x2` unused
by the shim (Crook reuses `x1`/`x2` after `boot_shim` returns).

#### (6) Linux Image header → `boot_shim`

The combo image is `boot-shim.bin` concatenated with the kernel at
`KERNEL_ALIGN = 65536`. The first bytes are a Linux ARM64 Image
header sitting in `.text.boot0`:

```text
offset 0     add / "MZ" lookalike, then b header_end
offset 8     load offset     0
offset 16    image size      0 (unused; QEMU does not place us)
offset 56    magic           "ARM\x64"
```

`_start` (MMU off, PIC, link address 0):

- `x1 = _start + KERNEL_ALIGN` → kernel BOOTDATA container
- 4 KiB shim stack
- save `x3` as `bootmode`
- `bl boot_shim(dtb, kernel)`
- return PC in `x0`; then
  `x0 = bootdata`, `x1 = arch_bootargs`, `x2 = &dram_range`, `br` kernel

`boot_shim` sanity-checks the kernel's BOOTDATA container + KERNEL
item, walks the HEE DTB, and **appends** Zircon items the kernel
cannot discover by itself:

| BOOTDATA item | Source |
| --- | --- |
| `MEM_CONFIG` RAM | `/reserved-memory/hee-region` `reg` |
| `MEM_CONFIG` reserved | kernel image itself (`KERNEL_ALIGN`) + DTB 64 KiB |
| `CMDLINE` | `/chosen bootargs` |
| `CPU_CONFIG` | `/cpus` |
| `KDRV_VCPU_PHYP` | `/reserved-memory/ree1-region` (SOS IPA + entry + `HCR`) |
| UART / VGIC / timer / virq / shbuf | matching DTB nodes |

`KDRV_VCPU_PHYP` is the early hint that SOS already lives in
`ree1-region`: `os_entry_pa = base + offset`, `hcr = 0x80080003`
(`HCR_VM | HCR_SWIO | HCR_TSC | HCR_RW`). Guest_manager later builds
the real guest; this item is how the kernel knows the SOS window
before user space is up.

AHYP vs TEE is encoded in `arch_bootargs` and becomes
`kernel.bootmode=ahyp` on the command line. Cache-all invalidate is
**skipped** in TEE mode so BL31's stack is not dropped from a dirty
line.

#### (7) kernel `_start`: EL2 → EL1h

The kernel is still at a physical address. `_start` in `.text.boot`:

1. CPU0 only: save `x0` (BOOTDATA), `x1` (bootargs), `x2` (dram
   `{start,size}`), `CurrentEL`, physical `_start`.
2. `bl arm64_elX_to_el1`.
3. Enable I/D cache (`SCTLR_EL1.I/C`). Secondary CPUs wait on
   `page_tables_not_ready`.
4. Zero `.bss` at the current physical location.
5. `boot_alloc_init`, record `kernel_base_phys = __code_start`.

`arm64_elX_to_el1` is the first real EL switch on the Crook CPU:

```text
already EL1  → ret

in EL2:
  init_el2_state
  VBAR_EL2     = arm64_el2_init_table     (HVC vectors, not yet guest)
  CNTHCTL_EL2 |= EL1PCTEN | EL1PCEN       (EL1 may touch timers)
  CNTVOFF_EL2  = 0
  VTTBR_EL2    = 0                        (no stage-2 yet)
  CPTR_EL2     = 0x33ff                   (no coprocessor traps)
  HCR_EL2      = RW | API | APK | ATA     (EL1 is AArch64, pointer auth)
  CPACR_EL1    = FPEN                     (no EL1 FP trap)
  SP_EL1       = current SP
  ELR_EL2      = .Ltarget
  SPSR_EL2     = DAIF | EL1h              (0b1111<<6 | 0b0101)
  eret
```

After `eret` the CPU is EL1h, MMU still off, `VBAR_EL2` live. Crook
is a host at EL1 that will `HVC` into those EL2 vectors to create
guests. `HCR.VM` is **not** set yet — that bit is per-VCPU.

#### (8) MMU on, jump to `KERNEL_BASE`

CPU0 builds two tables.

**TTBR1** (`arm64_kernel_translation_table`):

- physmap: `KERNEL_ASPACE_BASE + dram.start` → DRAM, data flags
- kernel:  `KERNEL_BASE` → `__code_start` … `_end`, RWX for now
  (locked down later in VM init)

**TTBR0** (`tt_trampoline`): one identity block covering the 1 GiB
that holds `_start`. Used only for the window between `SCTLR.M=1`
and the `br` to the high VA.

```text
MAIR_EL1  = MMU_MAIR_VAL
TCR_EL1   = MMU_TCR_FLAGS_IDENT     (TTBR0 walk enabled)
TTBR0     = tt_trampoline
TTBR1     = kernel table
SCTLR.M   = 1
.Lmmu_on_pc:  isb
br .Lmmu_on_vaddr                  (absolute KERNEL_BASE + offset)
TCR_EL1   = MMU_TCR_FLAGS_KERNEL   (EPD0: trampoline gone)
tlbi vmalle1
```

`paddr_to_physmap(PA) = 0xffff000000000000 + PA` once TTBR1 is on.
CPU0 then sets `SP` / `TPIDR_EL1` / `x18` (percpu) and `bl lk_main`.
Secondaries take the same MMU sequence and `bl arm64_secondary_entry`.

#### (9) `lk_main` → `userboot_init`

`start.S` has already jumped to `KERNEL_BASE`. CPU0 is EL1h, MMU on,
TTBR1 walking the kernel + physmap, TTBR0 **disabled** (`TCR.EPD0`).
`bl lk_main` (`zircon/kernel/top/main.cpp`) is still a kernel C call —
no EL change.

Single-threaded on CPU0 until it becomes idle. Init hooks live in
`.data.rel.ro.lk_init` and run in level order
(`lk_primary_cpu_init_level`):

```text
thread_init_early                         current thread = "bootstrap"
constructors                              .init_array
LK_INIT_LEVEL_EARLIEST … ARCH_EARLY
arch_early_init
LK_INIT_LEVEL_ARCH_EARLY … PLATFORM_EARLY
platform_early_init
LK_INIT_LEVEL_PLATFORM_EARLY … TARGET_EARLY
target_early_init
"welcome to Crook"
LK_INIT_LEVEL_TARGET_EARLY … VM_PREHEAP
vm_init_preheap
LK_INIT_LEVEL_VM_PREHEAP … HEAP
heap_init
LK_INIT_LEVEL_HEAP … VM
vm_init
LK_INIT_LEVEL_VM … KERNEL
kernel_init                               mp / threads / timers
LK_INIT_LEVEL_KERNEL … THREADING
Thread "bootstrap2"  affinity CPU0
thread_become_idle                        this CPU's idle; ints on; reschedule
```

Crook also records `el2_cpu_resume` as a physical address for later
PSCI suspend (`resume_phy_addr = vaddr_to_paddr(el2_cpu_resume)`).

`lk_main` does **not** run userboot itself. It creates `bootstrap2`
and turns the bootstrap thread into idle. The scheduler then runs
`bootstrap2` on CPU0:

```text
LK_INIT_LEVEL_THREADING … ARCH     arch_init
LK_INIT_LEVEL_ARCH … PLATFORM      platform_init   (PSCI CPU_ON → idle)
LK_INIT_LEVEL_PLATFORM … TARGET    target_init
LK_INIT_LEVEL_TARGET … LAST        includes LK_INIT_LEVEL_USER (0xc0000)
```

`LK_INIT_HOOK(userboot, userboot_init, LK_INIT_LEVEL_USER)` is the
hook that builds the first process. Until that hook, there is no
user aspace, no TTBR0 walk, no EL0.

#### Kernel → userspace: EL1h → EL0t

This is the first time the host CPU drops from kernel to user. Later
processes reuse the same path (`zx_process_start` →
`ThreadDispatcher::Start` → `arch_enter_uspace` → `eret`).

Three kernel objects, then one `eret`.

**Process + aspace.** `ProcessDispatcher::Create(root_job, "userboot")`
allocates a `TYPE_USER` aspace
(`[USER_ASPACE_BASE, +USER_ASPACE_SIZE)` =
`[0x0000000001000000, +0xfffffe000000)`).
`ArmArchVmAspace::Init` (no `ARCH_ASPACE_FLAG_KERNEL`):

- allocate a unique ASID
- allocate and zero one top-level TTBR0 table (`pmm_alloc_kpage`)
- TTBR1 is untouched — kernel mappings stay

**Map userboot + vDSO.** The kernel does not parse ELF at runtime.
`libuserboot.so` is a RODSO baked into `.rodata.rodso_image`;
`userboot-code.h` (generated by `gen-rodso-code.sh`) gives
`USERBOOT_CODE_START`, `USERBOOT_CODE_END`, `USERBOOT_ENTRY`.
`UserbootImage::Map`:

1. Allocate a VMAR covering `userboot.size + vdso.size`.
2. Map userboot RO then RX at offset 0. Entry =
   `vmar.base + USERBOOT_ENTRY`.
3. Map the vDSO **immediately after** userboot. Syscall entry points
   are therefore link-time constants from userboot's `_end`
   (`vdso-syms.ld`). Userboot never looks up vDSO symbols.

A paged stack VMO is mapped RW; `sp` =
`compute_initial_stack_pointer(stack_base, ZIRCON_DEFAULT_STACK_SIZE)`.

**Thread.** `ThreadDispatcher::Create` → `Initialize`:

- kernel stack in the **kernel** aspace (TTBR1)
- `thread_create_etc(..., StartRoutine, this, kstack)`
- `process_->aspace()->AttachToThread(lkthread)` — the LK thread
  now owns this user aspace
- state `INITIALIZED`

**Bootstrap channel.** The kernel writes a `processargs` message down
one end and gives the other end to the new process. Environment =
kernel command line (so `kernel.bootmode=ahyp` is already there).
Handles:

| Slot | `PA_HND` | Object |
| --- | --- | --- |
| VDSO variants | `PA_VMO_VDSO` | vDSO VMOs |
| ramdisk | `PA_VMO_BOOTDATA` | BOOTDATA blob (BOOTFS inside) |
| resource | `PA_RESOURCE` | root resource |
| stack | `PA_VMO_STACK` | userboot's own stack |
| proc / thread / vmar | `PA_PROC_SELF` / `PA_THREAD_SELF` / `PA_VMAR_ROOT` | self |
| job | `PA_JOB_DEFAULT` | child of the root job |
| crashlog | `PA_VMO_KERNEL_FILE` | platform crashlog |

The channel handle value is `arg1`. `arg2` is the vDSO base (userboot
ignores it; the adjacent mapping is enough).

**Start.** `thread->Start(entry, sp, hv, vdso_base, initial_thread=true)`
stores the user PC/SP/args, `AddThread`, `thread_resume`. The idle
CPU reschedules onto that LK thread.

**Aspace switch (still EL1).** `sched.c` sees `newthread->aspace !=
oldthread->aspace` and calls `vmm_context_switch` →
`ArmArchVmAspace::ContextSwitch`:

```text
idle / kernel thread   aspace = null    TCR = MMU_TCR_FLAGS_KERNEL  (EPD0)
userboot thread        aspace = proc    TCR = MMU_TCR_FLAGS_USER    (walks on)
                                        TTBR0_EL1 = (ASID << 48) | tt_phys
```

TTBR1 does not change. The kernel stack and `StartRoutine` still
execute at `KERNEL_BASE`. What changed is: EL0 virtual addresses
would now translate.

**`eret` into EL0.** `StartRoutine` (`thread_dispatcher.cpp`) is the
LK thread entry. It never returns:

```text
arch_enter_uspace(user_entry, user_sp, arg1, arg2)
  spsr = 1 << 8                  // EL0t, SError masked, IRQ/FIQ on
  arm64_uspace_entry(arg1, arg2, pc, sp, kstack_top, spsr, mdscr)
```

`uspace_entry.S`:

```text
mov  sp,       x4        // SP_EL1 = kernel stack (for the next exception)
msr  sp_el0,   x3        // user SP
msr  elr_el1,  x2        // user PC = userboot _start
msr  spsr_el1, x5        // M[3:0] = 0 → EL0t
msr  mdscr_el1, x6
zero x2–x30              // x0 = bootstrap handle, x1 = vdso_base
eret
```

After `eret` the CPU is EL0t. PC is userboot `_start`. User pages
walk TTBR0; kernel pages still walk TTBR1 on the next exception.
This is the host kernel → userspace switch.

**Staying in EL0.** Userboot (and every later process) talks to the
kernel only through the vDSO. A syscall is `svc` from EL0:

```text
EL0  svc
  →  VBAR_EL1  arm64_el1_sync_exc_lower_el_64
  →  ESR.EC == 0x15  →  arm64_syscall_dispatcher
  →  wrapper_zx_*  (still EL1, same TTBR0)
  →  arm64_exc_shared_restore_long
  →  eret  back to EL0t
```

IRQ from EL0 takes the same restore path. There is no second
"enter userspace" helper after the first `eret`; exception return
is the steady-state EL1 ↔ EL0 switch.

#### (10) `userboot`: first EL0 process

`_start(void* start_arg)` in `zircon/system/core/userboot/start.c`.
No libc, no `PT_INTERP`, no writable segment (`RODSO_LDFLAGS -e
_start`). vDSO calls are direct (`vdso-syms.ld`).

```text
zx_log_create
zxr_processargs_read(bootstrap)     // kernel command line + handles
zx_handle_close(bootstrap)
parse_options                       // userboot=  /  userboot.shutdown
bootdata_get_bootfs                 // first BOOTDATA_BOOTFS_BOOT, LZ4
bootfs_mount
zx_process_create(job, filename)    // default filename = "bin/devmgr"
zx_thread_create
elf_load_bootfs                     // ELF + PT_INTERP
elf_load_vmo                        // vDSO at a random base in the child
zx_vmar_map                         // child stack
zx_channel_write                    // child's processargs
zx_process_start(proc, thread, entry, sp, child_channel, vdso_base)
loader_service loop                 // until the child closes PA_SVC_LOADER
zx_process_exit(0)                  // userboot dies; devmgr owns the world
```

The kernel still does not parse BOOTFS. Userboot does.

**ELF load.** `elf_load_bootfs` opens `bin/devmgr` (or `userboot=`*file*)
from BOOTFS. `devmgr` is `ET_DYN` with `PT_INTERP` `ld.so.1`. Userboot
then loads `lib/ld.so.1` from the same BOOTFS and **that** is the
entry it passes to `zx_process_start`. A loader-bootstrap
`processargs` is written first (`PA_VMO_EXECUTABLE`, `PA_SVC_LOADER`,
self handles). The main `processargs` follows, with userboot's
handles forwarded and self slots replaced:

- `PA_PROC_SELF` / `PA_THREAD_SELF` / `PA_VMAR_ROOT` → the child
- extra `PA_VMO_BOOTFS` → the decompressed BOOTFS VMO
- `PA_JOB_DEFAULT`, `PA_RESOURCE`, `PA_VMO_BOOTDATA`, `PA_VMO_VDSO`
  handed on so the rest of the system can create processes and
  drivers

`zx_process_start` (`sys_process_start`) transfers the bootstrap
channel handle into the child and calls the same
`ThreadDispatcher::Start` as (9). The child's first instruction is
`ld.so.1` `_start` in EL0t, with `x0` = channel and `x1` = vDSO base.
The dynamic linker consumes the loader message, maps `devmgr` and
`DT_NEEDED` libs via the loader service (`lib/` + name in BOOTFS),
then jumps to `devmgr`'s `e_entry`.

Userboot's one thread stays in `loader_service()` until `devmgr`
closes that channel (`dl_set_loader_service(ZX_HANDLE_INVALID)` is
the first thing `devmgr` `main` does). Then userboot exits. `devmgr`
lives in the job userboot was given (`PA_JOB_DEFAULT`).

#### (11) `devmgr` → `appmgr` → `sysmgr`

From here every launch is userspace: `zx_process_create` + ELF map +
`zx_process_start`, usually via `launchpad`. The EL1 → EL0 switch is
identical to userboot's child; only the loader and the handles
change.

**Job tree** (`devmgr.c` `main`). The kernel created userboot in the
true root job, and handed userboot a *child* job as `PA_JOB_DEFAULT`.
devmgr names that child `"root"` and hangs everything else off it:

```text
kernel root job
 ├─ process userboot             (exiting; kernel created it here)
 └─ job  (PA_JOB_DEFAULT; devmgr names it "root")
      ├─ process devmgr
      ├─ job "zircon-services"   svcs_job   (trusty policy DENY)
      │    crashlogger, netsvc, virtual-console, fshost, sh
      └─ job "fuchsia"           fuchsia_job
           appmgr
             env "app"
               sysmgr
                 env "sys"
                   guest_manager, nbl_loader_srv, …
```

`devmgr` installs `devfs`, creates the two jobs, a `/svc` channel
pair, an `/otrp` channel pair, and `fshost_event`. Then
`devmgr_vfs_init()` → `fshost_start()`.

**`fshost`.** Launched into `svcs_job` with `launchpad` from the
BOOTFS copy of `/boot/bin/fshost` (devmgr still has no `/boot`
mount). Handles: `/` channel, `/dev`, `/svc`, `/otrp`, root
resource, `PA_VMO_BOOTFS`, remaining BOOTDATA / vDSO / kernel-file
VMOs, `fshost_event`. `fshost` publishes `/boot` from that BOOTFS
and later `/system`. When `/system` is ready it signals
`FSHOST_SIGNAL_READY`.

**`fuchsia_starter`.** A devmgr thread waits on that event, calls
`load_system_drivers()`, `stat("/system/bin/appmgr")`, then
`devmgr_launch(fuchsia_job, "appmgr", {"/system/bin/appmgr"}, …,
FS_FOR_APPMGR)`:

```text
launchpad_create(job)
launchpad_load_from_file("/system/bin/appmgr")    // now via fshost
namespace: /svc /dev /boot /system …  (FS_* flags)
PA_DIRECTORY_REQUEST  = svc_request_handle        // appmgr serves /svc
PA_HND(PA_USER0, 0)   = otrp_request_handle
launchpad_go  →  zx_process_start
```

`launchpad_go` → `launchpad_start` is the generic form of (10):
create channel, map ELF (`PT_INTERP` `ld.so.1` from `/system` or
`/boot` `lib/`), map vDSO at a random base, `zx_process_start(proc,
thread, entry, sp, child_bootstrap, vdso_base)`. musl
`__libc_start_main` reads `processargs` from `x0` and calls `main`.

**`appmgr`.** `garnet/bin/appmgr/main.cc`. Root `JobHolder` labelled
`"app"`, serving `ApplicationLoader`. First async task:

```text
launch_info.url = "sysmgr"
root_job_holder.CreateApplication(launch_info, sysmgr_controller)
sysmgr.set_error_handler(run_sysmgr)     // restart if it dies
```

`CreateApplication` canonicalizes the URL, `LoadApplication`, then
either `CreateApplicationWithProcess` (raw ELF) or
`CreateApplicationFromPackage`. Both end in `launchpad` /
`zx_process_start` into appmgr's job. sysmgr's namespace includes
the environment directory so it can create a nested env.

**`sysmgr`.** `garnet/bin/sysmgr/app.cc`. Constructor:

1. `ApplicationContext::CreateFromStartupInfo`.
2. Read every file in `/system/data/sysmgr/` (`services.config`,
   `apps.config`, …).
3. `CreateNestedEnvironment(..., label="sys")` — the boot
   environment.
4. `RegisterSingleton` for each services.config entry: first
   `ConnectToService` lazily `CreateApplication`s the implementing
   binary and keeps it.
5. `LaunchApplication` for each `apps.config` entry.
6. Hard-coded eager launches, then bootmode:

```text
zx_system_get_bootmode(&bootmode)          // from kernel.bootmode
AHYP  LaunchService("crook.LogService", true)
      LaunchService("fuchsia.virtualization.GuestManager", true)
TEE   LaunchService("crook.GpManager")
```

`LaunchService` is a `ConnectToService` that forces the singleton
path. `services.config` maps
`fuchsia.virtualization.GuestManager` → `"guest_manager"`.
`auto_restart=true` re-launches it 2 s after death.

Guest_manager is therefore a **sysmgr singleton**, not an
`apps.config` entry. It starts because the kernel was entered at
EL2 (`kernel.bootmode=ahyp` from (5)/(6)).

Handle / capability handoff along this chain:

```text
kernel  --processargs-->  userboot
  PA_JOB_DEFAULT, PA_RESOURCE, PA_VMO_BOOTDATA, PA_VMO_VDSO, PA_VMAR_ROOT
userboot  --processargs-->  devmgr
  same, plus PA_VMO_BOOTFS; self handles replaced
devmgr  --launchpad-->  fshost, appmgr, …
  namespace dirs + PA_DIRECTORY_REQUEST (/svc) + resource
appmgr  --CreateApplication-->  sysmgr
  environment channel, loader, /system namespace
sysmgr  --ConnectToService-->  guest_manager
  nested env "sys", service directory
```

#### (12) `guest_manager`: the VM catalog

On start:

1. Open the guest-memory allocator. Remoteproc shared pages come from
   a reserved-memory search string, else a 2 MiB anonymous VMO.
2. Parse `guest_manager.pb.txt`.
3. `LaunchGuests()` → each guest `ScheduleAutoLaunch()`.

QEMU catalog:

```text
guest_configs {
  vmid: -1,
  name: "SOS",
  default_cfg: "sos.lua",
  bootloader: "sos_bootloader.pb.txt",
  auto_start: true,
}

guest_configs {
  vmid: 1,
  name: "tbox",
  default_cfg: "tbox.lua",
  bootloader: "tbox_bootloader.pb.txt",
  memory: "grt,tbox-reserved",
  auto_start: false,
  delayed_launch_ms: 2000,
}
```

`ScheduleAutoLaunch` is a no-op unless `auto_start`. SOS therefore
starts immediately. tbox starts when something calls
`GuestManager.Launch(1)` (or if a board config sets `auto_start`
with the 2 s delay). Both take the same Create / Run path.

`Launch`:

1. If the VMM component is not up, `CreateApplication("file://sos")`
   and bind the `GuestLifecycle` channel (`PA_USER0`).
2. `lifecycle.Create({default_cfg, bootloader, memory, vmid})`.
3. On OK: state `RUNNING`, `lifecycle.Run()`, start the management
   probe.

One VMM component package serves every VM; `vmid` selects the guest.

#### (13) SOS VMM: stage-2 and a VCPU

`VmmController::Create` → `Vmm::Initialize`:

1. Parse `sos.lua` (CPUs, VGIC, IRQs, IO window, cmdline).
2. Guest RAM from the allocator:
   - SOS: `ioctl_get_system_memory` (the leftover DRAM window)
   - tbox: `ioctl_get_guest_memory("grt,tbox-reserved")`
3. Identity `VmemSpec`: `gpa_base = hpa_base = resp.addr`, cached.
4. `zx_guest_create_new(hypervisor_resource, vmid)` → guest + VMAR.
5. For each spec, a physical (or demand) VMO is mapped
   `ZX_VM_FLAG_SPECIFIC | MAP_CONTIGUOUS` at `gpa_base`. That mapping
   **is** the stage-2 table the EL2 `VTTBR` will walk.
6. `TeeVmCreate(vmid)` tells TEE this VM exists.
7. Bootloader proto: load payloads into guest RAM, then the Linux
   protocol helper.
8. VGIC, PCI, virtio-console / vsock / pvblock, …

`Run` → `StartPrimaryVcpu`:

```text
zx_vcpu_create(guest, entry=guest_ip)
WriteState: x[0] = DTB GPA   (extra_params)
SPSR_EL2 = DAIF | EL1h
ELR_EL2  = guest_ip
HCR_EL2  = VM|PTW|FMO|IMO|FB|BSU_IS|TWI|TSC|RW|APK|API|ATA|TID3
VMPIDR   = vpid-derived
```

The VCPU thread is pinned. `zx_vcpu_resume` `HVC`s into EL2, which
loads guest sysregs, `msr vttbr_el2, stage2_root`, and `eret`s.
That `eret` is the host→guest switch: EL2 → guest EL1h, stage-2 on,
DAIF still masked. First intercept (SMC, WFI, VGIC, MMIO trap)
saves guest state and `eret`s back to the VMM thread.

SOS also registers its `El2State` with TEE via
`SMC_FC_NBL_SOS_STATE` so the secure world can find SOS GPRs.

#### (14) SOS Linux protocol

`sos_bootloader.pb.txt`, protocol `"linux"`:

```text
payload kernel  ramdisk-1  offset 0x80000000
payload rootfs  ramdisk-0  offset 0x84000000
```

The kernel payload is a combo: 64 KiB DTB at `phys_base`, Image
immediately after. The bootloader:

- sets `/memory.reg` to the guest RAM window
- `/chosen linux,initrd-{start,end}` = rootfs GPA
- `/chosen bootargs` from lua
- `x0 = dtb_start`, `guest_ip = dtb_start + 0x10000`

Lua (QEMU):

```text
cpus            4
vgic            GICD 0x08000000  GICR 0x080A0000
vgic IRQs       36–251, 253–512   (host SPIs forwarded)
percpu IRQ      15
IO window       GPA 0x00000000 size 0x40000000  (identity MMIO)
guest-reserved  256 MiB advertised to SOS as nbl,guest-memory
vsock / vtee / vhm / ipc-mbox / tipc-vq-notifier
```

SOS therefore sees:

```text
GPA 0x00000000–0x40000000   host MMIO (GIC, UART, …) identity
GPA 0x80000000              DTB + Image (already in ree1-region)
GPA 0x84000000              initrd
rest of system RAM          from the allocator, identity
```

Linux at guest EL1 turns on its own TTBR0/TTBR1. Stage-2 stays on
underneath; IPA = GPA here because every SOS map is identity.

#### (15) tbox: same path, different window

tbox `Launch(1)` is the SOS path with four substitutions.

**RAM.** `memory = "grt,tbox-reserved"`: a DTB reserved-memory node
whose name contains that compatible. Allocator returns `{addr,size}`;
GPA = HPA, cached. tbox does **not** get the SOS IO 0–1 GiB window.

**Images.** `tbox_bootloader.pb.txt`:

```text
payload kernel  ramdisk-1           offset 0x0
payload rootfs  ramdisk-0           offset 0x4000000
payload dtb     tbox_dtb            offset 0x0     (overlays the 64 KiB DTB)
```

tbox DTB `/memory@0` is `reg = <0 0 0 0x10000000>` (256 MiB view);
the bootloader overwrites `reg` with the real reserved window. Entry
is still `dtb + 64 KiB`. Cmdline:

```text
earlycon=pl011,0xffff32000 console=hvc0 INITTAB=/etc/inittab.tbox
```

**IRQs / devices.** 4 VCPUs, same VGIC base, virtio-console (`hvc0`),
no SOS-only VGIC SPI range. `setVmemAuto("vmlog_sink_tbox")` pulls
an extra reserved node by search string if present.

**When.** QEMU leaves `auto_start=false`, so tbox does not race SOS.
Boards that set `auto_start` wait `delayed_launch_ms` (2 s) so SOS
block backends and vsock are up. `vmctl` / FIDL `Launch(1)` is the
same `Create` + `Run`.

After (15) both guests are EL1h under Crook EL2. Further switches
are intercepts (`cpu.md`, `interrupt.md`), not boot.

### Physical memory

#### QEMU virt (hardware, not a linker script)

```text
PA
0x00000000  flash0 / BL1 ROM          128 KiB
0x04000000  flash1 FIP                64 MiB
0x08000000  GICv3 GICD                64 KiB
0x08010000  GICC                      (v2 path)
0x080A0000  GICR                      128 KiB × NCPU
0x09000000  PL011 UART0               HEE / Crook
0x09040000  PL011 UART1               ATF console
0x0e000000  trusted SRAM              320 KiB
              0x0e000000  shared / mailbox  4 KiB
              0x0e001000  BL_RAM
              ≈0x0e003000  BL2
              ≈0x0e020000  BL31   (top 192 KiB)
0x0e100000  trusted DRAM              15 MiB (non-Crook BL32 fallback)
0x40000000  NS DRAM                   3 GiB window
              tee-region   BL32 TEE
              hee-region   Crook combo (shim + kernel + BOOTFS)
              ree1-region  SOS DTB + Image
              ree2-region  SOS rootfs
              tbox-reserved  tbox RAM (if the DTB publishes it)
              leftover     SOS system RAM (allocator)
```

`paddr_to_physmap(0x40000000) = 0xffff000040000000` once Crook TTBR1
is on.

#### Trusted SRAM — BL1 / BL2 / BL31

```text
SEC_SRAM  0x0e000000 + 0x50000
┌─────────────────────────────────────────────────────────────┐
│ shared mailbox + per-CPU hold flags           4 KiB         │
│ BL2   BL2_BASE = BL31_BASE − 0x1D000                        │
│ BL1 RW (relocated from ROM, grows down from BL31_LIMIT)     │
│ BL31  BL31_BASE = BL31_LIMIT − 0x30000                      │
└─────────────────────────────────────────────────────────────┘
BL31_LIMIT = SEC_SRAM end
```

BL1 RO stays in flash. BL32/BL33 are **not** in SRAM; they are DRAM
windows from the FDT.

#### Crook HEE window (`hee-region`)

Combo image the shim described in (6):

```text
hee-region.reg[0]
┌─────────────────────────────────────────────────────────────┐
│ boot-shim.bin     PIC, one RWX PT_LOAD, link address 0      │
│                   Linux Image header in .text.boot0         │
│ pad to KERNEL_ALIGN (64 KiB)                                │
│ zircon kernel     BOOTDATA container + KERNEL item          │
│                   then .text … .data; .bss reserved         │
│ BOOTFS / rest of HEE RAM → PMM after handoff                │
└─────────────────────────────────────────────────────────────┘
kernel image itself is MEM_RANGE_RESERVED so PMM will not reuse it
```

#### SOS window (`ree1` / `ree2` + system RAM)

```text
ree1-region.reg[0]                 = SOS DTB GPA
ree1-region.reg[0] + offset (64K)  = SOS Image entry
ree2-region / ramdisk-0 @ 0x84000000
                                   = initrd
allocator system memory            = SOS RAM, identity
0x00000000 + 1 GiB                 = SOS IO (lua setVmem)
256 MiB (or DTB gz-guest-rk*)      = nbl,guest-memory hole SOS must not touch
```

#### tbox window (`grt,tbox-reserved`)

```text
reserved.reg[0] + 0x00000000   DTB (overlaid) + Image
reserved.reg[0] + 0x04000000   rootfs
size from the allocator        identity GPA = HPA
```

tbox's own DTB initially claims 256 MiB at IPA 0; the Linux
bootloader rewrites `/memory.reg` to the real reserved base/size.

### Linker scripts

Five families. ATF scripts describe **absolute physical** SRAM.
Phys images (shim) are PIC, link address 0. The Crook kernel is
linked at a fixed high VA. Guest Linux Images use the ARM64 header
and are not linked by Crook.

#### BL1 — ROM + SRAM

```text
ENTRY(bl1_entrypoint)
MEMORY {
  ROM (rx)  : ORIGIN = BL1_RO_BASE,  LENGTH = BL1_RO_LIMIT − BL1_RO_BASE
  RAM (rwx) : ORIGIN = BL1_RW_BASE,  LENGTH = BL1_RW_LIMIT − BL1_RW_BASE
}
. = BL1_RO_BASE;          page-aligned
.text / .rodata     >ROM
.data / .bss        >RAM      (copied from ROM at runtime)
```

#### BL2 — SRAM below BL31

```text
ENTRY(bl2_entrypoint)
MEMORY { RAM (rwx): ORIGIN = BL2_BASE, LENGTH = BL2_LIMIT − BL2_BASE }
. = BL2_BASE;             page-aligned
.text / .rodata / .data / .bss  >RAM
```

#### BL31 — top of SRAM

```text
ENTRY(bl31_entrypoint)
MEMORY { RAM (rwx): ORIGIN = BL31_BASE, LENGTH = BL31_LIMIT − BL31_BASE }
. = BL31_BASE;            page-aligned
.text     bl31_entrypoint first, then vectors, page-round
.rodata   rt_svc_descs, cpu_ops, pubsub
.data / stacks / .bss
ASSERT(__BL31_END__ ≤ BL31_LIMIT)
```

BL31 stays mapped in EL3 for the life of the machine.

#### Boot-shim — `boot-shim.ld`

PIC, one segment, used only with the MMU off:

```text
ENTRY(_start)
. = 0;
.text.boot0     Linux Image header
.text / .rodata / .data
.boot-shim.stack    4 KiB
.bss                (should be empty)
PHDRS  load  PT_LOAD  PF_R|PF_W|PF_X
```

`KERNEL_ALIGN` is **not** in this script; the image builder pads the
shim file to 64 KiB before appending the kernel.

#### Crook kernel — `kernel.ld`

Linked at a fixed VA; loaded wherever BL2 put `hee-region + offset`.
`_start` runs at the physical address, then (8) jumps here.

```text
ENTRY(_start)
. = KERNEL_BASE;                 0xffffffff00000000
__code_start
  .text.boot0   BOOTDATA container + KERNEL item
                entry = _start − _bootdata_header
  .buildsig     BSIG / self-pointer / version / build-id
  .text.boot    _start
  .text
__code_end
  page
__rodata_start
  .rodata.rodso_image    userboot + vDSO blobs
  .note.gnu.build-id
  .kcounter.desc
  .rodata
  .data.rel.ro           lk_init, commands, …
  .init_array
  page
.data
  __data_start … ALIGN(8) __data_end     BOOTDATA 8-byte rule
  page
.bss
  kcounters_arena
  remaining .bss
_end
```

`.text.boot0` is why a raw kernel file is already a valid BOOTDATA
container: the shim treats `_start + KERNEL_ALIGN` as
`zircon_kernel_t`.

`KERNEL_ASPACE_BASE = 0xffff000000000000` (physmap),
`KERNEL_BASE = 0xffffffff00000000` (−4 GiB). User space is
`[0x0000000001000000, +0xfffffe000000)`.

#### Guest Linux Image (not a Crook script)

SOS and tbox kernels are ordinary ARM64 Images. Crook never
relocates them. The VMM only:

- copies the file into guest RAM at the proto `offset`
- points `ELR_EL2` at `offset + 64 KiB`
- leaves `x0` at `offset` (DTB)

The Image header inside those blobs is Linux's, not Crook's.

### Terminology

| Term | Meaning in this note |
| --- | --- |
| ATF | ARM Trusted Firmware. BL1/BL2/BL31. EL3, then PSCI/SMC resident. |
| FIP | Firmware Image Package in flash. BL31 + BL32 + BL33 + extras. |
| BL32 / TEE | Secure EL1 payload. SPD `ERET`s into it before Crook. |
| HEE / Crook | BL33. Zircon hypervisor host. AHYP enters it at EL2. |
| REE | SOS Linux. Loaded as BL33 extra1/extra2; **run** by guest_manager, not by BL31 (AHYP). |
| boot-shim | PIC Linux Image in front of the kernel. DTB → BOOTDATA. |
| Combo image | shim + 64 KiB pad + kernel BOOTDATA. One BL33 file. |
| BOOTDATA | Zircon boot container (v2). Kernel item + appended driver items. |
| `KERNEL_BASE` | `0xffffffff00000000`. Kernel link VA. |
| Physmap | `KERNEL_ASPACE_BASE + PA`. Host kernel's view of DRAM. |
| AHYP | `kernel.bootmode=ahyp`. EL2 handoff, guest_manager runs. |
| SOS | System OS. Linux guest, `vmid = -1`, auto-start, identity IO + RAM. |
| tbox | User OS. Linux guest, `vmid = 1`, reserved RAM, virtio console. |
| Stage-2 | EL2 `VTTBR` walk. IPA (guest PA) → host PA. Identity for both VMs here. |
| Switch node | A step where EL, MMU, or aspace changes. Numbered (1)–(15) in **Flow**. |
| RODSO | Read-only DSO. No writable segment. userboot and the vDSO. |
| `processargs` | Bootstrap message on a channel: handles + argv/environ. Kernel → userboot, then every `zx_process_start`. |
| `launchpad` | Userspace ELF loader. `zx_process_create` + map + `zx_process_start`. |
| EL0t | EL0 using `SP_EL0`. SPSR `M[3:0] = 0`. Host userspace. |
| `TYPE_USER` aspace | Per-process TTBR0 table + ASID. `[0x1000000, +0xfffffe000000)`. |

### Code (lk_main → sysmgr)

| Step | File |
| --- | --- |
| `bl lk_main` | `zircon/kernel/arch/arm64/start.S` |
| init levels, `bootstrap2`, idle | `zircon/kernel/top/main.cpp` |
| `userboot_init` / map RODSO | `zircon/kernel/lib/userboot/userboot.cpp` |
| user aspace / ASID / TTBR0 | `zircon/kernel/object/process_dispatcher.cpp`, `zircon/kernel/arch/arm64/mmu.cpp` |
| LK thread + `AttachToThread` | `zircon/kernel/object/thread_dispatcher.cpp` |
| scheduler TTBR0 install | `zircon/kernel/kernel/sched.c` `vmm_context_switch` |
| `eret` EL0t | `zircon/kernel/arch/arm64/arch.cpp`, `uspace_entry.S` |
| syscall `svc` → `eret` | `zircon/kernel/arch/arm64/exceptions.S` |
| userboot `_start`, BOOTFS, ELF | `zircon/system/core/userboot/start.c` |
| `zx_process_start` | `zircon/kernel/syscalls/task.cpp` |
| `devmgr` jobs, fshost, appmgr | `zircon/system/core/devmgr/devmgr.c` |
| `launchpad_go` | `zircon/system/ulib/launchpad/launchpad.c` |
| appmgr → `sysmgr` | `garnet/bin/appmgr/main.cc` |
| `LaunchService` GuestManager | `garnet/bin/sysmgr/app.cc` |

---

Everyday's a Hustle.
