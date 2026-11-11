# Trusted Firmware-A

Secular uses in-tree TF-A as QEMU virt EL3 firmware. QEMU loads
`secular.bios` as `-bios`. This note is the map of that firmware: each
Boot Loader (BL) stage, the exception-level switches from reset to
lkboot, and the **physical** layout `plat/qemu` publishes so BL32 / BL33
can be placed.

The product stays:

```text
zircon kernel + linux-arm64-boot-shim + BOOTFS + serial console + zxsh
```

Read [`bootstrap.md`](../../products/reference/zircon/bootstrap.md) for
EL2 shim → `zxsh`, [`sdk/lk/README.md`](../lk/README.md) for Trusty and
lkboot, [`userspace/secular/bin/README.md`](../../userspace/secular/bin/README.md)
for NS probes into S-EL1. This note stops when BL31 has `ERET`ed to
BL33 (lkboot at EL2).

Upstream: <https://trustedfirmware-a.readthedocs.io/> (BSD-3-Clause).
Do not clone a toolchain into this tree; host tools come from
`SECULAR_SDK`.

## Role in Secular

### Framework

Three worlds, one QEMU virt. ATF owns EL3 for the life of the machine.
BL32 is LK Trusty. BL33 is LK lkboot. The Linux Image and ZBI still
arrive via `-kernel` / `-initrd`; lkboot chain-loads them.

```text
 QEMU virt  (secure=on, GICv3)
   -bios  secular.bios     BL1 + FIP (BL2, BL31, BL32, BL33)
   -kernel linux-arm64-boot-shim.bin
   -initrd secular.zbi
              │
              ▼
 ┌────────────────────────────────────────────────────────────────┐
 │ EL3  BL1  flash 0x00000000                                     │
 │  Booting SECULAR ATF at EL3                                    │
 │  copy RW to secure SRAM, load BL2 from FIP, ERET BL2           │
 └────────────────────────────┬───────────────────────────────────┘
                              ▼
 ┌────────────────────────────────────────────────────────────────┐
 │ EL3  BL2  secure SRAM                                          │
 │  load BL31 / BL32 / BL33 from FIP, pass ep_info, ERET BL31     │
 └────────────────────────────┬───────────────────────────────────┘
                              ▼
 ┌────────────────────────────────────────────────────────────────┐
 │ EL3  BL31  runtime (never returns)                             │
 │  GIC, PSCI (NS only), Trusty SPD                               │
 │  1. register SMC UUID / PSCI SPD hooks                         │
 │  2. enter BL32                                                 │
 └───────────────┬───────────────────────────────┬────────────────┘
                 ▼                               │
 ┌───────────────────────────────────────────┐   │
 │ S-EL1  BL32  LK Trusty                    │   │
 │  0x0e100000  app/trusty + lib/sm          │   │
 │  SMC_YC_NS_RETURN back to EL3             │   │
 └───────────────────────┬───────────────────┘   │
                         ▼                       ▼
                    EL3  SPD restores NS, ERET BL33
                         │
                         ▼
 ┌────────────────────────────────────────────────────────────────┐
 │ NS EL2  BL33  LK lkboot   0x60000000                           │
 │  firmware kernel + ramdisk → linux-arm64-boot-shim             │
 └────────────────────────────────────────────────────────────────┘
```

| Image | Role | EL | Loaded from | Runs until |
| --- | --- | --- | --- | --- |
| **BL1** | AP Trusted ROM. First code after reset. | EL3 | flash (`bl1.bin`) | hands off to BL2 |
| **BL2** | Trusted Boot Firmware. Loads the rest. | EL3 | FIP | reclaimed after BL31 starts |
| **BL31** | EL3 Runtime Firmware. Monitor. | EL3 | FIP | always resident |
| **BL32** | Secure-EL1 Payload. Trusty TEE. | S-EL1 | FIP → secure DRAM | yields to EL3, stays mapped |
| **BL33** | Non-secure world bootloader. lkboot. | NS EL2 | FIP → NS DRAM | chain-loads the shim |

BL2U, in-tree TSP / OP-TEE / `sp_min`, RME, and Trusted Board Boot are
**not** part of this product. `SPD=trusty` dispatches to the external
LK Trusty image; ATF's own `bl32/` payloads are unused.

### Flow

Each box is a **switch node**: exception level, security state, or the
resident firmware image changes here.

