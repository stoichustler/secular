---
name: zircon-ipc
description: >
  Use when changing channels, handles, FIDL wire format, sockets, FIFOs, bootstrap channel on the Secular console image. Read the map
  in products/reference/zircon/ipc.md before editing.
---

# zircon-ipc

Before changing this area, read
[`products/reference/zircon/ipc.md`](../../../../products/reference/zircon/ipc.md).

That file is written as a Secular skill: **When to use**, numbered
sections, box-drawing diagrams, **Anti-patterns**, and a first-pass
checklist.

## When to use

* Changing channels, handles, FIDL wire format, sockets, FIFOs, or the bootstrap channel.

## Do not

* Skip the map and grep only. The diagrams are the contract.
* Copy Fuchsia product paths (`core`, Starnix, netstack) back into this
  tree.

## Related

* Other maps: [`products/reference/zircon/README.md`](../../../../products/reference/zircon/README.md)
