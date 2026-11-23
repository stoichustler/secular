# Bootstrap

Savior is a type-1 hypervisor on `qemu-system-aarch64` (virt, arm64). ATF
is EL3 firmware. Savior itself is the **HEE** image: a Zircon kernel that
stays at EL2 in **VHE** (`HCR.E2H|TGE`) so host code looks like EL1,
then clears `TGE` only to run guests. This note is the map of that
path: exception-level and MMU switches from reset to SOS and tbox,
and the **physical** layouts the linker scripts publish so a
bootloader can place the images.

The product stays:

```text
ATF (BL1/BL2/BL31) + Savior HEE (boot-shim + Zircon)
  + guest_manager + SOS (Linux) + tbox (Linux)
```

This is chapter 2 of the handbook ([`README.md`](README.md) learning
path). End-to-end picture: [`flows.md`](flows.md) F1. Read
[`README.md`](README.md) for the product map,
[`memory.md`](memory.md) for TTBR / physmap / stage-2,
[`cpu.md`](cpu.md) for VCPU / idle / SMP / VHE,
[`interrupt.md`](interrupt.md) for GICv3 / VGIC.
This note stops when SOS and tbox have taken their first `eret` into
guest EL1. The host kernel → userspace switch (`lk_main` → first
`eret` EL0t → `sysmgr`) is (9)–(11).

Start with **Diagrams** (B1–B10), then **Switch nodes** for the
prose. End-to-end one-pager: [`flows.md`](flows.md) F1.

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
 │ Savior boot-shim   Linux Image header, MMU off                  │
 │  DTB → BOOTDATA (RAM, GIC, UART, vcpu-phyp, cmdline)           │
 │  br kernel _start                                              │
 │    x0 = BOOTDATA  x1 = bootargs  x2 = dram_range               │
 └────────────────────────────┬───────────────────────────────────┘
                              ▼
 ┌────────────────────────────────────────────────────────────────┐
 │ Savior kernel   VHE EL2 (E2H|TGE), MMU on at KERNEL_BASE, eret EL0t │
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
| Exception level | EL3 (ATF) | host VHE EL2 (looks like EL1) + host EL0 + guest EL1h |
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
         ▼  (5) EL3 exit → Savior shim
 SPSR EL2h (AHYP). x0 = DTB at hee-region base
         │
         ▼  (6) Linux Image header → boot_shim
 MZ / "ARM\\x64", DTB → BOOTDATA, KERNEL_ALIGN combo
         │
         ▼  (7) kernel _start  stay EL2 (VHE)
 arm64_elX_to_el2: HCR.E2H|TGE, VTTBR=0, ret
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
 SOS and tbox running under Savior EL2
