---
name: zircon-async
description: >
  Use when changing ports, PortObserver, async_loop, fuchsia_async, or FIDL event loops on the Secular console image.
---

# Zircon Async: Port, async_loop, and fuchsia_async

Stop when a handler or a polled `Future` has run.

## When to use

* Editing `PortDispatcher`, `async_loop`, `async::Wait`, or `fuchsia_async` executors.
* A hung FIDL server (forgot to re-arm a one-shot wait).

## When not to use

* Channel/FIDL bytes and txid — `ipc.md`.
* Kernel `WaitQueue` / `Mutex` — `sync.md`.

> [!NOTE]
> Console image: Zircon kernel + linux-arm64-boot-shim + BOOTFS + serial
> console + `zxsh`. The kernel is an EL2 VHE host.

## 1. Framework

Two user-space loops, one kernel object. They do **not** share code.
Both sit on a port.

```text
┌─────────────────────────────────────────────────────────────────┐
│ process  (EL0)                                                  │
│                                                                 │
│  C / C++                          Rust                          │
│  async_loop  (libasync-loop)      fuchsia_async executor        │
│    async_wait_t / async_task_t      Future + Waker              │
│    async::Wait / async::Task        OnSignals / Channel / Timer │
│    fpromise::Executor               LocalExecutor / SendExecutor│
│         │                                    │                  │
│         │  zx_object_wait_async              │  same syscalls   │
│         │  zx_port_queue / zx_timer_set      │                  │
│         ▼                                    ▼                  │
│     loop.port                            executor.port          │
└─────────┬────────────────────────────────────┬──────────────────┘
          │                                    │
          ▼                                    ▼
┌─────────────────────────────────────────────────────────────────┐
│ kernel  PortDispatcher                                          │
│   packet queue  {key, type, observed}                           │
│   PortObserver on each waited-upon object                       │
│                                                                 │
│   zx_port_wait  →  thread Blocked::PORT  →  packet  →  READY    │
└─────────────────────────────────────────────────────────────────┘
```

| Layer | What it is | Who uses it on this image |
| --- | --- | --- |
| Port | kernel object, one wait site for many sources | every async path |
| `async_dispatcher_t` | C vtable (`async_ops_t`) | clients of libasync |
| `async_loop` | the stock dispatcher: one port + one timer | C++ FIDL, console-launcher, syslog, critical-services |
| `async::Loop` / `Wait` / `Task` | C++ RAII around that loop | same |
| `async::Executor` | `fpromise` tasks posted onto the loop | C++ promise chains |
| `fuchsia_async` | Rust `Future` executor on its **own** port | userboot, `component_manager` |
| `zx_object_wait_one` | **not** a loop; one thread, one object | `zxsh`, short blocking calls |

The kernel does not run an async loop. A syscall either returns or the
calling thread goes `BLOCKED` ([`ipc.md`](ipc.md)). Async is a
**userspace** way to multiplex many of those waits onto one (or a few)
threads.

Three questions, one primitive each:

| Need | Primitive |
| --- | --- |
| Sleep until one object signals | `zx_object_wait_one` |
| Sleep until any of many objects signal | port + `zx_object_wait_async` |
| Run callbacks / futures when those packets arrive | `async_loop` or `fuchsia_async` |

## 2. Flow

One unit of work on `async_loop`. `fuchsia_async` is the same port
wait, then a `Future::poll` instead of a C handler.

```text
┌────────────────────────────────────────────────────────────────┐
│ async_loop_create                                              │
│   zx_port_create                                               │
│   zx_timer_create(ZX_CLOCK_MONOTONIC)                          │
│   maybe TLS default dispatcher                                 │
└────────────────────────────┬───────────────────────────────────┘
                             ▼
┌────────────────────────────────────────────────────────────────┐
│ client  async_begin_wait(channel, READABLE)                    │
│         async_post_task(deadline)                              │
│   wait: zx_object_wait_async(object, port, key=wait*)          │
│   task: insert deadline list; zx_timer_set + wait_async(timer) │
└────────────────────────────┬───────────────────────────────────┘
                             ▼
┌────────────────────────────────────────────────────────────────┐
│ thread  async_loop_run(ZX_TIME_INFINITE)                       │
└────────────────────────────┬───────────────────────────────────┘
                             ▼  (1) zx_port_wait(port)
┌────────────────────────────────────────────────────────────────┐
│ packet.key / packet.type                                       │
├────────────────────────────┬───────────────────────────────────┤
│ KEY_PORT_CONTROL  USER     │ quit / wake other threads         │
│ KEY_TIMER_CONTROL SIGNAL   │ dispatch due tasks                │
│ wait*             SIGNAL   │ wait->handler                     │
│ receiver*         USER     │ receiver->handler                 │
│ irq*              INTERRUPT│ irq->handler                      │
│ trap*             GUEST    │ trap->handler                     │
│ paged_vmo*        PAGE     │ paged_vmo->handler                │
└────────────────────────────┬───────────────────────────────────┘
                             ▼  (2) handler runs on this thread
┌────────────────────────────────────────────────────────────────┐
│ typically: zx_channel_read, then async_begin_wait again        │
└────────────────────────────┬───────────────────────────────────┘
                             ▼
                      next zx_port_wait
```

`fuchsia_async` `worker_lifecycle`:

```text
┌────────────────────────────────────────────────────────────────┐
│ poll ready Futures                                             │
│   none ready?  mark sleeping                                   │
└────────────────────────────┬───────────────────────────────────┘
                             ▼
┌────────────────────────────────────────────────────────────────┐
│ zx_port_wait(INFINITE)     (or INFINITE_PAST if still busy)    │
└──────────────┬─────────────────────────────┬───────────────────┘
               ▼                             ▼
┌──────────────────────────────┐  ┌──────────────────────────────┐
│ key == TASK_READY_WAKEUP_ID  │  │ PacketReceiver::receive_packet│
│ another thread posted a task │  │ typically AtomicWaker::wake  │
└──────────────┬───────────────┘  └──────────────┬───────────────┘
               └────────────────┬────────────────┘
                                ▼
┌────────────────────────────────────────────────────────────────┐
│ poll the woken Future                                          │
│   Pending  →  wait again                                       │
│   Ready    →  drop / join                                      │
└────────────────────────────────────────────────────────────────┘
```

