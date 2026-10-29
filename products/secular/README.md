# Secular product

GN image for qemu-arm64:

```text
//products/secular:secular      secular.zbi
//products/secular:boot_shim    linux-arm64-boot-shim.bin
```

The ZBI is a bringup-class system assembled without Bazel:

- Zircon `kernel.eng` (userboot + vDSO)
- `component_manager` and its boot config
- BOOTFS packages: root, bootstrap, console, console-launcher,
  critical-services, ptysvc, archivist, shutdown-shim,
  kernel-args-forwarder
- `/boot/bin/sh` (`zxsh`)
