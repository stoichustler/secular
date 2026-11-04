# Linux guest image (vmid 1)

- Linux 7.3.1 ARM64 `Image` (uncompressed, magic `ARM\x64`; no net/USB/Bluetooth/GPU/sound)
- minifs initramfs (`minifs.cpio`, Alpine 3.24 busybox/musl)
- 4x vCPU, 512 MiB guest RAM
- virtio-console (`hvc0`) + PL011 fallback (`ttyAMA0`)
- Kernel cmdline: `earlycon=pl011,0x808300000 console=hvc0 rdinit=/init`
- Interactive shell on virtio-console `hvc0` (`getty`, job control), green prompt `Linux $ `

Launch on the Secular console:

```text
vmm launch vmid=1
```

Rebuild from `~/clot/linux-7.3.1` and `~/clot/minifs`:

```sh
export ARCH=arm64 CROSS_COMPILE=aarch64-linux-gnu-
BUILD=/tmp/linux-machina-build
make -C ~/clot/linux-7.3.1 O=$BUILD defconfig
make -C ~/clot/linux-7.3.1 O=$BUILD virt.config
~/clot/linux-7.3.1/scripts/kconfig/merge_config.sh -m -O $BUILD \
  $BUILD/.config products/prebuilt/linux/machina.config
make -C ~/clot/linux-7.3.1 O=$BUILD olddefconfig Image
cp $BUILD/arch/arm64/boot/Image products/prebuilt/linux/Image

( cd ~/clot/minifs && find . -print0 | cpio --null --create --format=newc ) \
  > products/prebuilt/linux/minifs.cpio
```