| Node | Blocks? | Who runs |
| --- | --- | --- |
| `zx_port_create` / `zx_timer_create` | no | creator thread |
| `async_begin_wait` / `OnSignals` first poll | no | any thread (registers on the port) |
| `zx_port_wait` | **yes** `Blocked::PORT` | loop / executor thread |
| handler / `Future::poll` | only if it makes a blocking syscall | that same thread |
| `async_loop_quit` / executor done | no | any thread; wakes waiters with a USER packet |

## 3. Port

**Principle:** one wait site for many objects. Bind a handle to a port
with `zx_object_wait_async`. When the object matches the trigger, the
kernel queues a **packet** `{key, type, observed}`. `zx_port_wait`
blocks on that queue (`Blocked::PORT`) and returns one packet.

Unlike `zx_object_wait_one` (one object, this thread sleeps on it) or
`zx_object_wait_many` (a scanned list, cap `ZX_WAIT_MANY_MAX_ITEMS`),
a port can keep gaining sources, and each event is a packet rather
than a bit in a scanned array. That is why `async_loop` and
`fuchsia_async` are built on it.

#### Framework

User view first: three objects, one port, one thread.

```text
┌──────────────┐         ┌──────────────┐         ┌──────────────┐
│ Channel X    │         │ Event Y      │         │ Timer Z      │
│ READABLE     │         │ SIGNALED     │         │ SIGNALED     │
└──────┬───────┘         └──────┬───────┘         └──────┬───────┘
       │                        │                        │
       │      zx_object_wait_async(port, handle, key, signals)
       │                        │                        │
       └────────────┬───────────┴────────────┬───────────┘
                    ▼                        ▼
         ┌─────────────────────────────────────────────────┐
         │ Port                                            │
         │   packets_            FIFO of PortPacket        │
         │   interrupt_packets_  IRQ first (spinlock)      │
         │   observers_          pending PortObserver      │
         │   sema_               wake Blocked::PORT        │
         └──────────────────────────┬──────────────────────┘
                                    │
                                    ▼
                           zx_port_wait(port)
                           returns one zx_port_packet_t
```

Kernel view: the port is a `PortDispatcher`. Each bind allocates a
`PortObserver` that sits on **two** lists until a match or a cancel.
`p` = raw pointer, `rc` = refcounted, `o` = owning.

```text
┌──────────────────┐                          ┌──────────────────┐
│ Dispatcher X     │                          │ Dispatcher Y     │
│ (Channel)        │                          │ (Event)          │
│ observers_       │                          │ observers_       │
└────────┬─────────┘                          └────────┬─────────┘
         │ p                                           │ p
         ▼                                             ▼
┌──────────────────┐                          ┌──────────────────┐
│ PortObserver     │                          │ PortObserver     │
│  packet_   key   │                          │  packet_   key   │
└───┬──────────┬───┘                          └───┬──────────┬───┘
  p │          │ rc                             p │          │ rc
    ▼          │                                  ▼          │
 object still  └────────────────┬─────────────────┘          │
 holds observer                 ▼                            ▼
                   ┌────────────────────────────────────────┐
                   │ PortDispatcher                         │
                   │   packets_                             │
                   │   interrupt_packets_                   │
                   │   observers_                           │
                   │   sema_                                │
                   └────────────────────────────────────────┘
```

Three packet sources share that dequeue:

| Source | Syscall that produces the packet | List | Type |
| --- | --- | --- | --- |
| Object signal | `zx_object_wait_async` then object `UpdateState` | `packets_` | `ZX_PKT_TYPE_SIGNAL_ONE` |
| Manual | `zx_port_queue` | `packets_` | `ZX_PKT_TYPE_USER` (forced) |
| Interrupt | `zx_interrupt_bind` (port created with `ZX_PORT_BIND_TO_INTERRUPT`) | `interrupt_packets_` (spinlock) | `ZX_PKT_TYPE_INTERRUPT` |
| Guest / pager | `zx_guest_set_trap` / `zx_pager_create_vmo` | `packets_` | `GUEST_*` / `PAGE_REQUEST` |

`Dequeue` always services **interrupt packets first**, then the
regular FIFO. A waiting thread is a `Semaphore`: queue posts
`sema_.Post()`, wait is `sema_.Wait` under `Blocked::PORT`.

#### Packet

Userspace sees a 48-byte `zx_port_packet_t`: 8-byte key, 4-byte type,
4-byte status, 32-byte union. Every sub-packet is exactly 32 bytes.

```text
┌──────────────────────────────────────────────────────────────────┐
│ zx_port_packet_t                                                 │
├──────────────┬──────────┬──────────┬─────────────────────────────┤
│ key  u64     │ type     │ status   │ union (32 bytes)            │
│              │          │          │                             │
│ who to wake  │ what     │ ZX_OK /  │  USER          u64[4]       │
│ (loop ptr or │ happened │ error    │  SIGNAL_ONE    trigger,     │
│  slab index) │          │          │                observed,    │
│              │          │          │                count=1,     │
│              │          │          │                timestamp    │
│              │          │          │  INTERRUPT     timestamp    │
│              │          │          │  GUEST_BELL    GPA          │
│              │          │          │  PAGE_REQUEST  cmd/off/len  │
└──────────────┴──────────┴──────────┴─────────────────────────────┘
```

| `type` | Produced by | Payload |
| --- | --- | --- |
| `ZX_PKT_TYPE_USER` `0x00` | `zx_port_queue` | caller bytes; kernel overwrites `type` to USER |
| `ZX_PKT_TYPE_SIGNAL_ONE` `0x01` | wait_async match | `trigger`, `observed`, `count=1`, optional timestamp |
| `ZX_PKT_TYPE_GUEST_BELL` `0x03` | guest trap | guest physical address |
| `ZX_PKT_TYPE_GUEST_MEM` `0x04` | guest MMIO | GPA + access |
| `ZX_PKT_TYPE_GUEST_IO` `0x05` | guest PIO | port + data |
| `ZX_PKT_TYPE_GUEST_VCPU` `0x06` | VCPU exit | interrupt / startup |
| `ZX_PKT_TYPE_INTERRUPT` `0x07` | bound IRQ | boot timestamp |
| `ZX_PKT_TYPE_PAGE_REQUEST` `0x09` | pager | command, offset, length |

