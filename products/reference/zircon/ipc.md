# IPC

Secular boots a Zircon kernel on `qemu-system-aarch64` (virt, arm64).
Processes do not share a POSIX fd table or a global namespace. They talk
through **kernel objects** named by **handles**. This note is the map of
that path: what each IPC object is for, how a message or a signal moves,
and which of them the console image actually uses.

The product stays:

```text
zircon kernel + linux-arm64-boot-shim + BOOTFS + serial console + zxsh
```

Read [`cpu.md`](cpu.md) for syscalls / `Thread` block, [`memory.md`](memory.md)
for VMOs, [`synchronization.md`](synchronization.md) for `WaitQueue` /
`CriticalMutex`. Userboot's two channel messages are [`bootstrap.md`](bootstrap.md).

## IPC

### Framework

One kernel object, two (or one) handles, signals to wait on. Data either
moves as a **message** or stays in a **shared VMO**.

```text
 process A handle table              process B handle table
   hA ─────────────────┐               ┌───────────────── hB
                       ▼               ▼
              ┌─────────────────────────────────┐
              │ kernel object (peered or solo)  │
              │   Channel / Socket / FIFO       │
              │   Event / EventPair / Port      │
              │   VMO (shared memory)           │
              │                                 │
              │  signals: READABLE WRITABLE     │
              │           PEER_CLOSED SIGNALED  │
              └─────────────────────────────────┘
                       │
                       ▼
              zx_object_wait_*  /  zx_port_wait  /  futex
              thread BLOCKED → WaitQueue → Reschedule
```

| Kind | What moves | Handles in payload? | Typical use |
| --- | --- | --- | --- |
| Channel | discrete messages (bytes + handles) | **yes** (capability pass) | FIDL, userboot bootstrap, component_manager |
| Socket | byte stream or datagrams | no | logs, bulk bytes |
| FIFO | small fixed records | no | driver ↔ userspace rings |
| Event / EventPair | only signals | no | notify / hangup |
| Port | packets from many objects | no | async wait (one thread, many sources) |
| Futex | nothing; a user word | no | libc mutex (`synchronization.md`) |
| VMO | pages, not messages | mapping, not send | shared buffers |

Peered objects (`Channel`, `Socket`, `FIFO`, `EventPair`) are created as
a pair. `related_koid` of one end is the other. Closing one end asserts
`PEER_CLOSED` on the survivor.

### Flow

Channel is the one that carries capabilities. Write on A, queue on B,
wake whoever waits on B.

```text
 A  zx_channel_write(hA, bytes, handles)
        │  take handles out of A's table
        │  MessagePacket  (≤ 64 KiB, ≤ 64 handles)
        ▼
 B's ChannelDispatcher
        messages.push_back
        raise ZX_CHANNEL_READABLE
        notify observers  (wait_one / port)
        │
        ▼
 B  was waiting?  →  WaitQueue wake → READY
        │
 B  zx_channel_read(hB)
        pop packet
        install handles in B's table
        clear READABLE if empty
```

`zx_channel_call` is write then wait for a reply whose `txid` matches
(kernel-generated txids have bit 31 set).

### Handles and rights

A handle is a process-local integer: `(object, rights)`. It is not a
pointer and is not valid in another process until it is **transferred**.

#### Framework

```text
 process A                         kernel                        process B
 ┌──────────────┐                  ┌────────────┐                ┌──────────────┐
 │ handle table │                  │ Dispatcher │                │ handle table │
 │  h=3  rights │──► same object ──│  koid K    │◄── same object │  h=7  rights │
 └──────────────┘                  └────────────┘                └──────────────┘
        local int                         ▲                             local int
        not a pointer                     │
                                   Channel write moves
                                   the *handle*, not a copy
                                   of the object
```

#### Flow