```

| Node | EL | MMU | Who runs |
| --- | --- | --- | --- |
| (1) BL1 | EL3 | off / ATF tables | firmware ROM |
| (2) BL2 | EL3 | ATF SRAM tables | loader |
| (3)–(4) BL31 + SPD | EL3 ↔ S-EL1 | ATF EL3 tables | BL31, then TEE |
| (5)–(6) shim | EL2 | off | PIC phys |
| (7) `_start` | EL2 VHE (`E2H\|TGE`) | still off | kernel phys |
| (8) MMU on | VHE EL2 | identity TTBR0, then TTBR1 | still phys-executable until `br` |
| (9) `lk_main` | VHE EL2 (EL1 names) | TTBR1; TTBR0 `EPD0` | kernel (bootstrap, then idle) |
| (10) userboot | VHE EL2 → EL0t | TTBR0 walks on (`MMU_TCR_FLAGS_USER`) | first user process |
| (11) devmgr … sysmgr | EL0 (syscall round-trips host) | that process's TTBR0 | each launched by the previous |
| (12) guest_manager | EL0 | its TTBR0 | sysmgr singleton |
| (13)–(15) VCPU | host VHE ↔ guest EL1h | host TTBR1 + guest VTTBR | VMM thread / guest |

How each node is **loaded** and **switched**. Load is who copies or maps the image. Switch is the instruction or syscall that changes EL, MMU, or aspace.

| Node | Image lives | Who loads it | Switch |
| --- | --- | --- | --- |
| (1) BL1 | ROM `0x0` | CPU reset PC | still EL3 |
| (2) BL2 | FIP → SRAM | BL1 copy, `ERET` | EL3 → EL3 |
| (3) BL31 | FIP → SRAM | BL2 copy, `ERET` | EL3 runtime stays |
| (4) TEE | FDT `tee-region` | BL2 copy, SPD `ERET` | EL3 → S-EL1 → EL3 |
| (5) shim | FDT `hee-region` | BL2 copy, EL3 `ERET` | EL3 → EL2h |
| (6) `boot_shim` | combo file | PIC, MMU off | same EL2 |
| (7) kernel `_start` | combo + `KERNEL_ALIGN` | shim `br` | stay EL2 VHE |
| (8) MMU on | `kernel.ld` high VA | `br KERNEL_BASE` | VHE EL2, TTBR1 |
| (9) `lk_main` | already at `KERNEL_BASE` | C `bl` | still VHE EL2, TTBR0 off |
| (10) userboot | RODSO in kernel `.rodata` | kernel maps, `eret` | VHE EL2 → EL0t, TTBR0 on |
| (11a) devmgr | BOOTFS `bin/devmgr` | userboot ELF + `zx_process_start` | EL0 → EL1 syscall → EL0 |
| (11b) appmgr | `/system/bin/appmgr` | `launchpad` from devmgr | same |
| (11c) sysmgr | ApplicationLoader `"sysmgr"` | appmgr `CreateApplication` | same |
| (12) guest_manager | `services.config` | sysmgr `LaunchService` | EL0 |
| (13)–(15) guests | reserved RAM + payloads | VMM `zx_vcpu_resume` + `eret` | host VHE ↔ guest EL1h |

From (10) onward every process uses the same EL1 → EL0 machinery (`ThreadDispatcher::Start` → `arch_enter_uspace` → `eret`). Only userboot is special: the kernel maps it. Everyone else is loaded in EL0 via VMO + ELF.

### Diagrams

Numbered swimlanes for (1)–(15). Each box is a real function or
instruction. EL / MMU is in the left gutter.

| # | Covers | Nodes |
| --- | --- | --- |
| B1 | ATF: reset → BL33 | (1)–(5) |
| B2 | boot-shim: DTB → BOOTDATA → kernel `br` | (6) |
| B3 | `_start` VHE + MMU trampoline | (7)–(8) |
| B4 | `lk_main` / `bootstrap2` / idle / secondaries | (9) |
| B5 | first `eret` EL0t (userboot) | (10) |
| B6 | userboot → `devmgr` | (10)–(11a) |
| B7 | `devmgr` → `sysmgr` → GuestManager | (11)–(12) |
| B8 | SOS VMM → first guest `eret` | (13)–(14) |
| B9 | tbox (same path, four substitutions) | (15) |
| B10 | register handoff at each `ERET`/`br`/`eret` | (5)(6)(10)(14) |

#### B1  ATF: reset → Savior shim

```text
 CPU reset  PC = 0x0  EL3  MMU off
   │
   ▼
[1] bl1_entrypoint                    BL1_RO in flash
      copy .data → SRAM (BL1_RW)
      early GIC / UART
      load BL2 just below BL31
      ERET  still EL3
   │
   ▼
[2] bl2_main                          SRAM
      walk image descriptors from FIP
      for each: FDT /reserved-memory  overrides compile-time base
        BL32        tee-region
        BL33        hee-region          Savior combo
        BL33 extra1 ree1-region         SOS DTB+Image
        BL33 extra2 ree2-region         SOS rootfs
      copy DTB into each region base; x0 will be that DTB
      ERET  BL31  still EL3
   │
   ▼
[3] bl31_main                         top of SRAM
      console, GIC, runtime_svc_init (PSCI + SPD)
   │
   ▼
[4] SPD trusty_init  (bl32_init)
      save NS EL1 sysregs + FP
      SPSR = EL1h DAIF  x0 = TEE DTB
      ERET  S-EL1  BL32
      TEE  first SMC
      restore NS  →  bl31_main continues
   │
   ▼
