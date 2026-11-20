# Logging

**`SYS_NEBULA_RAMCONSOLE_DUMP_T` is a whole-SoC dump.** It is
written only when **BL2** kedump runs. A SOS-only abnormal reboot
that restarts the VM and never comes back through BL2 does **not**
produce this file.

| Event | Reboot path | This file in the db |
| --- | --- | --- |
| Hypervisor panic / HWT / SoC WDT | ATF WDT → **BL2** kedump → `expdb` | **yes** |
| SOS kernel panic / SOS hang, VM restart only | PSCI RESET dropped; RESET2 → VCPU stop → VMM | **no** |
| SOS BL33 kedump (`yocto-expdb`) | guest bootloader, `LK_AS_BL33` | **no** (hook not compiled) |

SOS `PSCI SYSTEM_RESET` is canceled in the hypervisor (returns
before any real PSCI). `SYSTEM_RESET2` becomes a VCPU stop packet.
Neither path enters BL2, so neither copies `gz_nebula`.

If a SOS hang kicks a **physical** WDT (passthrough), that *is*
a whole-SoC reset and **does** generate the file. That is HWT, not
"SOS VM reboot".

```text
 SOS panic
    PSCI RESET      ── canceled ──► VM stays / VMM policy
    PSCI RESET2     ── VCPU STOP ─► VMM restarts SOS
         │                            BL2 never runs
         │                            no SYS_NEBULA_RAMCONSOLE_DUMP_T
         ▼
 Crook panic / SoC WDT / HWT
    PSCI WDT_REBOOT (NEBULA_PANIC) / RGU
         │
         ▼
    BL2  ram_console abnormal?
         no  →  normal boot, skip
         yes →  kedump_mini
                 save first 128 KB of gz_nebula
                 name = SYS_NEBULA_RAMCONSOLE_DUMP_T
                 → expdb → later DB zip
```

Two Nebula logs survive a **whole-SoC** crash. They are **not**
the same buffer, and they do not share a compile gate.

| Dump name in the db | What it is | Who writes it in life | Who copies it after KE |
| --- | --- | --- | --- |
| `SYS_NEBULA_LOG_DUMP_T` | hypervisor printk ring (`nebulalog_reserved`, 33 pages) | Crook / SOS `nebulalog` | BL2/BL33 kedump, **unless** standalone |
| `SYS_NEBULA_RAMCONSOLE_DUMP_T` | first 128 KB of `gz_nebula` (hypervisor load region) | Crook ramconsole, just below `bootdata` | **BL2** kedump only |

`MTK_HYP_UOS_TBOX_STANDALONE_RUN` does **not** turn the ramconsole
dump on. It turns the **fwlog** dump **off**. Ramconsole dump is
gated by `LK_AS_BL2`. SOS BL33 (`auto8678p1_64_sos_yct`) sets
standalone and is not BL2: a SOS-only crash writes `yocto-expdb`
with **no** `SYS_NEBULA_*`. The ramconsole file appears only on
the next **whole-SoC** abnormal boot into BL2.

## Framework

```text
 Crook  (gz_nebula, hypervisor image)
    ramconsole ring     64 KB just below bootdata
    printk → ramconsole_write  (wrap)

 SOS  nebulalog_reserved
    hypervisor fwlog    33 pages
    (standalone: not copied into expdb)

        │  whole-SoC KE / WDT / HWT   (not SOS VM restart)
        ▼
 BL2 AEE  kedump_mini
    walk .mboot_expdb hooks
    SYS_NEBULA_RAMCONSOLE_DUMP_T  →  first 128 KB of gz_nebula
    SYS_NEBULA_LOG_DUMP_T         →  nebulalog_reserved  (not standalone)
        │
        ▼  expdb partition  (BL2: expdb / SOS BL33: yocto-expdb)
    ipanic_header
      data_hdr[i].name / offset / used / total
        │
        ▼  next boot  aee_aed  /  USB·PCIe mrdump zip
    DB.*.zip  file named SYS_NEBULA_RAMCONSOLE_DUMP_T
```

The db is a zip. Each `data_hdr` with `valid=1` becomes one file
whose name is the hook name. No extra rename.

