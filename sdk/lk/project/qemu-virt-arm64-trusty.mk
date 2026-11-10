# Trusty TEE OS as ATF BL32 (S-EL1) on QEMU virt arm64.
# EL3 ATF SPD=trusty enters this image. lib/sm yields SMC_SC_NS_RETURN
# so EL3 can start lkboot (BL33).
LK_TRUSTY_TEE := 1
WITH_SMP := 1
SMP_MAX_CPUS := 8
WITH_TRUSTY_IPC := 1
WITH_TRUSTY_VIRTIO_IPC_DEV := false
WITH_TRUSTY_QL_TIPC_DEV := true
KERNEL_LOAD_OFFSET := 0
MEMSIZE := 0x00e00000

MODULES += \
    app/trusty \
    lib/version \

include project/target/qemu-virt-arm64.mk