[5] bl31_prepare_next_image_entry     AHYP: BL33, not extra1
      ELR_EL3  = hee-region + offset
      SPSR_EL3 = EL2h DAIF
      x0       = HEE DTB  (region base, not QEMU 0x40000000)
      x3       = bootmode
      ERET  EL3 → EL2h  MMU off
```

No Savior instruction has run yet. Next is B2.

#### B2  boot-shim: Image header → kernel `br`

Combo file: `boot-shim.bin` + pad to `KERNEL_ALIGN=64KiB` +
kernel BOOTDATA. PIC, link address 0, MMU still off.

```text
 EL2h  PC = hee-region  (Linux Image header in .text.boot0)
   │
   ▼
[1] MZ / "ARM\x64"  b header_end
[2] x1 = _start + KERNEL_ALIGN          kernel BOOTDATA
    4 KiB shim stack
    save x3 as bootmode
[3] bl boot_shim(dtb=x0, kernel=x1)
      check BOOTDATA + KERNEL item
      walk HEE DTB, append:
        MEM_CONFIG RAM          hee-region.reg
        MEM_CONFIG reserved     kernel image + DTB 64KiB
        CMDLINE                 /chosen bootargs
        CPU_CONFIG              /cpus
        KDRV_VCPU_PHYP          ree1-region  (SOS window hint)
        UART / VGIC / timer …
      bootmode → arch_bootargs → kernel.bootmode=ahyp
      TEE mode: skip cache-all invalidate
      return PC of kernel _start in x0
[4] x0 = bootdata
    x1 = arch_bootargs
    x2 = &dram_range {start,size}
    br  kernel _start                   still EL2, still phys
```

`KDRV_VCPU_PHYP` is a hint only. Real SOS guest is B8.

#### B3  `_start` VHE + MMU trampoline

```text
 EL2  MMU off  phys _start
   │
   ▼
[1] CPU0: save x0 BOOTDATA, x1 bootargs, x2 dram, CurrentEL, phys _start
    secondaries: wait page_tables_not_ready
[2] bl arm64_elX_to_el2                 HEE / WITH_VHE_SUPPORT
      must already be EL2
      init_el2_state
      HCR_EL2 |= E2H | TGE              host uses EL1 names
      VTTBR_EL2 = 0
      ret                               still EL2
    (TEE / mt8678: arm64_elX_to_el1, eret EL1h)
[3] SCTLR_EL1.I/C = 1                   VHE EL2&0 view of caches
    zero .bss at current phys
    boot_alloc_init
    kernel_base_phys = __code_start
   │
   ▼
[4] CPU0 builds two tables
      TTBR1  arm64_kernel_translation_table
        physmap  KERNEL_ASPACE_BASE+dram → DRAM
        kernel   KERNEL_BASE → __code_start … _end  RWX
      TTBR0  tt_trampoline
        one 1 GiB identity block holding _start
[5] MAIR / TCR = MMU_TCR_FLAGS_IDENT    TTBR0 walk on
    msr ttbr0, ttbr1
    SCTLR.M = 1
    isb
[6] br .Lmmu_on_vaddr                   absolute KERNEL_BASE + offset
      now executing at high VA
[7] TCR = MMU_TCR_FLAGS_KERNEL          EPD0: trampoline gone
    tlbi vmalle1
[8] SP / TPIDR_EL1 / x18 = percpu
    bl lk_main                          still VHE EL2, TTBR1 only
```

`paddr_to_physmap(PA) = 0xffff000000000000 + PA` after [6].

#### B4  `lk_main` → `bootstrap2` → idle

Still VHE EL2, TTBR1, no EL0. Single-threaded on CPU0 until idle.

```text
 bl lk_main                            zircon/kernel/top/main.cpp
   │
   ▼
[1] thread_init_early                   current = "bootstrap"
    call_constructors
    ARCH_EARLY → arch_early_init
    PLATFORM_EARLY → platform_early_init
    TARGET_EARLY → target_early_init
    "welcome to Savior"  CurrentEL
[2] VM_PREHEAP → vm_init_preheap
    HEAP → heap_init
    VM → vm_init                        lock kernel RX/RO/RW later
    KERNEL → kernel_init                mp / threads / timers