## Diagrams

### 1. Two buffers

```text
 DRAM
 ┌─────────────────────────────────────────────────────────────┐
 │  gz_nebula          hypervisor load  (~104 MB @ 0x1340_0000)│
 │  ┌──────────────┐                                           │
 │  │ 128 KB dump  │  ← SYS_NEBULA_RAMCONSOLE_DUMP_T           │
 │  │  ramconsole  │     64 KB ring + 4 KB boot-shim tail      │
 │  │  sits here,  │     just below bootdata                   │
 │  │  then image  │                                           │
 │  └──────────────┘                                           │
 │                                                             │
 │  nebulalog_reserved  0x21000                                │
 │  ┌──────────────┐                                           │
 │  │ fwlog ring   │  ← SYS_NEBULA_LOG_DUMP_T                  │
 │  └──────────────┘     skipped when standalone               │
 └─────────────────────────────────────────────────────────────┘
```

Ramconsole is a circular byte log. Position lives in the first
word (boot-shim) / wrap index (kernel). Dump does **not** unwrap:
it copies the raw 128 KB window from the start of `gz_nebula`.

### 2. Compile gates

```text
                    MTK_HYP_UOS_TBOX_STANDALONE_RUN
                    no                    yes
                 ┌──────────┐         ┌──────────┐
  BL2            │ LOG  on  │         │ LOG  off │
  (LK_AS_BL2)    │ RAMC on  │         │ RAMC on  │
                 └──────────┘         └──────────┘
                                         ▲
                                         │ product BL2 does not
                                         │ set the flag today
                 ┌──────────┐         ┌──────────┐
  BL33 SOS       │ LOG  on  │         │ LOG  off │   ← yct project
                 │ RAMC off │         │ RAMC off │
                 └──────────┘         └──────────┘

  RAMC = SYS_NEBULA_RAMCONSOLE_DUMP_T
  LOG  = SYS_NEBULA_LOG_DUMP_T
```

Product KE db (BL2, flag off) therefore contains **both** files.
Standalone only changes SOS BL33: no Nebula hook, `yocto-expdb`
has no `SYS_NEBULA_*`.

### 3. From hook to db file

```text
  link-time
    AEE_EXPDB_INIT_HOOK(SYS_NEBULA_RAMCONSOLE_DUMP_T, 0x20000, save)
         │  struct { name, size, hook }  in section .mboot_expdb
         ▼
  kedump_to_expdb
    write ipanic_header  (magic, version, blksize, partsize, exp_type)
    reserve one block for the catalog
         │
         ▼
  kedump_platform_debug
    for each hook (VIP DFD first, then the rest):
         hook(plat_dump_write)
         if bytes written > 0:
              kedump_add2hdr(logical_size, written, name)
         │
         ▼
  save_nebula_ramconsole
    lookup mblock "gz_nebula"  (survives reboot via reserved-info)
    map it
    plat_dump_write(pa, 0x20000)  →  expdb, 4 KB trunks
         │
         ▼
  kedump_add2hdr
    next free data_hdr[i]
      name   = "SYS_NEBULA_RAMCONSOLE_DUMP_T"
      offset = where those 128 KB landed
      used   = bytes actually written
      total  = 0x20000
      type   = FILE_NORMAL
      valid  = 1
    write that data_hdr into the catalog immediately (eMMC)

  next boot / USB mrdump
    walk data_hdr[]
    each valid name → one zip entry  →  DB.<reason>.<time>.zip
```

`plat_dump_write` clips to the hook's `size`. A short write still
gets a header; `used < total` means a truncated file in the db.

### 4. kedump only runs on abnormal boot

```text
  reset
    ram_console abnormal?  ──no──►  skip (normal boot)
    expdb already has magic? ──yes─► skip (keep the first exception)
    kedump_skip()?           ──yes─► skip
         │
         ▼
    kedump_to_expdb
      mboot_params / pstore
      platform hooks   ← ramconsole dump lives here
      mini-rdump ELF
      ZAEE_LOG
```

First exception wins. A later panic does not overwrite
`SYS_NEBULA_RAMCONSOLE_DUMP_T`.