```text
 reset  (QEMU -bios secular.bios, PC = 0)
         │
         ▼  (1) BL1  EL3, flash
 Booting SECULAR ATF at EL3
 copy RW to SRAM, load BL2, ERET BL2
         │
         ▼  (2) BL2  EL3, SRAM
 load BL31 + BL32 + BL33 from FIP
         │
         ▼  (3) BL31  EL3 runtime
 GIC, runtime_svc_init (PSCI + Trusty SPD)
         │
         ▼  (4) SPD → Trusty  EL3 → S-EL1
 ERET BL32 at 0x0e100000, SPSR EL1
         │
         ▼  (5) Trusty init  S-EL1
 lib/sm idle, SMC_YC_NS_RETURN
         │
         ▼  (6) EL3 exit → lkboot  NS EL2
 SPSR EL2, PC = 0x60000000, x0 = DTB
         │
         ▼
 lkboot → linux-arm64-boot-shim → secular.zbi
   (see products/reference/zircon/bootstrap.md)
```

| Node | EL | Security | Who runs |
| --- | --- | --- | --- |
| (1) BL1 | EL3 | monitor | flash ROM, then SRAM RW |
| (2) BL2 | EL3 | monitor | loader in SRAM |
| (3) BL31 | EL3 | monitor | PSCI + SPD, stays |
| (4)–(5) BL32 | S-EL1 | `SCR_EL3.NS=0` | LK Trusty |
| (6) BL33 | NS EL2 | `SCR_EL3.NS=1` | LK lkboot |

Three things change along this path:

| Dimension | Starts as | Ends as (this note) |
| --- | --- | --- |
| Exception level | EL3 | NS EL2 (lkboot) |
| Security state | reset (monitor) | NS, with S-EL1 Trusty parked |
| Resident firmware | BL1 in flash | BL31 in SRAM + BL32 in secure DRAM |

## Switch nodes

### (1) BL1 — AP Trusted ROM

`bl1/`. First instruction after reset. QEMU maps `secular.bios` at
flash `0x00000000`. BL1:

- prints `Booting SECULAR ATF at EL3` (`CurrentEL` is 3; EL3 is the
  monitor, never `S-EL3`)
