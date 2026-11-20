---
name: zircon-console-maps
description: >
  Index of Secular Zircon console maps. Use when changing boot, MMU, CPU,
  GIC, IPC, async loops, or kernel locks on qemu-system-aarch64 virt.
---

# Zircon console maps

Read the matching map **before** editing that area. Each file is a
Secular skill: YAML frontmatter, **When to use**, numbered sections,
box-drawing diagrams (`┌┐└┘│─`), **Anti-patterns**, and a first-pass
checklist.

The kernel is an EL2 VHE host. The image is:

```text
zircon kernel + linux-arm64-boot-shim + BOOTFS + serial console + zxsh
```

## When to use

| Skill | Map | Read when |
| --- | --- | --- |
| [`zircon-bootstrap`](../../../tools/ai-coding/skills/zircon-bootstrap/SKILL.md) | [`bootstrap.md`](bootstrap.md) | ATF, Trusty, lkboot, physboot, userboot, QEMU `-bios` |
| [`zircon-memory`](../../../tools/ai-coding/skills/zircon-memory/SKILL.md) | [`memory.md`](memory.md) | page tables, physmap, PMM, VMO/VMAR, page fault, cache, ASID |
| [`zircon-cpu`](../../../tools/ai-coding/skills/zircon-cpu/SKILL.md) | [`cpu.md`](cpu.md) | thread, syscall, idle, SMP, VCPU |
| [`zircon-interrupt`](../../../tools/ai-coding/skills/zircon-interrupt/SKILL.md) | [`interrupt.md`](interrupt.md) | GICv3, SYNC, vGIC |
| [`zircon-ipc`](../../../tools/ai-coding/skills/zircon-ipc/SKILL.md) | [`ipc.md`](ipc.md) | handles, channel, FIDL |
| [`zircon-async`](../../../tools/ai-coding/skills/zircon-async/SKILL.md) | [`async.md`](async.md) | port, `async_loop`, `fuchsia_async` |
| [`zircon-sync`](../../../tools/ai-coding/skills/zircon-sync/SKILL.md) | [`sync.md`](sync.md) | barriers, locks, WaitQueue, futex |

## When not to use

* Do not copy Fuchsia products (`core`, Starnix, netstack) into this tree.
* Do not vendor the toolchain here. Host tools come from `SECULAR_SDK`.

## First-pass checklist

- [ ] Identify which map owns the change
- [ ] Read **When to use** / **Anti-patterns** on that map
- [ ] After boot-path edits: `make build` and `make verify` or `make run`

Boot path: ATF (Trusty BL32, lkboot BL33) → linux-arm64-boot-shim →
physboot → `vmzircon` → userboot → `component_manager` →
console-launcher → `/boot/bin/sh`.