### 5. Who fills ramconsole while alive

```text
  boot-shim  (optional)
    4 KB just below bootdata
    first word = bytes written

  Crook kernel
    64 KB − 4 KB  further below bootdata
    every dprintf / print_callback  →  wrap store
    ramconsole_curr_pos is the wrap index

  BL2 dump window
    gz_nebula.base .. +128 KB
    covers that ring if bootdata sits inside gz_nebula
    (hypervisor image is loaded at gz_nebula.base)
```

If `gz_nebula` mblock is missing after reboot, the hook returns
without writing, and **no** `SYS_NEBULA_RAMCONSOLE_DUMP_T` appears
in the header. The db then has no such file — not an empty one.

## Occupancy of the db

Same catalog as every other `SYS_*` file. Order in the zip is
hook walk order (VIP DFD / LAST_CPU_BUS / GPU_DFD first, then
the rest of `.mboot_expdb`, which includes this one). Name is
the identifier string, 31 bytes max.

USB / PCIe full mrdump builds the same zip: `SYS_COREDUMP` plus
every valid `data_hdr` from expdb, including
`SYS_NEBULA_RAMCONSOLE_DUMP_T`.

## Standalone, in one picture

```text
  flag on SOS BL33 only today

  whole-SoC abnormal reset         SOS-only abnormal reset
  ─────────────────────────        ────────────────────────────
  BL2 kedump  →  expdb             VM restart / BL33 kedump
    RAMCONSOLE  yes                  RAMCONSOLE  no
    LOG         yes                  LOG         no  (standalone)
    → db has                         → yocto-expdb has no
      SYS_NEBULA_RAMCONSOLE_DUMP_T     SYS_NEBULA_*
      SYS_NEBULA_LOG_DUMP_T
```

Do not look for `SYS_NEBULA_RAMCONSOLE_DUMP_T` in a SOS-only db.
It is a BL2 whole-SoC dump file.

## Pitfalls

- Standalone ≠ ramconsole dump. Standalone hides **fwlog**.
- BL33 never registers the ramconsole hook. SOS VM restart
  never runs BL2 kedump.
- Physical WDT from SOS (passthrough HWT) **is** whole-SoC
  and **does** generate the file. Guest PSCI reboot does not.

- Dump is a raw window, not an unwrapped ring. Wrap index is
  inside the buffer; readers must reconstruct.
- Missing `gz_nebula` reserved-info → silent skip, no file.
- First exception in expdb is sticky until something erases it.

## Guest restart (proposal)

Scope: **SOS and tbox only.** Android is out. Do **not** map
`gz_nebula` into guests. Do **not** turn a guest panic into a
whole-SoC reset.

Today the file is BL2-only. To also get it on SOS / tbox **VM**
abnormal reboot: hypervisor snapshot + BL33 hook. Both guests
already compile `nebulalog_expdb.c` (mt8678 LK).

```text
  SOS / tbox  panic
        PSCI RESET / RESET2
        │
        ▼
  Crook  (before VCPU_STOP)
        snapshot[vmid]  ←  128 KB ramconsole  (frozen)
        guest-visible reserved mblock, RO
        SOS = slot 1, tbox = slot 3
        │
        ▼
  that VM's BL33 kedump
        SYS_NEBULA_RAMCONSOLE_DUMP_T
        reads snapshot[vmid], not gz_nebula
        →  SOS: yocto-expdb
           tbox: expdb_uos_tbox
```

Whole-SoC BL2 path stays as it is (still dumps `gz_nebula`).

Work:

1. Hypervisor: reserved snap[SOS]+snap[tbox], fill on RESET/RESET2
   before VCPU_STOP; stage-2 RO map into those two guests.
2. Yocto LK hook: BL2 **or** BL33. BL2 dumps `gz_nebula`. BL33
   dumps `snapshot[VM_ID]`. Independent of standalone (that flag
   only hides fwlog).
3. Timing: snapshot before Stop; guest `mboot_params` must still
   look abnormal so BL33 `kedump_mini` runs. If VMM skips BL33,
   add the same dump to that guest's kernel aee.

Not in scope: ALPS / Android LK, Android `expdb`.
