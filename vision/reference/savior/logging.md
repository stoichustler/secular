# Logging

**After this:** you can say why `SYS_NEBULA_RAMCONSOLE_DUMP_T` is
missing. **First read:** the dump table. **Next:** [`glossary.md`](glossary.md).

Read [`README.md`](README.md) for the product map.

## Principle

1. **Two rings, two dump files.** Ramconsole lives in `gz_nebula`
   (hypervisor image). Fwlog lives in `nebulalog_reserved`.
   BL2 kedump copies them under different `SYS_*` names.

2. **Ramconsole dump is BL2-only.** `SYS_NEBULA_RAMCONSOLE_DUMP_T`
   appears only on a **whole-SoC** abnormal boot into BL2.
   SOS VM relaunch never runs BL2.

3. **Standalone hides fwlog, not ramconsole.**
   `MTK_HYP_UOS_TBOX_STANDALONE_RUN` turns `SYS_NEBULA_LOG_DUMP_T`
   off. Ramconsole dump is gated by `LK_AS_BL2`.

4. **Guest RESET snapshot is a different slot.**
   `ramconsole_snapshot_take` copies 4 KiB into the **tail** of
   `gz_nebula`. BL2 still dumps the **first 128 KiB**.

5. **Dump does not unwrap because kedump is a byte copy.**
   BL2 `dev_write`s a window of `gz_nebula`. It does not know
   `curr_pos` / `log_rb.put`. Reconstructing the ring is a
   **host-side** job (this handbook’s unwrap recipe). Putting
   unwrap in BL2 would couple kedump to Savior’s ring format
   across every bootloader branch.

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
 Savior panic / SoC WDT / HWT
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

Two Savior logs survive a **whole-SoC** crash. They are **not**
the same buffer, and they do not share a compile gate.

| Dump name in the db | What it is | Who writes it in life | Who copies it after KE |
| --- | --- | --- | --- |
| `SYS_NEBULA_LOG_DUMP_T` | hypervisor printk ring (`nebulalog_reserved`, 33 pages) | Savior / SOS `nebulalog` | BL2/BL33 kedump, **unless** standalone |
| `SYS_NEBULA_RAMCONSOLE_DUMP_T` | first 128 KB of `gz_nebula` (hypervisor load region) | Savior ramconsole, just below `bootdata` | **BL2** kedump only |

`MTK_HYP_UOS_TBOX_STANDALONE_RUN` does **not** turn the ramconsole
dump on. It turns the **fwlog** dump **off**. Ramconsole dump is
gated by `LK_AS_BL2`. SOS BL33 (`auto8678p1_64_sos_yct`) sets
standalone and is not BL2: a SOS-only crash writes `yocto-expdb`
with **no** `SYS_NEBULA_*`. The ramconsole file appears only on
the next **whole-SoC** abnormal boot into BL2.

## Framework

```text
 Savior  (gz_nebula, hypervisor image)
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

**How to unwrap a dump** (`ramconsole.c`):

```text
 gz_nebula + 0
   bootshim  4 KiB   word0 = bootshim_write_chars  (chars written, not wrap)
             rest    boot-shim bytes
   kernel    60 KiB  ramconsole_curr_pos wraps at ramconsole_sz (0xF000)
             write_chars = curr_pos after last byte
 BL2 copies 128 KiB from gz_nebula start  (includes both rings + slack)

 reconstruct kernel ring:
   if write_chars never wrapped (curr_pos never hit sz then 0)
      linear [0, curr_pos)
   else
      [curr_pos, sz) then [0, curr_pos)     oldest → newest
```

`ramconsole_snapshot_take`: memcpy **min(64 KiB, 4 KiB)** of the
**live kernel ring** (not unwrapped) into
`gz_nebula_end - 4 KiB`. Last RESET wins. BL2 kedump still uses
the **head** 128 KiB, not this tail.

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

**fwlog wrap** (`memlog.cpp` / `trusty-log.h`), not ramconsole:

```text
 struct log_rb { alloc, put, sz, data[0] }   sz power-of-two
 producer  (Savior print_callback)
    log_offset = alloc;  alloc += len
    data[(log_offset+i) & (rb_sz-1)] = byte
    put += len
 consumer  (Linux / kedump)
    read [put, alloc) around the ring
    put == alloc  →  empty
```

`nebulalog_reserved` (33 pages) holds this rb. Standalone skips
`SYS_NEBULA_LOG_DUMP_T`; the ring still runs. Dump is the raw
buffer, same as ramconsole: kedump does **not** unwrap.

Product KE db (BL2, flag off) therefore contains **both** files.
Standalone only changes SOS BL33: no Savior hook, `yocto-expdb`
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

  Savior kernel
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
- SOS VM restart never runs BL2 kedump. BL33 may register the
  ramconsole hook when standalone / tbox / android hypervisor
  configs are set (`nebulalog_expdb.c`). The dump is still the
  head of `gz_nebula`, not the 4 KiB tail snapshot.
- Physical WDT from SOS (passthrough HWT) **is** whole-SoC
  and **does** generate the file. Guest PSCI reboot does not.

- Dump is a raw window, not an unwrapped ring. Wrap index is
  inside the buffer; readers must reconstruct.
- Missing `gz_nebula` reserved-info → silent skip, no file.
- First exception in expdb is sticky until something erases it.

## Guest restart snapshot (shipped)

`ramconsole_snapshot_take(vmid)` already runs on `PSCI SYSTEM_RESET`
and `SYSTEM_RESET2` for SOS, ALPS, and tbox. It is **not** a
proposal.

```text
  any of SOS / ALPS / tbox  PSCI RESET / RESET2
        │
        ▼
  Savior  ramconsole_snapshot_take
        copy min(live ring, 4 KiB) into the **tail** of gz_nebula
        one shared slot; last reset wins
        then ZX_ERR_CANCELED (RESET) or GUEST_VCPU_STOP (RESET2)
```

BL2/BL33 kedump still copies the **first 128 KiB of `gz_nebula`**,
not this tail snapshot. Indep lua maps `gz_nebula` into Android/tbox
(outside their RAM mblock); SOS must **not** `setVmemAuto("gz_nebula")`
(overlaps the RAM identity map). See
`patches-kedump-nebula-ramconsole/patches/README.md`.

---

Prev: [`security.md`](security.md) · Next: [`glossary.md`](glossary.md) · [`README.md`](README.md)

Everyday's a Hustle.