```text
 zx_channel_write(hA, …, handles[])
        │
        ▼
 HandleTable.remove(h)     sender loses it
        │
        ▼
 packet.handles.append     KernelHandle + rights
        │
        ▼
 peer queue
        │
        ▼
 zx_channel_read
        │
        ▼
 HandleTable.add(new_h)    receiver: new local value, same object
```

Rights shrink on duplicate (`zx_handle_duplicate`) and on write with a
disposition. `ZX_RIGHT_TRANSFER` is required to put a handle in a
channel. Without `ZX_RIGHT_WAIT` you cannot `zx_object_wait_*`.

The kernel may write an endpoint (owner koid `ZX_KOID_INVALID`) — that
is how `userboot_init` stuffs the bootstrap channel.

### Signals and wait

Every waitable object has a signal bitset. The kernel raises/clears
bits; userspace waits for a mask.

#### Framework

```text
 ┌──────────────────────────────────────────┐
 │ Dispatcher                               │
 │   signals_  (bitset)                     │
 │   observers ──► WaitSignalObserver       │
 │              ──► PortObserver            │
 └──────────────────┬───────────────────────┘
                    │ raise / clear bits
                    ▼
        ┌───────────┴───────────┐
        ▼                       ▼
 zx_object_wait_one/many    zx_object_wait_async
   Event::Wait                Port packet queue
   Blocked::WAIT_*            zx_port_wait
```

#### Flow

```text
 thread  zx_object_wait_one(h, mask)
        │  attach WaitSignalObserver
        ▼
 Event::Wait     BLOCKED
        │
 object raises a bit in mask
        │
        ▼
 observer → Event::Signal
        │
        ▼
 thread READY  (maybe IPI RESCHEDULE)
        │
        ▼
 return observed bits
        handle closed while waiting → ZX_ERR_CANCELED
```

| Signal | Typical meaning |
| --- | --- |
| `ZX_CHANNEL_READABLE` / `ZX_SOCKET_READABLE` / `ZX_FIFO_READABLE` | queue not empty |
| `ZX_*_WRITABLE` | room to write |
| `ZX_*_PEER_CLOSED` | other end gone |
| `ZX_EVENT_SIGNALED` | `zx_object_signal` set it |
| `ZX_TASK_TERMINATED` | job/process/thread dead (`cpu.md`) |
| `ZX_USER_SIGNAL_0…7` | app-defined, if the object allows |

Wait APIs:

| Syscall | Blocks on | Limit |
| --- | --- | --- |
| `zx_object_wait_one` | one handle, one mask | — |
| `zx_object_wait_many` | several handles | 64 |
| `zx_object_wait_async` | bind object → **port** (does not block) | — |
| `zx_port_wait` | next packet on that port | — |

`wait_one` / `wait_many` attach a `WaitSignalObserver`, then
`Event::Wait` (`Blocked::WAIT_ONE` / `WAIT_MANY`). When the object
raises a watched bit, the observer signals that event and the thread
becomes `READY` (`synchronization.md`). Closing the handle while
waiting returns `ZX_ERR_CANCELED` (`ZX_SIGNAL_HANDLE_CLOSED`).

### Channel

**Principle:** a two-ended queue of messages. Each message is atomic
(all bytes and handles, or none). This is how Zircon moves
**capabilities** (handles) between processes. FIDL sits on top.

#### Framework

```text
        handle A                         handle B
            │                                │
            ▼                                ▼
 ┌─────────────────────┐          ┌─────────────────────┐
 │ ChannelDispatcher A │◄──peer──►│ ChannelDispatcher B │
 │  messages[]         │          │  messages[]         │
 │  waiters[]  (call)  │          │  waiters[]          │
 │  READABLE WRITABLE  │          │  READABLE WRITABLE  │
 │  PEER_CLOSED        │          │  PEER_CLOSED        │
 └──────────┬──────────┘          └──────────▲──────────┘
            │ write                            │
            └──── MessagePacket ───────────────┘
                  bytes ≤ 64 KiB
                  handles ≤ 64
```

Write on A always enqueues on **B**. Each end has its own inbound
queue.

#### Flow

