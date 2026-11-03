# Zephyr RTOS guest image (vmid 0)

- 2x Cortex-A53, SMP
- 72 MiB guest RAM (Image linked at `0x2080000`, matching VMM load address)
- virtio-console (PCI slot 1, modern virtio 1.0 device id `0x1043`)
- serial console + Zephyr shell (`kernel`, `device`, …)
- PL011 present as a fallback UART

Launch on the Secular console:

```text
vmm launch vmid=0
```
