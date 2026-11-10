# LK (Little Kernel)

This directory is Little Kernel, used in Secular as two ATF payloads on
QEMU virt:

- **BL32** Trusty TEE (`qemu-virt-arm64-trusty`, `app/trusty` + `lib/sm`) in
  S-EL1 at secure DRAM `0x0e100000`
- **BL33** bootloader (`qemu-virt-arm64-lkboot`, `app/lkboot`) at
  `0x60000000`

lkboot loads the Linux Image and ramdisk from firmware and chain-loads
them with the arm64 Linux boot protocol.

Upstream: <https://github.com/littlekernel/lk> (MIT). Do not clone a
toolchain into this tree; host tools come from `SECULAR_SDK`.

Build internals, module rules, and coding conventions are in
[`AGENTS.md`](AGENTS.md).

## Role in Secular

```text
QEMU (qemu-system-aarch64, virt, GICv3, secure=true, -cpu max)
  │
  ▼
ATF  BL1 / BL2 / BL31          -bios secular.bios (SPD=trusty)
  ├── LK BL32  app/trusty      secure DRAM 0x0e100000
  └── LK BL33  app/lkboot      lk.bin at 0x60000000
  │                            firmware kernel  = linux-arm64-boot-shim
  │                            firmware ramdisk = zircon.zbi / secular.zbi
  ▼
linux-arm64-boot-shim          QEMU -kernel
  │
  ▼
physboot → vmzircon            QEMU -initrd (ZBI)
  │
  ▼
userboot → component_manager → serial console (zxsh)
```

The supported product projects are **`qemu-virt-arm64-lkboot`** (BL33)
and **`qemu-virt-arm64-trusty`** (BL32). Other `project/*.mk` files
remain from upstream LK; Secular does not build or boot them.

`app/lkboot` follows MediaTek `mt_boot`: load kernel + ramdisk, write
`/chosen` `linux,initrd-*`, then jump. QEMU ATF has no
`MTK_SIP_KERNEL_BOOT`, so LK stays at EL2 (VHE) and `arch_chain_load`
enters the shim directly. That matches ATF BL33 entry and lets Zircon
physload run `ArmDropToEl1` at EL2.

## DRAM layout (QEMU virt)

`MEMBASE` is `0x40000000`. `qemu-virt-arm64-lkboot` sets
`KERNEL_LOAD_OFFSET=0x20000000` (LK physical `0x60000000`, ATF
`NS_IMAGE_OFFSET`) and `MEMSIZE=0x40000000`. `WITH_SMP=0`.
`LK_BOOTLOADER=1` omits PCI/virtio.

| Physical address | Contents                                      | Size window |
| ---------------- | --------------------------------------------- | ----------- |
| `0x0e100000`     | Trusty TEE (`lk.bin`, BL32)                   | 14 MiB      |
| `0x40000000`     | DTB from ATF                                  | 64 KiB      |
| `0x40200000`     | linux-arm64-boot-shim (ARM64 Linux Image)     | 16 MiB max  |
| `0x44000000`     | zircon.zbi / secular.zbi (ramdisk)            | 64 MiB max  |
| `0x60000000`     | LK lkboot (`lk.bin`, BL33)                    | from FIP    |

QEMU still places the shim and ZBI via `-kernel` / `-initrd` (firmware
config). LK is the only firmware consumer; `app/lkboot` talks to
`firmware_kernel_*` / `firmware_ramdisk_*`, not fw_cfg selectors.

## Build

From the Secular source root (preferred):

```sh
export SECULAR_SDK=/absolute/path/to/secular-sdk   # default: ../secular-sdk
make lkboot     # BL33 lkboot + BL32 trusty → out/secular-boot-arm64/lk/...
make atf        # pack Trusty BL32 + lkboot BL33 → secular.bios
make build      # console image + LK + ATF
make run        # interactive QEMU serial console
make kick       # boot existing image (no rebuild)
```

`tools/secular-boot/build-lkboot.sh` invokes:

```text
make -C sdk/lk qemu-virt-arm64-lkboot \
  TOOLCHAIN=clang LD=ld.lld \
  CLANG_BINDIR=<sdk clang> \
  BUILDROOT=out/secular-boot-arm64/lk \
  IGNORE_LOCAL_MK=1
```

Outputs:

| File | Meaning |
| ---- | ------- |
| `out/secular-boot-arm64/lk/build-qemu-virt-arm64-lkboot/lk.bin` | BL33 lkboot packed into the FIP |
| `out/secular-boot-arm64/lk/build-qemu-virt-arm64-trusty/lk.bin` | BL32 Trusty TEE packed into the FIP |
| `out/secular-boot-arm64/firmware/arm-trusted-firmware-qemu-bios/secular.bios` | ATF + Trusty + lkboot, used as QEMU `-bios` |

Do not run `scripts/fetch-toolchains.py` here. Do not add `prebuilt/`
under this directory.

## Boot path in this tree

1. `app/lkboot/lkboot.c` — `boot_linux_from_firmware()` then
   `boot_linux_fdt()`. Fills `/chosen` `linux,initrd-start/end`, checks
   the Image magic `0x644d5241` at offset `0x38`, then
   `arch_chain_load(shim, dtb, 0, 0, 0)`.
2. `app/lkboot/firmware.c` — QEMU firmware-config backend (selectors
   stay in this file).
3. `arch/arm64/start.S` — with `LK_BOOTLOADER`, `arm64_stay_el2`
   (HCR_EL2 `{RW,E2H,TGE}`, ICC_SRE_EL2) instead of dropping to EL1.
4. `arch/arm64/arch.c` / `asm.S` — identity-map trampoline, disable
   MMU and caches, branch to the shim at EL2.

A successful serial trace includes:

```text
trusty: EL3 SPD ready, S-EL1 BL32 entry ...
trusty: Found 64 bit S-EL1 image
welcome to trusty TEE 🅢
TEE: S-EL1 services started
TEE: return to EL3, then lkboot BL33
welcome to lkboot 🅚
ARM64: boot EL2, current EL2 (EL2 VHE host)
lkboot: firmware kernel=linux-arm64-boot-shim ramdisk=zircon.zbi
lkboot: jumping to secular [EL2]
lk finished --> jump to linux kernel 64Bit
physboot: Handing off at physical load address ...
starting user space
secular $
ktee: spd uuid 40ee25f0-a2bc-304c-8c4c-a173c57d8af1
ktee: s-el1 nop -> 0x54454521
TEE: alive
```

## Layout

| Path | Role |
| ---- | ---- |
| `app/lkboot/` | BL33 bootloader (Secular product path) |
| `app/trusty/` | BL32 Trusty OS (`lib/sm`, `lib/trusty`, services) |
| `arch/arm64/` | EL2 stay, `arch_chain_load` trampoline, Trusty S-EL1 entry |
| `platform/qemu-virt-arm/` | QEMU virt; `LK_BOOTLOADER` / `LK_TRUSTY_TEE` skip PCI/virtio |
| `project/qemu-virt-arm64-lkboot.mk` | BL33 product project |
| `project/qemu-virt-arm64-trusty.mk` | BL32 Trusty TEE project |
| `engine.mk`, `make/` | Modular make engine |
| `AGENTS.md` | Build system, modules, style |

## License

LK is released under the [MIT license](LICENSE). Some third-party code
under `external/` carries its own license.