```text
 zx_channel_create  →  (hA, hB)

 ── write / read ──
 A  zx_channel_write(hA, bytes, handles)
        │  take handles out of A's table
        ▼
    peer = B
        │  try MessageWaiter (channel_call) first
        │  else B.messages.push_back
        ▼
    raise B.READABLE  →  wake wait_one / port
 B  zx_channel_read(hB)
        pop  /  empty → SHOULD_WAIT or PEER_CLOSED
        install handles in B
        queue empty → clear READABLE

 ── call (write + matched reply) ──
 A  zx_channel_call(hA, req, deadline)
        │  txid (kernel ids have bit 31)
        │  park MessageWaiter on A
        ▼
    write req onto B
 B  reads, handles, writes reply onto A
        │  waiter matches txid → deliver, no queue
        ▼
    call returns reply packet
```

Create: `zx_channel_create` → two handles, `ZX_POL_NEW_CHANNEL`.
Lock: `CriticalMutex` on the peer holder, plus a separate
`channel_lock` on the message list so `read` can run while observers
are notified.

| Limit | Value |
| --- | --- |
| bytes per message | `ZX_CHANNEL_MAX_MSG_BYTES` (64 KiB) |
| handles per message | `ZX_CHANNEL_MAX_MSG_HANDLES` (64) |
| pending messages (soft) | 3500; then policy exception `CHANNEL_FULL_WRITE` |

Write always targets the **peer**. Empty peer → `ZX_ERR_PEER_CLOSED`.
Empty queue on read → `ZX_ERR_SHOULD_WAIT` (or `PEER_CLOSED`). A
`zx_channel_call` waiter can take the reply off the queue without a
separate read (message waiter list).

Console: `userboot_init` sends two **handle-only** messages on the
bootstrap channel (process caps, then system caps). No FIDL bytes.
`component_manager` then speaks FIDL over channels to
console-launcher and everyone else.

### Socket

**Principle:** a pipe of bytes, not capabilities. Stream (TCP-like) or
datagram (`ZX_SOCKET_DATAGRAM`). No handles in the payload.

#### Framework

```text
        handle A                         handle B
            │                                │
            ▼                                ▼
 ┌─────────────────────┐          ┌─────────────────────┐
 │ SocketDispatcher A  │◄──peer──►│ SocketDispatcher B  │
 │  data  (byte buf)   │          │  data  (byte buf)   │
 │  STREAM | DATAGRAM  │          │                     │
 │  READABLE WRITABLE  │          │  READABLE WRITABLE  │
 │  WRITE_DISABLED     │          │  PEER_CLOSED        │
 └──────────┬──────────┘          └──────────▲──────────┘
            │ write bytes                      │
            └──────── copy into B.data ────────┘
```

#### Flow

```text
 zx_socket_create(flags)  →  (hA, hB)

 A  zx_socket_write(hA, buf, len)
        │  WRITE_DISABLED? → BAD_STATE
        │  peer gone?      → PEER_CLOSED
        ▼
    copy into B.data
        empty→nonempty → raise B.READABLE
        B full         → SHOULD_WAIT, clear A.WRITABLE
 B  zx_socket_read(hB, buf, len)
        STREAM: consume a prefix (no boundaries)
        DATAGRAM: one datagram, or SHOULD_WAIT
        last byte gone → clear B.READABLE
        space freed    → raise A.WRITABLE

 shutdown writes on A  →  A.WRITE_DISABLED, B sees peer write disabled
```

Use when you want bulk data and already have a channel to pass the
socket handle itself. The console image does not need sockets for
`zxsh`.

### FIFO

**Principle:** a tiny kernel ring of fixed-size elements. Fast, no
handles, no 64 KiB packets.

#### Framework

