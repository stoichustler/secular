---
name: zircon-sync
description: >
  Use when changing SpinLock, Mutex, WaitQueue, futex, memory ordering on the Secular console image. Read the map
  in products/reference/zircon/sync.md before editing.
---

# zircon-sync

Before changing this area, read
[`products/reference/zircon/sync.md`](../../../../products/reference/zircon/sync.md).

That file is written as a Secular skill: **When to use**, numbered
sections, box-drawing diagrams, **Anti-patterns**, and a first-pass
checklist.

## When to use

* Changing SpinLock, Mutex, WaitQueue, futex, or memory ordering.

## Do not

* Skip the map and grep only. The diagrams are the contract.
* Copy Fuchsia product paths (`core`, Starnix, netstack) back into this
  tree.

## Related

* Other maps: [`products/reference/zircon/README.md`](../../../../products/reference/zircon/README.md)