`key` is opaque to the kernel. `async_loop` stores the wait/task
pointer; `fuchsia_async` stores a slab index. `0xffffffff` is reserved
as an internal **canceled** sentinel (`kPortPacketTypeCanceled`): a
packet marked that way is never delivered.

#### Principle

`PortObserver` lifetime is the whole mechanism. Three diagrams from
[`port_dispatcher.h`](../../../zircon/kernel/object/include/object/port_dispatcher.h).

**(1) After `zx_object_wait_async` — observer on both lists**

```text
┌──────────────┐                    ┌──────────────────────┐
│ object       │                    │ Port                 │
│ observers_   │                    │ observers_           │
└──────┬───────┘                    └──────────┬───────────┘
       │ p                                     │ p
       ▼                                       ▼
┌──────────────────────────────────────────────────────────┐
│ PortObserver                                             │
│   key, trigger                                           │
│   packet_     SIGNAL_ONE, not queued yet                 │
│   dispatcher_ ── p ──► object                            │
│   port_       ── rc ────────────────────────────► Port   │
└──────────────────────────────────────────────────────────┘
```

State changes of the object propagate `object --p--> observer --rc-->
port`. The observer is on `Dispatcher::observers_` **and**
`PortDispatcher::observers_`. The packet lives inside the observer
(`allocator == nullptr`: not ephemeral, does not count against the
4 Ki packet cap).

If the trigger is **already asserted** and `ZX_WAIT_ASYNC_EDGE` is
clear, `MakePortObserver` calls `OnMatch` immediately and never
enqueues the observer. That is **level-triggered**. `ZX_WAIT_ASYNC_EDGE`
skips the check: the waiter only fires on a future `UpdateState`.

**(2) After a match (or cancel through the object) — packet queued,
object drops the observer**

```text
┌──────────────┐                    ┌──────────────────────┐
│ object       │                    │ Port                 │
│ (no observer)│                    │ packets_             │
└──────────────┘                    └──────────┬───────────┘
                                               │ o
                                               ▼
┌──────────────────────────────────────────────────────────┐
│ PortObserver                                             │
│   packet_     queued on packets_                         │
│   port_       ── rc ────────────────────────────► Port   │
│   dispatcher_     unlinked                               │
└──────────────────────────────────────────────────────────┘
```

`Dispatcher::NotifyObserversLocked` walks `observers_`, drops any
whose `triggering_signals_ & new_signals != 0`, and calls
`PortObserver::OnMatch`. That fills `signal.observed` / `count=1` /
optional timestamp, then `QueueAndRemoveObserver`:

- packet goes onto `packets_`
- observer is unlinked from the object
- if the packet is still queued, the packet **owns** the observer
  (`packet->observer`) until dequeue
- `sema_.Post()` wakes one `zx_port_wait`

The object no longer has a pointer to the observer. The wait is
**one-shot**. Another notification needs another `wait_async`.

**(3) After `zx_port_wait` — packet copied out, observer freed**

```text
┌──────────────┐                    ┌──────────────────────┐
│ object       │                    │ Port                 │
└──────────────┘                    │ packets_  (this pkt  │
                                    │            gone)     │
                                    │ sema_                │
                                    └──────────┬───────────┘
                                               │ copy
                                               ▼
                                    ┌──────────────────────┐
                                    │ user                 │
                                    │ zx_port_packet_t     │
                                    └──────────────────────┘
  observer UniquePtr dropped outside the lock
```

`Dequeue`:

```text
┌──────────────────────────────────────────────────────────┐
│ zx_port_wait                                             │
│                                                          │
│   Blocked::PORT                                          │
│   sema_.Wait(deadline)     TIMED_OUT → return            │
│                                                          │
│   ┌────────────────────┐   ┌───────────────────────────┐ │
│   │ interrupt_packets_ │   │ packets_                  │ │
│   │ pop first          │   │ pop if IRQ queue empty    │ │
│   │ (spinlock)         │   │ copy out, drop observer   │ │
│   └─────────┬──────────┘   └────────────┬──────────────┘ │
│             │                           │                │
│             └─────────────┬─────────────┘                │
│                           ▼                              │
│                  one zx_port_packet_t                    │
│                  both empty → spurious, wait again       │
└──────────────────────────────────────────────────────────┘
```

Ephemeral packets (`zx_port_queue`, some pager packets) are allocated
from a per-CPU `ObjectCache` and count toward
`kMaxAllocatedPacketCountPerPort` (4096). Signal-one packets live
inside the observer and do **not** count. Hitting the cap raises
`ZX_EXCP_POLICY_CODE_PORT_TOO_MANY_PACKETS`. Too many outstanding
observers raises `ZX_EXCP_POLICY_CODE_PORT_TOO_MANY_OBSERVERS`.

#### Flow