```text
        handle A                         handle B
            │                                │
            ▼                                ▼
 ┌─────────────────────┐          ┌─────────────────────┐
 │ FifoDispatcher A    │◄──peer──►│ FifoDispatcher B    │
 │  elem_size, count   │          │  elem_size, count   │
 │  data[count*size]   │          │  data[count*size]   │
 │  head / tail        │          │  head / tail        │
 │  READABLE WRITABLE  │          │  READABLE WRITABLE  │
 └──────────┬──────────┘          └──────────▲──────────┘
            │ write N elements                 │
            └──── copy into B ring ────────────┘
                  total ≤ 4 KiB
```

Write on A copies **whole elements** into B's ring (`head`/`tail`).
Payload that does not fit in 4 KiB lives in a VMO; the FIFO only
carries counts / indices.

#### Flow

```text
 zx_fifo_create(count, elem_size)  →  (hA, hB)
        count * elem_size ≤ 4 KiB

 A  zx_fifo_write(hA, elems, n)
        │  copy n * elem_size into B.data[head]
        │  head += n
        ▼
    B was empty → raise B.READABLE
    B now full  → clear A.WRITABLE
 B  zx_fifo_read(hB, buf, n)
        copy from B.data[tail], tail += n
        last element gone → clear B.READABLE
        space freed       → raise A.WRITABLE
 empty + peer closed → PEER_CLOSED
```

Drivers use this for “I completed N records in a VMO you already
mapped.” The records stay small; the payload lives in the VMO.

### Event and EventPair

**Principle:** signals with no queue.

#### Framework

```text
 Event (solo)                         EventPair (peered)
 ┌──────────────────┐                 ┌──────────────────┐   ┌──────────────────┐
 │ EventDispatcher  │                 │ EventPair A      │◄─►│ EventPair B      │
 │  SIGNALED        │                 │  SIGNALED        │   │  SIGNALED        │
 │                  │                 │  PEER_CLOSED     │   │  PEER_CLOSED     │
 └────────▲─────────┘                 └────────▲─────────┘   └────────▲─────────┘
          │  zx_object_signal                  │                      │
          │                                    │ zx_object_signal     │ signal_peer
        handle                               hA                      hB
```

| Object | Ends | How it signals |
| --- | --- | --- |
| Event | one | `zx_object_signal` → `ZX_EVENT_SIGNALED` |
| EventPair | two, peered | `zx_object_signal` on self; `zx_object_signal_peer` on the other |

#### Flow

```text
 ── Event ──
 zx_event_create → h
 zx_object_signal(h, clear, set)
        raise SIGNALED
 waiter  zx_object_wait_one(h, SIGNALED)  →  READY

 ── EventPair ──
 zx_eventpair_create → (hA, hB)
 A  zx_object_signal_peer(hA, …)  →  B.SIGNALED  →  wake B's waiters
 A  zx_handle_close(hA)           →  B.PEER_CLOSED (hangup)
```

`PEER_CLOSED` on an event pair is a hangup: the other process dropped
its end. Cheaper than a channel if you only need “it happened.”

### Port

**Principle:** one wait site for many objects. Bind with
`zx_object_wait_async(port, handle, key, signals)`. When the object
matches, a packet `{key, observed signals}` is queued. `zx_port_wait`
blocks on that queue (`Blocked::PORT`).

#### Framework

```text
  object X          object Y          object Z
  (channel)         (event)           (fifo)
       │                 │                 │
       │  PortObserver   │                 │
       └────────┬────────┴────────┬────────┘
                ▼                 ▼
         ┌─────────────────────────────────┐
         │ PortDispatcher                  │
         │   packet queue                  │
         │   {key, observed signals}       │
         └────────────────┬────────────────┘
                          │
                          ▼
                   zx_port_wait
                   Blocked::PORT
```

After bind (`wait_async`): observer is on **both** the object and the
port. After a match (or cancel): object drops its pointer; the packet
sits on the port until `zx_port_wait` (or cancel) consumes it.

#### Flow