- copies its RW section into secure SRAM
- finds the FIP at flash offset `0x40000` (see [secular.bios](#secularbios))
- loads BL2 into SRAM and `ERET`s to it

BL1 is not the runtime monitor. After BL31 starts, BL1 flash is idle.

### (2) BL2 — Trusted Boot Firmware

`bl2/`. The only job is to load later images and pass control. The
SRAM it occupies is reclaimed by BL31.

From the FIP it loads:

| ID | Destination |
| --- | --- |
| BL31 | secure SRAM, top of `BL_RAM` |
| BL32 | secure DRAM `0x0e100000` |
| BL33 | NS DRAM `NS_IMAGE_OFFSET` `0x60000000` |

Then `ERET` to BL31 with `ep_info` for BL32 and BL33.

### (3) BL31 — EL3 Runtime Firmware

`bl31/`. This is the monitor for the rest of the boot and for runtime
SMCs. It never returns to BL1/BL2.

Cold boot on CPU0:

1. platform / GIC setup
2. `runtime_svc_init()` — Standard Service (PSCI) and Trusty SPD
3. `bl31_register_bl32_init(trusty_init)` → enter S-EL1
4. after Trusty yields, prepare NS context and `ERET` BL33

Runtime:

| Path | Who |
| --- | --- |
| PSCI (`0x8400000x`) | `psci_smc_handler`, **NS callers only** |
| Trusty UUID / yielding SMC | `services/spd/trusty/trusty.c` |
| CPU_ON / OFF / SUSPEND | PSCI, then `psci_register_spd_pm_hook` into Trusty |

A PSCI call from S-EL1 returns `SMC_UNK` (`-1`). Trusty must not probe
PSCI; CPU bring-up of TEE cores is the SPD hook, not a PSCI client.

### (4)–(5) BL32 — Secure-EL1 Payload (Trusty)

Not the in-tree `bl32/` directory. Secular packs
`qemu-virt-arm64-trusty` (`sdk/lk`, `app/trusty` + `lib/sm`) as BL32.

ATF Trusty SPD:

- peeks the image, sets SPSR to EL1 (`SPSR_64(MODE_EL1, …)`)
- `ERET` to `0x0e100000` with `SCR_EL3.NS=0` → **S-EL1**
- Trusty inits, then `SMC_YC_NS_RETURN`
- SPD restores NS context; BL32 stays resident in secure DRAM

S-EL0 TAs are later. This product's BL32 is the TEE OS only.

### (6) BL33 — Non-secure bootloader (lkboot)

`qemu-virt-arm64-lkboot` at `0x60000000`. ATF is built with
`ARM_LINUX_KERNEL_AS_BL33=1`, so the NS entry uses the Linux arm64
protocol (`x0` = DTB). lkboot stays at EL2 (VHE) and chain-loads the
shim. From there the path is
[`bootstrap.md`](../../products/reference/zircon/bootstrap.md).

## Physical layout

QEMU virt, TrustZone secure DRAM/SRAM as `plat/qemu/qemu/include/platform_def.h`.

```text
 PA
 0x00000000 ┬ flash / SEC_ROM  128 KiB
            │  BL1 RO
 0x00040000 ├ FIP              BL2 + BL31 + BL32 + BL33
            │
 0x08000000 ├ GICv3
 0x09000000 ├ PL011 UART
            │
 0x0e000000 ├ SEC_SRAM  1 MiB
            │  shared 4 KiB, then BL2 / BL31 / BL1 RW
 0x0e100000 ├ SEC_DRAM  15 MiB
            │  BL32 Trusty (LK uses 14 MiB, 2 MiB aligned)
            │
 0x40000000 ▼ NS DRAM
            │  DTB, -kernel shim, -initrd ZBI
 0x60000000 ├ BL33 lkboot     (NS_IMAGE_OFFSET)
```

| Occupant | PA | Size window | Source |
| --- | --- | --- | --- |
| BL1 RO | `0x00000000` | 128 KiB (`SEC_ROM`) | `bl1.bin` |
| FIP | `0x00040000` | rest of the BIOS image | `fip.bin` |
| Secure SRAM | `0x0e000000` | 1 MiB | BL1 RW, BL2, BL31 |
| Trusty BL32 | `0x0e100000` | 14 MiB used / 15 MiB region | LK `qemu-virt-arm64-trusty` |
| DTB | `0x40000000` | 64 KiB typical | ATF → NS |
| linux-arm64-boot-shim | QEMU `-kernel` | Linux Image rules | not in the FIP |
| secular.zbi | QEMU `-initrd` | ramdisk | not in the FIP |
| lkboot BL33 | `0x60000000` | `NS_IMAGE_OFFSET` | LK `qemu-virt-arm64-lkboot` |

BL32 lives in **secure DRAM** (`BL32_RAM_LOCATION=tdram`). BL31 lives
at the top of **secure SRAM** (`BL31_BASE = BL31_LIMIT - 0x70000`).
BL2 sits immediately below BL31 (`BL2_BASE = BL31_BASE - 0x35000`).

## secular.bios

`plat/qemu/qemu/platform.mk`:

```text
secular.bios: bl1 fip
        cp bl1.bin secular.bios
        dd if=fip.bin of=secular.bios bs=64k seek=4
```

So the BIOS is BL1 at offset 0 and the FIP at `4 × 64 KiB = 0x40000`.
`make atf` copies it to
`out/secular-boot-arm64/firmware/arm-trusted-firmware-qemu-bios/secular.bios`.

QEMU `-bios` loads that file into flash. BL1 executes in place; it
reads the FIP from the same flash.

## Build

From the Secular source root:

```sh
export SECULAR_SDK=/absolute/path/to/secular-sdk   # default: ../secular-sdk
make lkboot     # BL32 Trusty + BL33 lkboot
make atf        # pack them into secular.bios
make run        # QEMU, -bios that image
```

`tools/secular-boot/build-atf.sh` invokes:

```text
make -C sdk/atf \
  PLAT=qemu ARCH=aarch64 SPD=trusty \
  NEED_BL32=yes ARM_LINUX_KERNEL_AS_BL33=1 \
  QEMU_USE_GIC_DRIVER=QEMU_GICV3 \
  BL32=<trusty lk.bin> BL33=<lkboot lk.bin> \
  secular.bios
```

| File | Meaning |
| --- | --- |
| `out/secular-boot-arm64/atf/qemu/release/bl1.bin` | BL1, start of the BIOS |
| `out/secular-boot-arm64/atf/qemu/release/fip.bin` | BL2 + BL31 + BL32 + BL33 |
| `out/secular-boot-arm64/firmware/arm-trusted-firmware-qemu-bios/secular.bios` | QEMU `-bios` |

Do not add `prebuilt/` under this directory.

## Layout

| Path | Role |
| --- | --- |
| `bl1/` | AP Trusted ROM |
| `bl2/` | image loader (reclaimed) |
| `bl2u/` | FWU updater — unused here |
| `bl31/` | EL3 runtime, PSCI |
| `bl32/` | in-tree SP payloads — unused; Trusty is `sdk/lk` |
| `services/spd/trusty/` | EL3 dispatcher for BL32 |
| `lib/psci/` | PSCI, NS only |
| `plat/qemu/` | virt memory map, `secular.bios` rule |
| `common/` | runtime_svc, logging, image load |