```text
┌────────────────────────────────────────────────────────────────┐
│ zx_port_create([ZX_PORT_BIND_TO_INTERRUPT]) → hPort            │
└────────────────────────────┬───────────────────────────────────┘
                             ▼  (1) bind
┌────────────────────────────────────────────────────────────────┐
│ zx_object_wait_async(hX, hPort, key=X, READABLE, options)      │
│   rights: port WRITE, object WAIT                              │
│   PortObserver on X and on the port                            │
│   already asserted and not EDGE?  OnMatch now                  │
└────────────────────────────┬───────────────────────────────────┘
                             ▼
┌────────────────────────────────────────────────────────────────┐
│ thread  zx_port_wait(hPort, deadline)     BLOCKED::PORT        │
└────────────────────────────┬───────────────────────────────────┘
                             ▼  (2) object signals
┌────────────────────────────────────────────────────────────────┐
│ X  UpdateState(set READABLE)                                   │
│   NotifyObserversLocked                                        │
│   observer OnMatch → QueueAndRemoveObserver                    │
│   X.observers_ no longer points at it                          │
│   packets_.push_back, sema_.Post                               │
└────────────────────────────┬───────────────────────────────────┘
                             ▼
┌────────────────────────────────────────────────────────────────┐
│ thread READY                                                   │
│   copy packet {key=X, SIGNAL_ONE, observed=READABLE}           │
└────────────────────────────┬───────────────────────────────────┘
                             ▼  (3) userspace
┌────────────────────────────────────────────────────────────────┐
│ async_loop / fuchsia_async looks up key                        │
│   handler / Future::poll                                       │
│   typically zx_channel_read, then wait_async again             │
└────────────────────────────────────────────────────────────────┘
```

Manual wake (quit, cross-thread task):

```text
┌────────────────────────────────────────────────────────────────┐
│ other thread  zx_port_queue(hPort, {key=CONTROL, USER, …})     │
│   Alloc ephemeral PortPacket, type forced USER                 │
│   packets_.push_back, sema_.Post                               │
└────────────────────────────┬───────────────────────────────────┘
                             ▼
┌────────────────────────────────────────────────────────────────┐
│ waiter  zx_port_wait returns that packet                       │
└────────────────────────────────────────────────────────────────┘
```

Cancel:

| Call | What it removes |
| --- | --- |
| `zx_port_cancel(port, handle, key)` | observers on that **handle** matching key, plus queued packets with that handle+key |
| `zx_port_cancel_key(port, 0, key)` | **all** observers and queued packets with that key (what `async_loop` uses) |
| close the waited handle | `Dispatcher::Cancel` → `OnCancel` → `MaybeReap` |
| close the port | `on_zero_handles`: drain packets, unlink every observer |

Cancel vs dequeue race (why `async_cancel_wait` can return
`ZX_ERR_NOT_FOUND` while the handler still runs):

```text
┌──────────────────────────────┐  ┌──────────────────────────────┐
│ thread A  zx_port_wait       │  │ thread B  zx_port_cancel_key │
│   pops the packet            │  │   observer already off both  │
│   about to copy              │  │   lists → NOT_FOUND          │
└──────────────┬───────────────┘  └──────────────────────────────┘
               ▼
┌────────────────────────────────────────────────────────────────┐
│ thread A  delivers the packet   handler runs anyway            │
└────────────────────────────────────────────────────────────────┘
```

`cancel_key` lock order: port lock first cannot take a Dispatcher
lock. The port marks pending observers' packets `type = canceled`,
moves them to a temp list, drops the port lock, then deregisters each
from its object. A match that fires in that window sees the sentinel
and does not queue.

`ZX_WAIT_ASYNC_*` options on bind:

| Option | Effect |
| --- | --- |
| `ONCE` `0` | leftover; waits are always one-shot |
| `TIMESTAMP` | `signal.timestamp = current_mono_time()` on match |
| `BOOT_TIMESTAMP` | same with boot clock; mutually exclusive with `TIMESTAMP` |
| `EDGE` | do not fire for signals already asserted at bind |

Rights: create needs `ZX_POL_NEW_PORT`. `wait_async` needs
`ZX_RIGHT_WRITE` on the port and `ZX_RIGHT_WAIT` on the object.
`port_wait` needs `ZX_RIGHT_READ`. `port_queue` / cancel need
`ZX_RIGHT_WRITE`.

#### vs `wait_one` / `wait_many`

```text
┌────────────┬─────────────────────────────────────────────────────┐
│ wait_one   │ one object, this thread, WaitSignalObserver → Event │
├────────────┼─────────────────────────────────────────────────────┤
│ wait_many  │ ≤ ZX_WAIT_MANY_MAX_ITEMS objects, still this thread │
├────────────┼─────────────────────────────────────────────────────┤
│ port       │ unbounded sources, packets, any thread can queue,   │
│            │ several threads can Dequeue the same port           │
└────────────┴─────────────────────────────────────────────────────┘
```

`wait_one` is what `zxsh` uses. A loop that already has a port never
needs it: bind, `port_wait`, re-bind. Mixing a blocking `wait_one`
**inside** a port handler stalls every other source on that port.

## 4. Dispatcher contract

Clients never call into a loop by name. They call
`async_begin_wait` / `async_post_task` / … on an
`async_dispatcher_t*`. That pointer's first word is `const async_ops_t*`.

```text
┌────────────────────────────────────────────────────────────────┐
│ struct async_dispatcher { const async_ops_t* ops; };           │
└────────────────────────────────────────────────────────────────┘
                             ▲
                             │ first member
┌────────────────────────────────────────────────────────────────┐
│ async_loop_t                                                   │
│   async_dispatcher_t dispatcher   loop IS a dispatcher         │
│   zx_handle_t port                                             │
│   zx_handle_t timer                                            │
│   wait_list / task_list / fifo_task_list / due_list            │
└────────────────────────────────────────────────────────────────┘
```

`async_ops_t` is versioned and append-only:

| Version | Methods |
| --- | --- |
| V1 | `now`, `begin_wait`/`cancel_wait`, `post_task`/`cancel_task`, `queue_packet`, `set_guest_bell_trap` |
| V2 | `bind_irq`/`unbind_irq`, `create_paged_vmo`/`detach_paged_vmo` |
| V3 | `get_sequence_id` / `check_sequence_id` |
| V4 | `acquire_shared_ref` / `release_shared_ref` |

`async_loop` implements **V2**. Missing methods would return
`ZX_ERR_NOT_SUPPORTED`; this implementation provides the whole V1+V2
set. It does **not** implement sequences (V3): a loop thread is a
plain thread, and `async::sequence_checker` falls back to thread id.

The point of the vtable: FIDL bindings, syslog, VFS, and
`async::Wait` take a dispatcher, not an `async_loop_t`. A test can
swap in a fake dispatcher. A specialized process can write its own
`async_ops_t`. On this image the only production implementation is
`async_loop`.

