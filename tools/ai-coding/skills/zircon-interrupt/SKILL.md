---
name: zircon-interrupt
description: >
  Use when changing GICv3, IRQ handlers, page faults, exception vectors, guest vGIC on the Secular console image. Read the map
  in products/reference/zircon/interrupt.md before editing.
---

# zircon-interrupt

Before changing this area, read
[`products/reference/zircon/interrupt.md`](../../../../products/reference/zircon/interrupt.md).

That file is written as a Secular skill: **When to use**, numbered
sections, box-drawing diagrams, **Anti-patterns**, and a first-pass
checklist.

## When to use

* Changing GICv3, IRQ handlers, page faults, exception vectors, or guest vGIC.

## Do not

* Skip the map and grep only. The diagrams are the contract.
* Copy Fuchsia product paths (`core`, Starnix, netstack) back into this
  tree.

## Related

* Other maps: [`products/reference/zircon/README.md`](../../../../products/reference/zircon/README.md)