[3] thread_create "bootstrap2"  CPU0
    thread_resume
    resume_phy_addr = vaddr_to_paddr(el2_cpu_resume)
    thread_become_idle                  ints on, this CPU's idle
   │
   ▼  scheduler
 bootstrap2
   [4] THREADING → ARCH → arch_init
         lk_init_secondary_cpus
         PSCI CPU_ON → each secondary:
           same B3 MMU sequence
           bl arm64_secondary_entry
           lk_secondary_cpu_entry → idle
   [5] ARCH → PLATFORM → platform_init
       PLATFORM → TARGET → target_init
       TARGET → LAST
         includes LK_INIT_LEVEL_USER (0xc0000)
         LK_INIT_HOOK(userboot, userboot_init, USER)
   │
   ▼
[6] userboot_init                       B5
    bootstrap2 returns; CPU0 idle runs until first user thread
```

No user aspace, no TTBR0 walk, no EL0 until B5.

#### B5  first `eret` EL0t (userboot)

Three objects, then one `eret`. Later processes reuse this path.

```text
 userboot_init
   │
   ▼
[1] ProcessDispatcher::Create(root_job, "userboot")
      TYPE_USER aspace  [0x1000000, +0xfffffe000000)
      ArmArchVmAspace::Init
        alloc ASID
        alloc+zero TTBR0 top table
        TTBR1 untouched
[2] UserbootImage::Map
      RODSO libuserboot.so in .rodata
      VMAR = userboot.size + vdso.size
      map userboot RO/RX at 0, entry = vmar.base + USERBOOT_ENTRY
      map vDSO immediately after (vdso-syms.ld)
      paged stack VMO RW
[3] ThreadDispatcher::Create
      kernel stack in TTBR1
      AttachToThread → LK thread owns user aspace
[4] processargs on a channel
      PA_VMO_VDSO / BOOTDATA / RESOURCE / STACK
      PA_PROC_SELF / THREAD_SELF / VMAR_ROOT / JOB_DEFAULT
      environ = kernel cmdline  (kernel.bootmode=ahyp)
[5] thread->Start(entry, sp, hv, vdso_base)
      thread_resume
   │
   ▼  idle reschedules onto userboot LK thread
[6] sched.c  aspace changed
      vmm_context_switch
      TCR = MMU_TCR_FLAGS_USER          TTBR0 walks on
      TTBR0_EL1 = (ASID<<48) | tt_phys
[7] StartRoutine → arch_enter_uspace
      spsr = EL0t, SError masked, IRQ/FIQ on
[8] uspace_entry.S
      SP_EL1 = kstack
      SP_EL0 = user sp
      ELR_EL1 = userboot _start
      SPSR_EL1 M=EL0t
      x0 = bootstrap handle, x1 = vdso_base
      zero x2–x30
      eret
   │
   ▼
 EL0t  PC = userboot _start
   user VA → TTBR0    kernel VA → TTBR1 (next svc/IRQ)
```

Steady-state: `svc` → `VBAR_EL1` → syscall wrapper → `eret` EL0t.

#### B6  userboot → `devmgr`

```text
 EL0t  userboot _start(start_arg)
   │  no libc, RODSO, vDSO direct
   ▼
[1] zxr_processargs_read(bootstrap)
    parse_options   userboot= / userboot.shutdown
[2] bootdata_get_bootfs     first BOOTDATA_BOOTFS_BOOT, LZ4
    bootfs_mount
[3] zx_process_create(job, "bin/devmgr")
    zx_thread_create
[4] elf_load_bootfs bin/devmgr
      ET_DYN  PT_INTERP = lib/ld.so.1
    elf_load_bootfs ld.so.1           that is the entry
    map child stack
[5] zx_channel_write
      loader processargs  PA_VMO_EXECUTABLE, PA_SVC_LOADER
      main processargs    PA_VMO_BOOTFS + forwarded handles
[6] zx_process_start(proc, thread, ld.so.1, sp, child_channel, vdso)
      same ThreadDispatcher::Start as B5
      child's first insn: ld.so.1 _start EL0t
[7] loader_service loop until child closes PA_SVC_LOADER
    zx_process_exit(0)                userboot dies