TLS default: `libasync-default.so` holds
`async_get_default_dispatcher` / `async_set_default_dispatcher`.
`kAsyncLoopConfigAttachToCurrentThread` installs the loop as that
default on create and clears it on destroy (same thread).
`kAsyncLoopConfigNoAttachToCurrentThread` does not, but worker
threads started with `async_loop_start_thread` still get it.
`kAsyncLoopConfigNeverAttachToThread` never touches TLS; the caller
must pass `loop.dispatcher()` around. console-launcher uses Never.

## 5. `async_loop`

[`sdk/lib/async-loop/loop.c`](../../../sdk/lib/async-loop/loop.c).
Thread-safe except destroy. Designed to avoid heap after create: the
wait/task structs are **owned by the client**, the loop only links
them. `async_state_t` (two words) is overlaid with a `list_node_t`.

#### Create

```text
async_loop_create(config)
  calloc async_loop_t
  zx_port_create(irq_support ? ZX_PORT_BIND_TO_INTERRUPT : 0)
  zx_timer_create(ZX_TIMER_SLACK_LATE, ZX_CLOCK_MONOTONIC)
  if make_default_for_current_thread:
      default_accessors.setter(&loop->dispatcher)
```

`irq_support` is required before `async_bind_irq`; the port must be
created with `ZX_PORT_BIND_TO_INTERRUPT`.

#### Port keys

The loop never allocates a key slab. The key **is** the object
pointer, except two sentinels that cannot be user pointers:

| Key | Packet type | Meaning |
| --- | --- | --- |
| `KEY_PORT_CONTROL` `0xF00000000001` | `ZX_PKT_TYPE_USER` | quit / wake another `zx_port_wait` |
| `KEY_TIMER_CONTROL` `0xF00000000002` | `ZX_PKT_TYPE_SIGNAL_ONE` | timer fired → drain task lists |
| `async_wait_t*` | `ZX_PKT_TYPE_SIGNAL_ONE` | object signaled |
| `async_receiver_t*` | `ZX_PKT_TYPE_USER` | `async_queue_packet` |
| `async_irq_t*` | `ZX_PKT_TYPE_INTERRUPT` | bound IRQ |
| `async_guest_bell_trap_t*` | `ZX_PKT_TYPE_GUEST_BELL` | guest bell trap |
| `async_paged_vmo_t*` | `ZX_PKT_TYPE_PAGE_REQUEST` | pager packet |

Cancel of a wait is `zx_port_cancel_key(port, 0, (uintptr_t)wait)`.
If another thread has already dequeued the packet,
`ZX_ERR_NOT_FOUND`: the handler **will** still run, and the client
must keep the struct alive until it does. That race is why
`async_cancel_wait` returning `NOT_FOUND` is not “the wait never
existed”; it often means “the callback is in flight.”

#### Wait

`async_begin_wait`:

```text
zx_object_wait_async(wait->object, loop->port,
                     (uintptr_t)wait, wait->trigger, wait->options)
list_add_head(wait_list)
```

Default options are one-shot (`ZX_WAIT_ASYNC_ONCE` is now a no-op:
waits are one-shot). After the handler, the client that wants another
notification must `async_begin_wait` again. That is the FIDL server
pattern: READABLE → `zx_channel_read` → begin_wait.

`ZX_WAIT_ASYNC_EDGE` does not fire for signals already asserted at
bind time. `ZX_WAIT_ASYNC_TIMESTAMP` / `BOOT_TIMESTAMP` put a
timestamp in the packet.

The wait struct must remain unmodified until the handler, a successful
cancel, or shutdown. The C++ `async::Wait` / `WaitOnce` / `WaitMethod`
wrappers cancel in the destructor and bind a `fit::function` or a
member.

#### Task and timer

Tasks are **not** port packets. They live on three lists under
`loop->lock`:

```text
┌────────────────────────────────────────────────────────────────┐
│ loop->lock                                                     │
│  ┌──────────────────┐ ┌──────────────────┐ ┌─────────────────┐ │
│  │ task_list        │ │ fifo_task_list   │ │ due_list        │ │
│  │ earliest         │ │ ZX_TIME_INFINITE │ │ extracted for   │ │
│  │ deadline first   │ │ _PAST  (“soon”)  │ │ dispatch        │ │
│  └──────────────────┘ └──────────────────┘ └─────────────────┘ │
└────────────────────────────────────────────────────────────────┘
```

`ZX_TIME_INFINITE_PAST` is the sentinel for “no deadline”: FIFO tasks
always run before delayed ones. If expired tasks never dry up, FIFO
tasks starve (documented in `async/task.h`).

The loop arms **one** `zx_timer` for the next deadline:

```text
┌────────────────────────────────────────────────────────────────┐
│ async_loop_restart_timer_locked                                │
│   next = head(task_list).deadline  or 0 if FIFO/due nonempty   │
└──────────────┬─────────────────────────────┬───────────────────┘
               ▼                             ▼
┌──────────────────────────────┐  ┌──────────────────────────────┐
│ INFINITE                     │  │ finite deadline              │
│ zx_timer_cancel              │  │ zx_timer_set(timer, deadline)│
│ zx_port_cancel_key           │  │ first time:                  │
│   (KEY_TIMER_CONTROL)        │  │   wait_async(timer, port,    │
│                              │  │     KEY_TIMER_CONTROL,       │
│                              │  │     ZX_TIMER_SIGNALED)       │
└──────────────────────────────┘  └──────────────────────────────┘
```

When that wait completes, `async_loop_dispatch_tasks` moves due items
onto `due_list` and runs them **one at a time**, lock dropped around
the handler, so an earlier task can cancel a later one. Only one
thread dispatches tasks (`dispatching_tasks` flag) to keep serial
order.

`async::PostTask` / `PostDelayedTask` / `PostTaskForTime` allocate a
heap `async::Task` around a `fit::closure` and post it. `async::Task`
/ `TaskClosure` / `TaskMethod` are the RAII forms.

