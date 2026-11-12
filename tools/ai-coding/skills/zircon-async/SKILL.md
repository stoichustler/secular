---
name: zircon-async
description: >
  Use when changing ports, PortObserver, async_loop, fuchsia_async, FIDL event loops on the Secular console image. Read the map
  in products/reference/zircon/async.md before editing.
---

# zircon-async

Before changing this area, read
[`products/reference/zircon/async.md`](../../../../products/reference/zircon/async.md).

That file is written as a Secular skill: **When to use**, numbered
sections, box-drawing diagrams, **Anti-patterns**, and a first-pass
checklist.

## When to use

* Changing ports, PortObserver, async_loop, fuchsia_async, or FIDL event loops.

## Do not

* Skip the map and grep only. The diagrams are the contract.
* Copy Fuchsia product paths (`core`, Starnix, netstack) back into this
  tree.

## Related

* Other maps: [`products/reference/zircon/README.md`](../../../../products/reference/zircon/README.md)