```

Kernel never parses BOOTFS. Userboot does. `ld.so.1` maps `devmgr`
and `DT_NEEDED` from `lib/` in BOOTFS.

#### B7  `devmgr` → `sysmgr` → GuestManager

Every step is B5's `eret` machinery via `launchpad` / `zx_process_start`.

```text
 EL0  ld.so.1 → bin/devmgr
   │
   ▼
[1] devmgr
      root job + driver jobs
      fshost  mount /boot (BOOTFS) + /system
      launchpad /system/bin/appmgr
   │
   ▼
[2] appmgr
      CreateApplication("sysmgr")
   │
   ▼
[3] sysmgr  (garnet/bin/sysmgr/app.cc)
      zx_system_get_bootmode
        AHYP → LaunchService("fuchsia.virtualization.GuestManager")
        TEE  → LaunchService("nebula.GpManager")   stop here
      services.config  → binary "guest_manager"
   │
   ▼
[4] GuestManagerImpl
      BuildFromFile /system/data/guest_manager.pb.txt
      LaunchGuests
        SOS   auto_start true   ScheduleAutoLaunch now
        tbox  auto_start false  wait Launch(1) / delay
```

AHYP-only from [3]. TEE never reaches B8.

#### B8  SOS VMM → first guest `eret`

```text
 Guest::Launch(vmid=-1)
   │
   ▼
[1] CreateApplication("file://sos")
      PA_USER0 = GuestLifecycle channel
      VmmController Bind
      state = STARTING
      ArmWatchdogStartupGrace  +60 s
[2] lifecycle.Create(config)
      Vmm::Initialize
        product.lua + sos.lua
        ioctl_get_system_memory          leftover DRAM
        VmemSpec ram kCached identity    GPA = HPA
        setVmem "IO" 0 + 1GiB kDevice
        zx_guest_create_new              Guest + guest VMAR  (S2)
        map each spec  ZX_VM_FLAG_SPECIFIC
        TeeVmCreate
        SetCpuAffinity
        VGIC traps + passthrough bind
        virtio / vsock / vSMMU / VHM …
        bootloader Setup
          payloads into guest RAM
          guest_ip = DTB + 64KiB
          x0 will be DTB
[3] lifecycle.Run → StartPrimaryVcpu
      RegisterVcpuFactory
      thrd_create "vcpu-0"
      zx_vcpu_create  pin bind_cpus[0]
        ELR_EL2 = guest_ip
        SPSR    = EL1h | DAIF
        HCR     = VM|PTW|FMO|IMO|FB|BSU_IS|TWI|TSC|RW|…
        SMC_FC_NBL_SOS_STATE  TEE sees GPRs
      WriteState  x0 = DTB GPA
      GICR BindVcpu
      loop zx_vcpu_resume               [`flows.md`](flows.md) F2
        first: tlbi vmalle1
        bic TGE, msr vttbr, eret EL1h
   │
   ▼
 SOS Linux  EL1h  PC = Image  x0 = DTB  DAIF masked
```

That `eret` is the host→guest switch. Further intercepts: F2–F14.

#### B9  tbox (same Create/Run, four substitutions)

```text
 Launch(1)  or delayed auto_start
   │  same B8 [1]–[3]
   │
   ├─ RAM     ioctl_get_guest_memory("grt,tbox-reserved")
   │          no SOS 0–1GiB IO window
   ├─ Image   tbox_bootloader.pb.txt
   │          DTB @ reserved+0, Image +64KiB, rootfs +0x4000000
   ├─ IRQs    same VGIC base, no SOS SPI range
   │          virtio-console hvc0
   └─ when    QEMU auto_start=false; boards delay 2 s
```

#### B10  register handoff

What the next image sees. Unused GPRs are not listed.

```text
 [5] EL3 → shim          x0=HEE DTB   x3=bootmode   PC=hee-region
                         SPSR=EL2h DAIF   MMU off

 [6] shim → kernel       x0=BOOTDATA  x1=bootargs   x2=&dram_range
                         PC=kernel _start          MMU off

 [8] br KERNEL_BASE      same GPRs, now high VA     TTBR1 on

 [10] eret EL0t          x0=bootstrap handle  x1=vdso_base
                         PC=userboot _start   SP=user stack
                         SPSR=EL0t            TTBR0 on

 [11] zx_process_start   x0=child channel     x1=vdso_base
                         PC=ld.so.1 / entry   same eret path

 [14] eret guest EL1h    x0=DTB GPA
                         PC=Image (DTB+64KiB)
                         SPSR=EL1h DAIF       VTTBR on  TGE=0