#### Receiver, IRQ, guest bell, pager

`async_queue_packet` is `zx_port_queue` with `key = receiver*` and
`ZX_PKT_TYPE_USER`. There is no cancel: once queued, the handler will
run. Useful when many packets share one receiver.

`async_bind_irq` is `zx_interrupt_bind(irq, port, irq*, BIND)`, not
`zx_object_wait_async`. Unbind is the same syscall with `UNBIND`.
Unlike wait, unbind does **not** tell you whether a callback is in
flight; multithreaded IRQ loops must not free `async_irq_t` until
handlers have settled.

Guest bell: `zx_guest_set_trap(..., port, trap*)`. Paged VMO:
`zx_pager_create_vmo(..., port, paged_vmo*)`. Both deliver through the
same `zx_port_wait`.

#### Run, quit, shutdown, threads

```text
async_loop_run(loop, deadline, once)
  active_threads++
  do async_loop_run_once(port_wait + dispatch)  while ZX_OK && !once
  active_threads--

async_loop_run_until_idle  =  run(deadline=0)  treating TIMED_OUT as OK
```

`RunUntilIdle` is for tests. On a multi-thread loop it can return
before work posted to another thread has finished.

Quit: CAS `RUNNABLE → QUIT`, then `async_loop_wake_threads` queues one
`KEY_PORT_CONTROL` USER packet **per** `active_threads` so every
`zx_port_wait` returns. `async_loop_start_thread` spawns a `thrd`
that sets the TLS default (if accessors exist) and calls
`async_loop_run(INFINITE, false)`. Several threads may run the same
loop: waits and packets fan out; tasks stay serialized.

Shutdown: state `SHUTDOWN`, wake + join workers, then
`async_loop_cancel_all` invokes every remaining wait/task/irq/pager
handler with `ZX_ERR_CANCELED`. Destroy closes the port and timer.

State:

| State | Meaning |
| --- | --- |
| `ASYNC_LOOP_RUNNABLE` | `Run` / `StartThread` accepted |
| `ASYNC_LOOP_QUIT` | current unit of work finishes, then `Run` returns `ZX_ERR_CANCELED` until `ResetQuit` |
| `ASYNC_LOOP_SHUTDOWN` | handlers get `ZX_ERR_CANCELED`; further `Run` is `ZX_ERR_BAD_STATE` |

## 6. C++ `fpromise` on the loop

[`sdk/lib/async/executor.cc`](../../../sdk/lib/async/executor.cc).
`async::Executor` wraps a dispatcher and is a `fpromise::executor`.
`schedule_task` posts an `async_task_t` whose handler polls the
promise. `MakeDelayedPromise` / `MakePromiseForTime` /
`MakePromiseWaitHandle` turn timer and `async::WaitOnce` into
promises.

This is the C++ analogue of Rust `async fn`: a state machine that
resumes when the loop delivers a packet. FIDL C++ bindings that take
an `async_dispatcher_t*` either wait directly (`async::Wait`) or go
through this executor. They never call `fuchsia_async`.

## 7. `fuchsia_async`

[`userspace/lib/fuchsia-async/`](../../../userspace/lib/fuchsia-async/).
A futures-rs executor. It does **not** implement `async_dispatcher_t`
and does **not** use `async_loop`. It creates its own `zx::Port` and
registers `PacketReceiver`s under a monotonically increasing key
slab (`PacketReceiverMap`; keys are not reused, so a late packet for a
dropped receiver is ignored).

```text
┌──────────────────────────────────────────────────────────┐
│ LocalExecutor / SendExecutor                             │
│   EHandle                                                │
│     root_scope                                           │
│     inner: Executor                                      │
│       ┌────────────────────────────────────────────────┐ │
│       │ port              zx::Port                     │ │
│       │ ready_tasks       SegQueue<TaskHandle>         │ │
│       │ receivers         PacketReceiverMap            │ │
│       │ monotonic_timers / boot_timers                 │ │
│       └────────────────────────────────────────────────┘ │
└──────────────────────────────────────────────────────────┘
```

| Type | Threads | Task bound |
| --- | --- | --- |
| `LocalExecutor` | 1 | `!Send` OK (`Rc`, thread locals). `run_singlethreaded` |
| `SendExecutor` | N (`num_threads`) | `Send + 'static`. Load-balances ready tasks |
| `TestExecutor` | 1, fake time | tests (`run_until_stalled`) |

`#[fuchsia_async::run_singlethreaded] async fn main()` (userboot)
expands to `LocalExecutor::default().run_singlethreaded(main())`.

`component_manager` drains userboot on a `LocalExecutor`, then builds
a `SendExecutor` with `runtime_config.num_threads` for the realm.

#### How a Future waits

`OnSignals::new(handle, READABLE)` does **not** bind at construction.
First `poll`:

```text
┌────────────────────────────────────────────────────────────────┐
│ 1. signals already asserted?  →  Ready                         │
└────────────────────────────┬───────────────────────────────────┘
                             ▼ else
┌────────────────────────────────────────────────────────────────┐
│ 2. register PacketReceiver (get a key)                         │
│    zx_object_wait_async(handle, port, key, signals)            │
│    store Waker, return Pending                                 │
└────────────────────────────┬───────────────────────────────────┘
                             ▼ packet
┌────────────────────────────────────────────────────────────────┐
│ 3. receive_packet stores observed bits                         │
│    AtomicWaker::wake  →  task on ready_tasks                   │
└────────────────────────────┬───────────────────────────────────┘
                             ▼
┌────────────────────────────────────────────────────────────────┐
│ 4. try_poll the Future                                         │
│    zx_channel_read; complete or wait_async again               │
└────────────────────────────────────────────────────────────────┘
```

`fuchsia_async::Channel` / `Socket` / `Fifo` / `Timer` /
`OnInterrupt` are that pattern with extra buffering. Drop unregisters
the receiver; a packet already in the port for that key is dropped on
the floor (false notification to a later key is why keys are not
reused).