```text
 zx_port_create → hPort

 zx_object_wait_async(hPort, hX, key=X, READABLE)
 zx_object_wait_async(hPort, hY, key=Y, SIGNALED)
        │  PortObserver on each object
        ▼
 thread  zx_port_wait(hPort)     BLOCKED
        │
 X becomes READABLE
        │  observer queues packet {key=X, READABLE}
        │  object no longer points at observer
        ▼
 thread READY, returns that packet
        loop: wait again for the next source
```

Unlike `wait_many`, you can keep adding sources, and you get a packet
per event rather than a scanned list. `component_manager` and drivers
use this for an event loop.

Cancel: close the handle, or `zx_port_cancel`. The observer may sit on
both the object and the port until the packet is queued
([`port_dispatcher.h`](../../../zircon/kernel/object/include/object/port_dispatcher.h)).

### Futex

**Principle:** the wait is in the kernel; the lock word is in **user**
memory (often a shared VMO). `zx_futex_wait(addr, expected, …)` sleeps
if `*addr == expected`. `zx_futex_wake` readies waiters.

#### Framework

```text
 process A aspace              process B aspace
    user VA p ─────────┐          user VA q
                       │          (same page if shared VMO)
                       ▼
                ┌──────────────┐
                │ word in RAM  │   no handle, no Dispatcher
                └──────┬───────┘
                       │ keyed by (aspace, VA)
                       ▼
                FutexContext
                WaitQueue of threads
```

#### Flow

```text
 A  zx_futex_wait(&word, expected, deadline)
        │  *word != expected  →  return OK (already changed)
        │  *word == expected  →  BLOCKED on that key
        ▼
 B  stores a new value, then zx_futex_wake(&word, n)
        │
        ▼
    n waiters → READY

 libc sync_mutex:  uncontended = user atomic
                   contended   = futex_wait / wake
```

No handle to a futex object. The key is the user VA (and the aspace).
Libc `sync_mutex_t` is this (`synchronization.md`). It is IPC only if
two processes map the same page and agree on the word.

### VMO (shared memory)

**Principle:** do not copy. Map the same pages in two aspaces
(`zx_vmar_map`, [`memory.md`](memory.md)). Pass the VMO **handle** over a
channel once, then produce/consume in place. FIFOs or channel bytes
carry offsets and lengths.

#### Framework

```text
        channel (control)                 VMO (payload)
   A ── msgs: offset, length ── B         same pages
                                          ┌─────────────┐
   A  zx_vmar_map  VA_a  ─────────────────│  VmObject   │
   B  zx_vmar_map  VA_b  ─────────────────│  pages      │
                                          └─────────────┘
                                          TTBR0 of A and of B
```

#### Flow

```text
 A  zx_vmo_create → hVmo
 A  zx_channel_write(hCh, …, hVmo)     transfer the handle
 B  zx_channel_read  →  hVmo'
 B  zx_vmar_map(hVmo') → VA_b
 A  writes at VA_a
        │  same PA  (memory.md physmap / PTE)
        ▼
 B  reads at VA_b
 A  zx_fifo_write / zx_channel_write("offset, len")
        B knows which bytes are valid
```

Kernel never interprets the bytes. Cacheability follows the mapping
flags. This is how large payloads avoid 64 KiB channel messages.

### Console path

```text
userboot_init                         kernel
  channel ── two handle-only msgs ──► userboot EL0
                                        unpack BOOTFS
                                        launch component_manager
                                          channels (FIDL) to
                                            console-launcher
                                              kernel serial syscalls
                                                zxsh  (stdio is serial,
                                                       not a channel)
```

`zxsh` on this image talks to the kernel console, not to a userspace
TTY channel. The IPC you will see on serial boot is: bootstrap
channel, then component_manager's FIDL channels to launchers. UART
IRQ → wait wakeup is [`interrupt.md`](interrupt.md) +
[`synchronization.md`](synchronization.md), not a Zircon socket.

### How to choose

