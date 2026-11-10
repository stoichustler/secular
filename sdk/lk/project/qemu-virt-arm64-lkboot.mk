# LK as BL33 bootloader on QEMU virt arm64.
# Loads the Linux Image (-kernel) and ramdisk (-initrd) from fw_cfg and
# chain-loads them with the arm64 Linux boot protocol.
LK_BOOTLOADER := 1
WITH_SMP := 0
KERNEL_LOAD_OFFSET := 0x20000000
MEMSIZE := 0x40000000

MODULES += \
    app/lkboot \
    lib/version \

include project/target/qemu-virt-arm64.mk