Task wake from another thread: the waker queues a USER packet with
`key = TASK_READY_WAKEUP_ID` (`u64::MAX - 1`) so a sleeping
`zx_port_wait` returns without a real object signal.

`Scope` / `Task::spawn` / `spawn_local`: spawn onto the executor.
`spawn` needs `Send`; `spawn_local` is `LocalExecutor` only. Do not
`.detach()` and leak; drop of the executor panics if Zircon objects
are still attached.

Fake time (`TestExecutor`) replaces `zx_timer` with an in-memory
heap so tests do not sleep.

## 8. FIDL on either loop

The wire format is [`ipc.md`](ipc.md). The wait is this note.

```text
┌────────────────────────────────────────────────────────────────┐
│ server  async_begin_wait / OnSignals(channel, READABLE)        │
└────────────────────────────┬───────────────────────────────────┘
                             ▼
┌────────────────────────────────────────────────────────────────┐
│ packet  READABLE                                               │
└────────────────────────────┬───────────────────────────────────┘
                             ▼
┌────────────────────────────────────────────────────────────────┐
│ zx_channel_read  →  decode header (txid, ordinal)              │
└──────────────┬─────────────────────────────┬───────────────────┘
               ▼                             ▼
┌──────────────────────────────┐  ┌──────────────────────────────┐
│ two-way                      │  │ one-way / event              │
│ handler, encode reply        │  │ handler only                 │
│ same txid, zx_channel_write  │  │                              │
└──────────────┬───────────────┘  └──────────────┬───────────────┘
               └────────────────┬────────────────┘
                                ▼
┌────────────────────────────────────────────────────────────────┐
│ begin_wait / poll OnSignals again                              │
└────────────────────────────────────────────────────────────────┘
```

C++ FIDL server: `fidl::ServerEnd<P>` + `async_dispatcher_t*`
(an `async_loop`). Rust FIDL: `fuchsia_async::Channel` inside the
binding, polled by `LocalExecutor` / `SendExecutor`.

Client two-way: pick a userspace txid, write, map txid → pending
future / completer, wait READABLE, read, complete the matching txid.
The loop never sees txids; it only delivers “this channel is
readable.”

## 9. What this image actually runs

```text
┌────────────────────────────────────────────────────────────────┐
│ userboot                                                       │
│   #[fuchsia_async::run_singlethreaded]                         │
│   drain BOOTFS, launch component_manager --boot                │
└────────────────────────────┬───────────────────────────────────┘
                             ▼
┌────────────────────────────────────────────────────────────────┐
│ component_manager                                              │
│   LocalExecutor  drain fuchsia.boot.Userboot                   │
│   SendExecutor   bootstrap realm, FIDL, capability routing     │
└──────────────┬─────────────────────────────┬───────────────────┘
               ▼                             ▼
┌──────────────────────────────┐  ┌──────────────────────────────┐
│ console-launcher             │  │ critical-services /          │
│   async::Loop NeverAttach    │  │ kernel_debug_broker          │
│   logging = loop.dispatcher()│  │   async::Loop                │
│   fdio_spawn /boot/bin/sh    │  │   OOM watcher, debuglog      │
└──────────────┬───────────────┘  └──────────────────────────────┘
               ▼
┌──────────────────────────────┐
│ zxsh                         │
│   no loop: blocking PTY I/O  │
└──────────────────────────────┘
```

A FIDL message from a child of the bootstrap realm therefore
crosses: child `fuchsia_async` port → kernel channel →
`component_manager` `SendExecutor` port → handler. console-launcher's
C++ loop is only for its own logging and any FIDL it serves, not for
the shell it execs.

## 10. Lifetime rules

These are the bugs the APIs are built around.

1. **The wait/task struct outlives the packet.** After
   `async_begin_wait` / `async_post_task`, do not free it until the
   handler runs, cancel returns `ZX_OK`, or shutdown has invoked the
   handler with `ZX_ERR_CANCELED`. C++ wrappers cancel in `~Wait` /
   `~Task`. Rust `OnSignals` unregisters in `Drop`.
2. **Cancel `NOT_FOUND` means the handler may still run.** Another
   thread dequeued the packet. Keep the object.
3. **Waits are one-shot.** Re-arm after the handler if you still
   care. Forgetting this looks like “FIDL stopped”.
4. **Do not block the loop thread.** A `zx_object_wait_one` or a
   long computation inside a handler stalls every other wait on that
   dispatcher. Post a task, spawn a thread, or use another loop.
5. **Do not hold a sync mutex across `.await`.** The executor may
   poll you later on another task's stack (`clippy::await_holding_lock`
   is `deny` in `fuchsia-async`).
6. **AttachToCurrentThread create and destroy on the same thread.**
   Destroy checks the TLS default is still this loop.
7. **`RunUntilIdle` is not “all posted work is done”** on a
   multi-thread loop.

## 11. References

