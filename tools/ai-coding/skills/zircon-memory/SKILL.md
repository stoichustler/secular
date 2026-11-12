---
name: zircon-memory
description: >
  Use when changing page tables, physmap, VMOs/VMARs, PTE bits, PMM, heap,
  page queues, demand paging, cache coherency, ASIDs, or translation
  faults on the Secular console image. Read the map in
  products/reference/zircon/memory.md before editing.
---

# zircon-memory

Before changing this area, read
[`products/reference/zircon/memory.md`](../../../../products/reference/zircon/memory.md).

That file is written as a Secular skill: **When to use**, numbered
sections, box-drawing diagrams, **Anti-patterns**, and a first-pass
checklist.

## When to use

* Changing page tables, physmap, VMOs/VMARs, PTE bits, or translation faults.
* Changing PMM arenas, `vm_page` state, kernel heap, VMO page allocation,
  demand paging, cache coherency, VMARs, or ASIDs.

## Do not

* Skip the map and grep only. The diagrams are the contract.
* Copy Fuchsia product paths (`core`, Starnix, netstack) back into this
  tree.

## Related

* Other maps: [`products/reference/zircon/README.md`](../../../../products/reference/zircon/README.md)