```text
Need to send a handle / capability?
  → Channel
Need an event loop over many objects?
  → Port + wait_async
Need to sleep on one object?
  → zx_object_wait_one
Need bytes, no handles, maybe a lot?
  → Socket, or VMO + Channel/FIFO for control
Need tiny fixed records to a driver?
  → FIFO (+ VMO for payload)
Need only “signalled / hangup”?
  → Event / EventPair
Need a userspace mutex?
  → futex (sync_mutex), not a kernel Mutex
```

### Where in the tree

| File | Why |
| --- | --- |
| [`zircon/kernel/object/channel_dispatcher.rs`](../../../zircon/kernel/object/channel_dispatcher.rs) | write → peer queue → `READABLE` |
| [`zircon/kernel/object/include/object/channel_dispatcher.h`](../../../zircon/kernel/object/include/object/channel_dispatcher.h) | `MessageWaiter` (`channel_call`) |
| [`zircon/kernel/lib/syscalls/channel.rs`](../../../zircon/kernel/lib/syscalls/channel.rs) | `sys_channel_{create,read,write,call}` |
| [`zircon/kernel/object/include/object/message_packet.h`](../../../zircon/kernel/object/include/object/message_packet.h) | bytes + handles in a packet |
| [`zircon/kernel/object/include/object/socket_dispatcher.h`](../../../zircon/kernel/object/include/object/socket_dispatcher.h) | stream / datagram |
| [`zircon/kernel/object/fifo_dispatcher.rs`](../../../zircon/kernel/object/fifo_dispatcher.rs) | ring, 4 KiB cap |
| [`zircon/kernel/object/include/object/event_dispatcher.h`](../../../zircon/kernel/object/include/object/event_dispatcher.h) | solo event |
| [`zircon/kernel/object/include/object/event_pair_dispatcher.h`](../../../zircon/kernel/object/include/object/event_pair_dispatcher.h) | peered events |
| [`zircon/kernel/object/include/object/port_dispatcher.h`](../../../zircon/kernel/object/include/object/port_dispatcher.h) | observer + packet queue |
| [`zircon/kernel/lib/syscalls/object_wait.rs`](../../../zircon/kernel/lib/syscalls/object_wait.rs) | `wait_one` / `wait_many` / `wait_async` |
| [`zircon/kernel/lib/syscalls/futex.rs`](../../../zircon/kernel/lib/syscalls/futex.rs) | user-word wait |
| [`zircon/kernel/lib/userabi/userabi.rs`](../../../zircon/kernel/lib/userabi/userabi.rs) | bootstrap channel |
| [`zircon/system/public/zircon/types.h`](../../../zircon/system/public/zircon/types.h) | signal bits, channel/fifo limits |

Suggested first pass: **handle table → `zx_channel_write` → peer
`READABLE` → `wait_one` → `zx_channel_read` (handles appear in the
receiver)**. Then **port wait_async**. That is enough to see userboot's
two messages, and a FIDL call from `component_manager`.

### Terminology

| Term | Meaning in this note |
| --- | --- |
| Handle | Process-local name of a kernel object plus a rights mask. |
| Rights | What this handle may do (`READ`, `WRITE`, `WAIT`, `TRANSFER`, …). |
| Peered | Object created as a pair. Write on one end, read on the other. |
| `related_koid` | KOID of the peer (channel/socket/fifo/eventpair) or parent (thread → process). |
| Signal | Bit on an object. Waiters sleep until a bit in their mask is raised. |
| Message | One channel packet: bytes + handles, atomic. |
| Capability | A handle. Moving it through a channel is how authority is delegated. |
| `ZX_ERR_SHOULD_WAIT` | Queue empty (or full) and peer still alive; caller should wait. |
| `ZX_ERR_PEER_CLOSED` | Other end is gone. |
| `txid` | Channel-call transaction id. Kernel-generated ids have bit 31 set. |
| Port packet | `{key, signals}` queued when an async wait matches. |
| Futex | Kernel wait keyed by a user address, not by a handle. |
| FIDL | IDL and wire format on channels. Not a kernel object. |

---

Everyday's a Hustle.
