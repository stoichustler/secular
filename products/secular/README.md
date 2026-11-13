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
  kernel-args-forwarder, role_manager, vmm_launcher (lazy VMM),
  guest_manager (eager; Linux/RTOS guests; no guest kernel image yet)
- `/boot/bin`: `sh` (`zxsh`), `vmm`, `cat`, `clear`, `tree`, `dlog`, `debuglog`,
  psutils, `kcounter`, `ktrace`, `loadgen`, `pistress`, `time`, `uname`, `hwstress`
- `/stow`: writable RAM memfs (same level as `/boot` and `/vmm`). Use for
  `ktrace save /stow/trace.fxt`. Summarize with `kdump` (defaults to
  `/stow/*.fxt`). Cleared on reboot. BOOTFS itself is
  read-only.
- `/boot/config/profiles`: bringup scheduler profiles for `role_manager`
- `/boot/meta/secular.txt`: microkernel identity, version, and toolchain stamp