```

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
handoff. With the Savior SPD the chain is:

```text
BL31  →  BL32 (TEE)  →  BL33 (HEE / Savior)  →  BL33 extra1 (REE kernel)
                                              →  BL33 extra2 (REE rootfs)
```

Before each load, BL2 reads the matching FDT node and **overrides**
the compile-time base:

```text
BL32        /reserved-memory/tee-region     TEE
BL33        /reserved-memory/hee-region     Savior HEE
BL33 extra1 /reserved-memory/ree1-region    SOS kernel + DTB
BL33 extra2 /reserved-memory/ree2-region    SOS rootfs
```

`pc = image_base = reg[0] + offset`. After load, BL2 copies the DTB
into each region's base, sets `/memory.reg` to that region, and leaves
`x0` = DTB address in `ep_info.args.arg0`. Extra2 additionally writes
`/chosen linux,initrd-{start,end}` and strips HEE/TEE-only nodes from
the REE tree (and REE-only nodes from the HEE/TEE trees).

Fallback when the FDT nodes are absent (QEMU Savior SPD defaults):

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
instead of Savior — no guest_manager, no SOS/tbox split.

#### (3) BL31: EL3 runtime

BL31 is linked at the top of trusted SRAM. `bl31_main`: platform
setup (console, GIC), `runtime_svc_init` (PSCI + SPD), then either
the SPD's `bl32_init` hook or a direct EL3 exit.

```text
BL31_BASE  = BL_RAM top − 0x30000     ≈ 0x0e020000
BL31_LIMIT = 0x0e050000               SEC_SRAM 0x0e000000 + 0x50000
```

`cm_init_my_context(next_image_info)` + `cm_prepare_el3_exit` program
`ELR_EL3` / `SPSR_EL3` / the NS or S GPRs. The CPU `ERET`s. Savior
does **not** run at EL3. AHYP enters Savior at EL2h; HEE keeps
that EL (`arm64_elX_to_el2`).

#### (4) SPD → TEE, then back

The SPD registers `trusty_init` as `bl32_init`. On the primary CPU:

1. Save NS EL1 sysregs + FP.
2. `cm_init_my_context(SECURE)` with BL32 `ep_info`.
3. SPSR = `EL1h`, DAIF masked. `x0` = TEE DTB (region base, 2 MiB aligned).
4. `ERET` S-EL1 into TEE. TEE runs until the first SMC.
5. SMC helper restores NS context. `bl31_main` continues.

Secondary CPUs take the same path on `CPU_ON` (`svc_on_finish`). After
this point BL31 stays resident: PSCI, SMC to TEE, FIQ forwarding.

#### (5) EL3 exit → Savior boot-shim

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
by the shim (Savior reuses `x1`/`x2` after `boot_shim` returns).

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

#### (7) kernel `_start`: stay EL2 (VHE)

The kernel is still at a physical address. `_start` in `.text.boot`:

1. CPU0 only: save `x0` (BOOTDATA), `x1` (bootargs), `x2` (dram
   `{start,size}`), `CurrentEL`, physical `_start`.
2. `bl arm64_elX_to_el2` (HEE, `WITH_VHE_SUPPORT`). TEE builds
   call `arm64_elX_to_el1` instead — see [`cpu.md`](cpu.md).
3. Enable I/D cache (`SCTLR_EL1.I/C` — VHE EL2&0 view). Secondary
   CPUs wait on `page_tables_not_ready`.
4. Zero `.bss` at the current physical location.
5. `boot_alloc_init`, record `kernel_base_phys = __code_start`.

`arm64_elX_to_el2` does **not** drop exception level:

```text
must already be EL2  (ATF SPSR EL2h)
  init_el2_state
  HCR_EL2 |= E2H | TGE            host looks like EL1
  VTTBR_EL2 = 0                   no stage-2 yet
  ret                             still EL2
