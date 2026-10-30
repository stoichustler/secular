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
  kernel-args-forwarder, role_manager
- `/boot/bin`: `sh` (`zxsh`), `cat`, `clear`, `tree`, `dlog`, `debuglog`, psutils,
  `kcounter`, `ktrace`, `loadgen`, `pistress`, `time`, `uname`, `hwstress`
- `/boot/config/profiles`: bringup scheduler profiles for `role_manager`
