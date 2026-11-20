---
name: zircon-cpu
description: >
  Use when changing threads, context switch, syscalls, idle/WFI, SMP, VCPU enter on the Secular console image. Read the map
  in vision/reference/zircon/cpu.md before editing.
---

# zircon-cpu

Before changing this area, read
[`vision/reference/zircon/cpu.md`](../../../../vision/reference/zircon/cpu.md).

That file is written as a Secular skill: **When to use**, numbered
sections, box-drawing diagrams, **Anti-patterns**, and a first-pass
checklist.

## When to use

* Changing threads, context switch, syscalls, idle/WFI, SMP, or VCPU enter.

## Do not

* Skip the map and grep only. The diagrams are the contract.
* Copy Fuchsia product paths (`core`, Starnix, netstack) back into this
  tree.

## Related

* Other maps: [`vision/reference/zircon/README.md`](../../../../vision/reference/zircon/README.md)