```

Kernel code keeps using EL1 names (`TTBR1_EL1`, `VBAR_EL1`,
`SCTLR_EL1`). Under VHE those are the EL2&0 aliases. `HCR.VM` is
**not** set yet — that bit is per-VCPU on guest enter
([`cpu.md`](cpu.md)).

Non-VHE (`venus-tee.mk`) is the older picture: `VBAR_EL2` init
table, `eret` EL1h, later `HVC` into `el2.S`. HEE does not take
that path.

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

`start.S` has already jumped to `KERNEL_BASE`. CPU0 is VHE EL2
(EL1 names), MMU on,
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
"welcome to Savior"
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

Savior also records `el2_cpu_resume` as a physical address for later
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
AHYP  LaunchService("savior.LogService", true)
      LaunchService("fuchsia.virtualization.GuestManager", true)
TEE   LaunchService("savior.GpManager")
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

The VCPU thread is pinned. `zx_vcpu_resume` calls
`arm64_el2_resume` (VHE: a branch, not `HVC`), which loads guest
sysregs, `msr vttbr_el2, stage2_root`, clears `HCR.TGE`, and
`eret`s. That `eret` is the host→guest switch: VHE host → guest
EL1h, stage-2 on, DAIF still masked. First intercept (SMC, WFI,
VGIC, MMIO trap) saves guest state and `br lr` back into
`Vcpu::Resume` on the same thread.

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

After (15) both guests are EL1h under Savior EL2. Further switches
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
0x09000000  PL011 UART0               HEE / Savior
0x09040000  PL011 UART1               ATF console
0x0e000000  trusted SRAM              320 KiB
              0x0e000000  shared / mailbox  4 KiB
              0x0e001000  BL_RAM
              ≈0x0e003000  BL2
              ≈0x0e020000  BL31   (top 192 KiB)
0x0e100000  trusted DRAM              15 MiB (non-Savior BL32 fallback)
0x40000000  NS DRAM                   3 GiB window
              tee-region   BL32 TEE
              hee-region   Savior combo (shim + kernel + BOOTFS)
              ree1-region  SOS DTB + Image
              ree2-region  SOS rootfs
              tbox-reserved  tbox RAM (if the DTB publishes it)
              leftover     SOS system RAM (allocator)
```

`paddr_to_physmap(0x40000000) = 0xffff000040000000` once Savior TTBR1
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

#### Savior HEE window (`hee-region`)

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
Phys images (shim) are PIC, link address 0. The Savior kernel is
linked at a fixed high VA. Guest Linux Images use the ARM64 header
and are not linked by Savior.

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

#### Savior kernel — `kernel.ld`

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

#### Guest Linux Image (not a Savior script)

SOS and tbox kernels are ordinary ARM64 Images. Savior never
relocates them. The VMM only:

- copies the file into guest RAM at the proto `offset`
- points `ELR_EL2` at `offset + 64 KiB`
- leaves `x0` at `offset` (DTB)

The Image header inside those blobs is Linux's, not Savior's.

### Terminology

| Term | Meaning in this note |
| --- | --- |
| ATF | ARM Trusted Firmware. BL1/BL2/BL31. EL3, then PSCI/SMC resident. |
| FIP | Firmware Image Package in flash. BL31 + BL32 + BL33 + extras. |
| BL32 / TEE | Secure EL1 payload. SPD `ERET`s into it before Savior. |
| HEE / Savior | BL33. Zircon hypervisor host. AHYP enters it at EL2. |
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
| BL1 / BL2 / BL31 | `arm-trusted-firmware/bl1/`, `bl2/`, `bl31/` |
| SPD → TEE | BL31 SPD `trusty_init` |
| boot-shim | `zircon/kernel/target/arm64/boot-shim/` |
| `_start` / MMU trampoline | `zircon/kernel/arch/arm64/start.S` |
| VHE stay EL2 | `zircon/kernel/arch/arm64/asm.S` `arm64_elX_to_el2` |
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
| catalog / Launch | `garnet/bin/guest_manager/` |
| Vmm init / VCPU 0 | `garnet/bin/guest/vmm.cc` |

---

Everyday's a Hustle.