| File | Why |
| --- | --- |
| [`zircon/system/public/zircon/syscalls/port.h`](../../../zircon/system/public/zircon/syscalls/port.h) | packet types, `ZX_WAIT_ASYNC_*` |
| [`zircon/kernel/lib/syscalls/port.rs`](../../../zircon/kernel/lib/syscalls/port.rs) | `sys_port_create` / `queue` / `wait` / `cancel` / `cancel_key` |
| [`zircon/kernel/lib/syscalls/object_wait.rs`](../../../zircon/kernel/lib/syscalls/object_wait.rs) | `sys_object_wait_async`, `wait_one`, `wait_many` |
| [`zircon/kernel/object/include/object/port_dispatcher.h`](../../../zircon/kernel/object/include/object/port_dispatcher.h) | observer lifetime diagrams, `PortPacket` |
| [`zircon/kernel/object/port_dispatcher.cc`](../../../zircon/kernel/object/port_dispatcher.cc) | `QueueAndRemoveObserver`, `Dequeue`, `MakeObserver` |
| [`zircon/kernel/object/include/object/signal_observer.h`](../../../zircon/kernel/object/include/object/signal_observer.h) | `OnMatch` / `OnCancel` |
| [`zircon/kernel/object/dispatcher.cc`](../../../zircon/kernel/object/dispatcher.cc) | `MakePortObserver`, `NotifyObserversLocked`, EDGE vs level |
| [`sdk/lib/async/include/lib/async/dispatcher.h`](../../../sdk/lib/async/include/lib/async/dispatcher.h) | `async_ops_t` V1–V4 |
| [`sdk/lib/async/include/lib/async/wait.h`](../../../sdk/lib/async/include/lib/async/wait.h) | `async_wait_t` |
| [`sdk/lib/async/include/lib/async/task.h`](../../../sdk/lib/async/include/lib/async/task.h) | `async_task_t`, `ZX_TIME_INFINITE_PAST` |
| [`sdk/lib/async-loop/loop.c`](../../../sdk/lib/async-loop/loop.c) | the C loop |
| [`sdk/lib/async-loop/include/lib/async-loop/cpp/loop.h`](../../../sdk/lib/async-loop/include/lib/async-loop/cpp/loop.h) | `async::Loop` |
| [`sdk/lib/async/include/lib/async/cpp/wait.h`](../../../sdk/lib/async/include/lib/async/cpp/wait.h) | `Wait` / `WaitOnce` / `WaitMethod` |
| [`sdk/lib/async/include/lib/async/cpp/task.h`](../../../sdk/lib/async/include/lib/async/cpp/task.h) | `PostTask`, `Task` |
| [`sdk/lib/async/include/lib/async/cpp/executor.h`](../../../sdk/lib/async/include/lib/async/cpp/executor.h) | `fpromise` on a dispatcher |
| [`sdk/lib/async-loop/include/lib/async-loop/default.h`](../../../sdk/lib/async-loop/include/lib/async-loop/default.h) | Attach / NoAttach / Never configs |
| [`userspace/lib/fuchsia-async/src/runtime/fuchsia/executor/`](../../../userspace/lib/fuchsia-async/src/runtime/fuchsia/executor/) | `LocalExecutor`, `SendExecutor`, `worker_lifecycle` |
| [`userspace/lib/fuchsia-async/src/handle/zircon/on_signals.rs`](../../../userspace/lib/fuchsia-async/src/handle/zircon/on_signals.rs) | `OnSignals` Future |
| [`userspace/secular/bin/userboot/main.rs`](../../../userspace/secular/bin/userboot/main.rs) | `#[fuchsia_async::run_singlethreaded]` |
| [`userspace/sys/component_manager/src/main.rs`](../../../userspace/sys/component_manager/src/main.rs) | Local then Send executor |
| [`userspace/secular/bin/console-launcher/main.cc`](../../../userspace/secular/bin/console-launcher/main.cc) | `async::Loop` NeverAttach |

Suggested first pass: **Port Framework + principle (1)(2)(3) →
`async_loop_run_once` switch on `packet.type` → `async_begin_wait` →
FIDL READABLE handler → `fuchsia_async` `worker_lifecycle`**. That is
enough to see why `component_manager` can serve many channels on a
handful of threads and why `zxsh` does not need any of this.

## Anti-patterns

* **Free a wait/task after `cancel` returns `ZX_ERR_NOT_FOUND`.** The handler may still run.
* **Forget to `begin_wait` again.** Waits are one-shot; FIDL looks dead.
* **Block the loop thread** (`zx_object_wait_one` in a handler).
* **Hold a sync mutex across `.await`.**
* **Mix `async_loop` and `fuchsia_async` on one port.** They are independent.

## First-pass checklist

- [ ] Port Framework + observer states (1)(2)(3)
- [ ] `async_loop_run_once` switch on `packet.type`
- [ ] `async_begin_wait` → FIDL READABLE → re-arm
- [ ] `fuchsia_async` `worker_lifecycle`

## Related

* [`bootstrap.md`](bootstrap.md)
* [`memory.md`](memory.md)
* [`cpu.md`](cpu.md)
* [`interrupt.md`](interrupt.md)
* [`ipc.md`](ipc.md)
* [`async.md`](async.md)
* [`sync.md`](sync.md)

## 12. Terminology

| Term | Meaning in this note |
| --- | --- |
| Port | Kernel object (`PortDispatcher`). Bind many handles with `zx_object_wait_async`; `zx_port_wait` returns one packet. |
| Packet | `zx_port_packet_t`: `{key, type, status, 32-byte union}`. |
| `PortObserver` | Glue: on the object's observer list and the port's observer list until match or cancel. |
| `PortPacket` | Kernel wrapper around a packet. Signal-one packets live in the observer; USER packets are ephemeral. |
| Ephemeral packet | Arena-allocated (`zx_port_queue`). Cap 4096 per port. |
| `sema_` | Port semaphore. Queue `Post`s; `zx_port_wait` `Wait`s under `Blocked::PORT`. |
| Level / EDGE | Default: fire if the signal is already asserted at bind. `ZX_WAIT_ASYNC_EDGE`: only a future transition. |
| Dispatcher | `async_dispatcher_t*`: vtable for wait/task/packet/irq. Not the kernel `Dispatcher` class. |
| `async_loop` | Stock dispatcher: one port, one timer, client-owned wait/task structs. |
| Default dispatcher | Thread-local `async_dispatcher_t*` from `libasync-default`. |
| Wait | One-shot bind of an object to the loop's port; handler once per `begin_wait`. |
| Task | Deadline callback on the loop's timer lists, not a port key (except the shared timer). |
| Receiver | Stable handler for many `zx_port_queue` USER packets. |
| `fpromise` | C++ promise/future; `async::Executor` polls it on the loop. |
| `fuchsia_async` | Rust executor. Own port, `PacketReceiver` keys, polls `Future`s. |
| `LocalExecutor` | Single-thread `fuchsia_async`; `!Send` tasks OK. |
| `SendExecutor` | Multi-thread `fuchsia_async`; tasks must be `Send`. |
| `OnSignals` | Future that completes when a handle shows a signal set. |
| Sequence | Sequential domain that may hop threads (ops V3). `async_loop` has none. |
| Switch node | A step where the wait site or the running handler changes. Numbered in **Flow**. |

