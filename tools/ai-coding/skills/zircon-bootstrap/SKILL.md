---
name: zircon-bootstrap
description: >
  Use when changing Secular boot, ATF, Trusty, lkboot, physboot, userboot, QEMU -bios/-kernel/-initrd on the Secular console image. Read the map
  in products/reference/zircon/bootstrap.md before editing.
---

# zircon-bootstrap

Before changing this area, read
[`products/reference/zircon/bootstrap.md`](../../../../products/reference/zircon/bootstrap.md).

That file is written as a Secular skill: **When to use**, numbered
sections, box-drawing diagrams, **Anti-patterns**, and a first-pass
checklist.

## When to use

* Changing ATF, Trusty, lkboot, physboot, userboot, or QEMU `-bios`/`-kernel`/`-initrd`.

## Do not

* Skip the map and grep only. The diagrams are the contract.
* Copy Fuchsia product paths (`core`, Starnix, netstack) back into this
  tree.

## Related

* Other maps: [`products/reference/zircon/README.md`](../../../../products/reference/zircon/README.md)
