# Zephyr RTOS guest image (vmid 0)

`zephyr.bin` is an ARM64 Linux `Image` built from `~/clot/zephyr` for the
Secular Machina VMM:

- 2x Cortex-A53, SMP
- 72 MiB guest RAM (Image linked at `0x2080000`, matching VMM load address)
- virtio-console (PCI slot 1, modern virtio 1.0 device id `0x1043`)
- serial console + Zephyr shell (`kernel`, `device`, …)
- PL011 present as a fallback UART

Rebuild:

```sh
# CROSS_COMPILE must be aarch64-*-gcc. Default: $HOME/aarch64-tc/aarch64-linux-gnu-
products/prebuilt/rtos/rebuild-zephyr.sh
```

Launch on the Secular console:

```text
guest launch vmid=0
```
