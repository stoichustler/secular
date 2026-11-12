---
name: zircon-ipc
description: >
  Use when changing channels, handles, FIDL wire format, sockets, FIFOs, or the console bootstrap channel on the Secular image.
---

# Zircon IPC: Handles, Channels, and FIDL

Stop when a FIDL reply is decoded, or when `zxsh` is on serial.

## When to use

* Editing `ChannelDispatcher`, handle rights, FIDL encode/decode, or userboot messages.
* Choosing channel vs socket vs FIFO vs port.

## When not to use

* The port observer / `async_loop` — `async.md`.
* Futex implementation — `sync.md`.

> [!NOTE]
> Console image: Zircon kernel + linux-arm64-boot-shim + BOOTFS + serial
> console + `zxsh`. The kernel is an EL2 VHE host.

## 1. Framework

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
| Futex | nothing; a user word | no | libc mutex ([`sync.md`](sync.md)) |
| VMO | pages, not messages | mapping, not send | shared buffers |

Peered objects (`Channel`, `Socket`, `FIFO`, `EventPair`) are created as
a pair. `related_koid` of one end is the other. Closing one end asserts
`PEER_CLOSED` on the survivor.

## 2. Flow

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

## 3. Handles and rights

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

## 4. Signals and wait

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
becomes `READY` ([`sync.md`](sync.md)). Closing the handle while
waiting returns `ZX_ERR_CANCELED` (`ZX_SIGNAL_HANDLE_CLOSED`).

## 5. Channel

**Principle:** a two-ended queue of messages. Each message is atomic
(all bytes and handles, or none). This is how Zircon moves
**capabilities** (handles) between processes. FIDL (next section)
is userspace on top of this packet.

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

## 6. FIDL

**Principle:** FIDL is not a kernel object. It is a **language**
(`.fidl`), a **wire format** (bytes + handles), and **bindings** that
encode/decode that format. The kernel still sees a channel
`MessagePacket`. It does not know ordinals, protocols, or UTF-8.

The transport on this image is a Zircon **channel**. A FIDL method
call is one channel message (request) and, for two-way methods,
another (response) whose `txid` matches. Handles in the FIDL payload
are the same capability transfer as `zx_channel_write`.

#### Framework

```text
 .fidl  library fuchsia.foo;
        protocol Device { GetHardwareId() -> (struct { id uint32; }); }
              │
              ▼  fidlc  (IR JSON)
              ▼  fidlgen_rust_next / fidlgen_cpp
 generated bindings
              │
              ▼
 ┌─────────────────────────────────────────────────────────────────┐
 │ userspace  fidl_next  /  libfidl  (encode · decode · dispatch)  │
 │   header 16 B   payload (inline + out-of-line)   handle list    │
 └────────────────────────────┬────────────────────────────────────┘
                              │ zx_channel_write / call / read
                              ▼
 ┌─────────────────────────────────────────────────────────────────┐
 │ kernel  ChannelDispatcher                                       │
 │   MessagePacket  bytes ≤ 64 KiB, handles ≤ 64                   │
 │   no ordinal, no UTF-8, no "protocol"                           │
 └─────────────────────────────────────────────────────────────────┘
```

Four layers. Only the bottom one is Zircon.

| Layer | What it is | This tree |
| --- | --- | --- |
| Language | `.fidl` types, protocols, methods | `fidl_library()` in GN |
| Compiler | `fidlc` → IR; `fidlgen_*` → bindings | [`products/build/fidl/`](../../build/fidl/) |
| Bindings | encode / decode / client / server | `fidl_next` (Rust), wire/natural C++ |
| Transport | channel bytes + handles | `ChannelDispatcher` above |

Wire vs natural: **wire** is an overlay on the message buffer (no
copy; `fidl_next` / C++ wire). **Natural** is owned language values
(`String`, `Vec`, `zx::Channel`) after a copy out of the buffer.

#### Diagrams

Six posters. Stack, on-wire layout, handle encoding, in-place
coding, two-way txid, client/server loop.

##### 1. From `.fidl` to a channel packet

Compiler at build time. Encoder at call time. Kernel never parses
FIDL.

```text
  Device.GetHardwareId()                 // language
        │  fidlc + fidlgen_rust_next
        ▼
  Device.GetHardwareId  ORDINAL = 0x…    // generated const
        │  fidl_next encode
        ▼
  bytes:  [ hdr 16 | struct body | out-of-line … ]
  handles: [ ] or [ hVmo, hChannel, … ]
        │  zx_channel_write / zx_channel_call
        ▼
  MessagePacket on the peer
        │  zx_channel_read
        ▼
  fidl_next decode  →  ordinal dispatch  →  server handler
```

##### 2. Transactional message on the wire (v2)

Every protocol message starts with the same 16-byte header
([`zircon/system/public/zircon/fidl.h`](../../../zircon/system/public/zircon/fidl.h)
`fidl_message_header_t`). Payload is 8-byte aligned. Out-of-line
objects follow in traversal order.

```text
 byte
  0        4        6        7        8               16
  ┌────────┬────────┬────────┬────────┬────────────────┐
  │ txid   │ at_rest│ dyn    │ magic  │ ordinal        │  header
  │ u32    │ flags 2│ flags  │ = 1    │ u64            │  16 bytes
  └────────┴────────┴────────┴────────┴────────────────┘
  ┌────────────────────────────────────────────────────┐
  │ inline body   (structs, envelopes, presence words) │
  │ padded to 8                                        │
  └────────────────────────────────────────────────────┘
  ┌────────────────────────────────────────────────────┐
  │ out-of-line  (string bytes, vector elements, …)    │
  │ each object 8-aligned; depth ≤ 32                  │
  └────────────────────────────────────────────────────┘

  handle array (not in the bytes)
  ┌────┬────┬────┐
  │ h0 │ h1 │ …  │   ≤ 64; kernel moves these, not the bytes
  └────┴────┴────┘
```

| Field | Size | Meaning |
| --- | --- | --- |
| `txid` | 4 | 0 = one-way / event. Nonzero = two-way; reply copies it. |
| `at_rest_flags[0]` | 1 | bit 1 = wire format **v2** (`USE_VERSION_V2`) |
| `dynamic_flags` | 1 | bit 7 = flexible method; bit 6 = byte-overflow (large message) |
| `magic_number` | 1 | must be `1`; else `ZX_ERR_PROTOCOL_NOT_SUPPORTED` |
| `ordinal` | 8 | which method; epitaph is `0xffff_ffff_ffff_ffff` |

`fidl_init_txn_header` always sets v2 + magic 1
([`txn_header.h`](../../../sdk/lib/fidl_base/include/lib/fidl/txn_header.h)).
`fidl_next` `MessageHeader` is the same layout
([`message_header.rs`](../../../userspace/lib/fidl/rust_next/fidl_next_protocol/src/wire/message_header.rs)).

##### 3. Handles are not in the byte stream as values

On the wire a handle slot is a **presence word**. The real handle
travels in the channel's handle vector. Decode walks the words and
fills them from that vector, in order.

```text
 encode                                      decode
 zx::Vmo (handle 3 in A's table)
        │
        ▼
 bytes:   …  FIDL_HANDLE_PRESENT (0xffffffff) …
 handles: …  [ KernelHandle+rights of that VMO ] …
        │
        │  zx_channel_write  (A loses handle 3)
        ▼
 B's MessagePacket
        │  zx_channel_read
        ▼
 bytes:   …  FIDL_HANDLE_PRESENT …
 handles: …  [ new local h=7, same VMO, rights ⊆ ] …
        │  decoder replaces PRESENT with 7
        ▼
 zx::Vmo in B  (handle 7)
```

| Encoded word | Meaning |
| --- | --- |
| `FIDL_HANDLE_PRESENT` (`UINT32_MAX`) | next handle in the packet |
| `FIDL_HANDLE_ABSENT` (`ZX_HANDLE_INVALID`) | nullable handle is null |

Same idea for out-of-line pointers: `FIDL_ALLOC_PRESENT`
(`UINTPTR_MAX`) vs `FIDL_ALLOC_ABSENT` (`0`). Decode patches the
word to a pointer **into this buffer**. That is why the buffer must
stay alive while a wire value is used.

##### 4. In-place encode / decode

Wire types are laid out like the C struct with pointers replaced by
presence. Encoding is a pre-order walk: write the inline object,
then append each out-of-line child, 8-byte aligned.

```text
 natural (owned)                         wire (in the buffer)

 struct { string name; handle<vmo> v; }
        │ encode
        ▼
 ┌─────────────────────────────┐  inline
 │ hdr 16                      │
 │ name: { size=5, PRESENT }   │──────┐
 │ v:    PRESENT               │──┐   │
 └─────────────────────────────┘  │   │
 ┌─────────────────────────────┐  │   ▼ out-of-line
 │ 'h' 'e' 'l' 'l' 'o' pad×3   │  │
 └─────────────────────────────┘  │
 handles: [ VMO ]  ◄──────────────┘

 decode is the reverse, in place:
   PRESENT → pointer to the next object in this buffer
   handle PRESENT → pop from the handle vector
```

Strings: UTF-8 bytes, **not** NUL-terminated; may contain NULs.
Vectors: `(count, presence)` + element array out-of-line. Bounds
(`string:40`, `vector<T>:N`) are not on the wire; the decoder
checks them. Envelope (v2, tables/unions): 8 bytes; values ≤ 4
bytes are **inlined** (`FIDL_ENVELOPE_INLINING_SIZE_THRESHOLD`).

`fidl_next` encodes into `Chunk`s of 8 bytes (`CHUNK_SIZE`) so the
buffer is already aligned for in-place decode.

##### 5. Two-way call and `txid`

The kernel matches replies for `zx_channel_call`. Userspace FIDL
may use that (sync) or allocate its own `txid` and demux on read
(async).

```text
 ── sync: zx_channel_call ──

 client  encode request, txid field unused
        zx_channel_call(h, req, deadline)
              │  kernel allocate_txid  (≥ 0x8000_0000, bit 31)
              │  stamp txid into the header
              │  park MessageWaiter on this end
              ▼
        write request onto the peer
 server zx_channel_read  →  decode  →  handler
        encode response, copy request.txid
        zx_channel_write onto the client end
              │  waiter matches txid  →  not queued
              ▼
        call returns the reply packet
        client decode

 ── async: write + port ──

 client  pick a userspace txid  (bit 31 clear)
        zx_channel_write(request)
        map txid → pending future
 server  same as above
 client  zx_channel_read / port packet
        lookup txid  →  complete the future
```

`FIDL_TXID_NO_RESPONSE` is 0: one-way methods and events. A
two-way request must not use 0. Kernel-generated ids have bit 31
set so they cannot collide with userspace ids
(`MIN_KERNEL_GENERATED_TXID` in
[`channel_dispatcher.rs`](../../../zircon/kernel/object/channel_dispatcher.rs)).

##### 6. Client / server on a channel

One channel, two roles. The server reads, switches on `ordinal`,
runs a handler, optionally writes a reply. Unknown ordinal: strict
method → close; flexible method → `FIDL_FRAMEWORK_ERR_UNKNOWN_METHOD`
(RFC-0138) if two-way.

```text
 client process                         server process
 ┌──────────────────────┐               ┌──────────────────────┐
 │ ClientEnd            │               │ ServerEnd            │
 │  pending txids       │               │  handler table       │
 │  fuchsia_async port  │               │  fuchsia_async port  │
 └──────────┬───────────┘               └──────────▲───────────┘
            │ channel A                            │ channel B
            └──────────── kernel packet ───────────┘

 close: server may write an epitaph first
   hdr.ordinal = 0xffff_ffff_ffff_ffff
   body: zx_status_t   then zx_handle_close
```

`component_manager` is this picture: it holds one end of each
routed protocol channel, the child holds the other
([`microkernel.md`](../microkernel.md) capability routing).

#### Language and compiler

A `.fidl` file declares a **library**, **types**, and **protocols**.
A protocol is a set of methods. Each method is one-way (`Foo(…);`)
or two-way (`Foo(…) -> (…);`). `compose` inherits another protocol.
`resource` types may contain handles; without it, the type is
persistable (no handles).

```fidl
library fuchsia.examples.gizmo;

@discoverable
closed protocol Device {
    strict GetHardwareId() -> (struct { id uint32; });
    flexible GetEvent() -> (resource struct { event zx.Handle:EVENT; });
};
```

Build:

```text
 fidl_library("fuchsia.examples.gizmo") { sources = [ "gizmo.fidl" ] }
        │
        ▼
 fidlc     →  IR JSON  (validated against schema)
 fidlgen_rust_next  →  fidl_next_fuchsia_examples_gizmo
 fidlgen_cpp        →  wire / natural C++ (if a C++ target depends)
```

The 64-bit **ordinal** is hashed from library + protocol + method
at compile time and baked into the bindings (`Method::ORDINAL` in
`fidl_next_bind`). Two ends must be generated from the same IR or
they will not agree.

#### Bindings in this tree

| Binding | Language | Role on Secular |
| --- | --- | --- |
| **`fidl_next`** | Rust | console path: `component_manager`, userboot FIDL, `fuchsia_async` |
| C++ **wire** | C++ | overlay on the buffer; drivers / leftover C++ |
| C++ **natural** | C++ | owned values; copy out of the buffer |
| **HLCPP** | C++ | older; do not add new uses |

`fidl_next` is three crates
([`userspace/lib/fidl/rust_next/`](../../../userspace/lib/fidl/rust_next/)):

```text
 fidl_next_codec      Wire, Encode, Decode, Chunk, handles/strings
        │
 fidl_next_protocol   Transport, MessageHeader, Client/Server untyped
        │               fuchsia::channel uses zx_channel_read/write
 fidl_next_bind       typed Client<P>, Server<P>, Method::ORDINAL
```

The channel transport
([`fidl_next_protocol/src/fuchsia/channel.rs`](../../../userspace/lib/fidl/rust_next/fidl_next_protocol/src/fuchsia/channel.rs))
holds a `zx::Channel` in a `fuchsia_async::RWHandle`, encodes into
`Buffer { chunks, handles }`, and calls `zx_channel_write`. It is
executor-agnostic at the protocol layer; on Fuchsia the executor is
`fuchsia_async` (port wait, [`rust.md`](../rust.md) Async).

C++ wire encode/decode lives in
[`sdk/lib/fidl/cpp/wire/`](../../../sdk/lib/fidl/cpp/wire/).
`fidl_message_header_t` and presence constants are the C ABI in
[`zircon/fidl.h`](../../../zircon/system/public/zircon/fidl.h).

#### Flow

Two-way method, async, handles in the response:

```text
 client  Device.GetEvent()
        encode MessageHeader { txid=T, ordinal=GetEvent, v2, magic=1 }
        zx_channel_write(bytes, handles=[])
        wait on port (channel READABLE)

 kernel  packet on server end, raise READABLE
        maybe wake zx_port_wait

 server  zx_channel_read
        validate magic / v2
        switch ordinal
          GetEvent → handler → zx_event_create
        encode reply: same txid T, event = PRESENT
        zx_channel_write(bytes, handles=[event])

 kernel  move event handle into client's table
        raise client READABLE

 client  read, match txid T, decode
        event slot ← new local handle
        future completes with zx::Event
```

Epitaph then close:

```text
 server  encode epitaph (ordinal all-ones, error: zx_status_t)
        zx_channel_write
        zx_handle_close(server end)
 kernel  client sees PEER_CLOSED after the epitaph packet
```

#### What the kernel does not do

| FIDL concept | Kernel |
| --- | --- |
| ordinal / protocol name | opaque bytes |
| UTF-8 string | opaque bytes |
| handle presence words | opaque bytes; **real** handles are the packet's handle vector |
| txid (async) | opaque, except `zx_channel_call` matches it |
| epitaph | last message, then `PEER_CLOSED` |
| flexible unknown method | userspace policy |

If you can see it with `zx_channel_read` as bytes+handles, it is
not a kernel FIDL object. Debug a bad FIDL call as: wrong ordinal
(bindings mismatch), truncated message, handle count off, or peer
closed — then look at the channel, not at a FIDL syscall.

## 7. Socket

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

## 8. FIFO

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

## 9. Event and EventPair

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

## 10. Port

**Principle:** one wait site for many objects. Bind with
`zx_object_wait_async(port, handle, key, signals)`. When the object
matches, a packet `{key, observed signals}` is queued. `zx_port_wait`
blocks on that queue (`Blocked::PORT`). The full observer / packet /
cancel map is [`async.md`](async.md) **Port**.

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

## 11. Futex

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
        │  *word != expected  →  ZX_ERR_BAD_STATE (do not sleep)
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
Libc `sync_mutex_t` is this ([`sync.md`](sync.md)). It is IPC only if
two processes map the same page and agree on the word.

## 12. VMO (shared memory)

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

## 13. Console path

```text
┌────────────────────────────────────────────────────────────────┐
│ userboot_init  (kernel)                                        │
│   channel ── two handle-only msgs ──► userboot EL0             │
└────────────────────────────┬───────────────────────────────────┘
                             ▼
┌────────────────────────────────────────────────────────────────┐
│ userboot                                                       │
│   unpack BOOTFS                                                │
│   launch libexec/component_manager --boot                      │
└────────────────────────────┬───────────────────────────────────┘
                             ▼
┌────────────────────────────────────────────────────────────────┐
│ component_manager                                              │
│   FIDL channels → console-launcher                             │
└────────────────────────────┬───────────────────────────────────┘
                             ▼
┌────────────────────────────────────────────────────────────────┐
│ console-launcher                                               │
│   kernel serial syscalls                                       │
│   fdio_spawn /boot/bin/sh                                      │
└────────────────────────────┬───────────────────────────────────┘
                             ▼
┌────────────────────────────────────────────────────────────────┐
│ zxsh   stdio is serial, not a channel                          │
└────────────────────────────────────────────────────────────────┘
```

`zxsh` on this image talks to the kernel console, not to a userspace
TTY channel. The IPC you will see on serial boot is: bootstrap
channel (handle-only, **not** FIDL), then `component_manager`'s
**FIDL** channels to launchers. UART IRQ → wait wakeup is
[`interrupt.md`](interrupt.md) + [`sync.md`](sync.md),
not a Zircon socket.

## 14. How to choose

```text
┌────────────────────────────────┬───────────────────────────────────┐
│ Need a typed RPC / protocol?   │ FIDL. Kernel still a Channel      │
│ Need to send a handle?         │ Channel                           │
│ Need an event loop?            │ Port + wait_async ([async.md])    │
│ Need to sleep on one object?   │ zx_object_wait_one                │
│ Need bytes, no handles?        │ Socket, or VMO + Channel/FIFO     │
│ Need tiny fixed records?       │ FIFO (+ VMO for payload)          │
│ Need only signalled / hangup?  │ Event / EventPair                 │
│ Need a userspace mutex?        │ futex (sync_mutex), not Mutex     │
└────────────────────────────────┴───────────────────────────────────┘
```

## 15. References

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
| [`zircon/system/public/zircon/fidl.h`](../../../zircon/system/public/zircon/fidl.h) | `fidl_message_header_t`, presence, envelope |
| [`sdk/lib/fidl_base/include/lib/fidl/txn_header.h`](../../../sdk/lib/fidl_base/include/lib/fidl/txn_header.h) | `fidl_init_txn_header` |
| [`userspace/lib/fidl/rust_next/`](../../../userspace/lib/fidl/rust_next/) | `fidl_next` codec / protocol / bind |
| [`sdk/lib/fidl/cpp/wire/`](../../../sdk/lib/fidl/cpp/wire/) | C++ wire encode/decode |
| [`products/build/fidl/`](../../build/fidl/) | `fidl_library`, fidlc, fidlgen |
| [`sdk/lib/async/rust/fidl/`](../../../sdk/lib/async/rust/fidl/) | `fuchsia_async` + `fidl_next` channel |

Suggested first pass: **handle table → `zx_channel_write` → peer
`READABLE` → `wait_one` → `zx_channel_read` (handles appear in the
receiver)**. Then **port wait_async**. Then **FIDL header + ordinal +
txid** on those same bytes. That is enough to see userboot's two
messages (not FIDL), and a FIDL call from `component_manager`.

## Anti-patterns

* **`zx_object_wait_one` inside a port handler.** It stalls every other source.
* **Put userboot in BOOTFS.** It unpacks BOOTFS; it lives in `STORAGE_KERNEL`.
* **Assume `zxsh` speaks FIDL.** Its stdio is kernel serial.

## First-pass checklist

- [ ] Handle + rights on a Channel
- [ ] FIDL header (txid, ordinal) on that channel
- [ ] Port wait_async for many channels — then `async.md`

## Related

* [`bootstrap.md`](bootstrap.md)
* [`memory.md`](memory.md)
* [`cpu.md`](cpu.md)
* [`interrupt.md`](interrupt.md)
* [`ipc.md`](ipc.md)
* [`async.md`](async.md)
* [`sync.md`](sync.md)

## 16. Terminology

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
| `txid` | Channel-call / FIDL two-way id. 0 = one-way. Kernel ids have bit 31 set. |
| Port packet | `{key, signals}` queued when an async wait matches. |
| Futex | Kernel wait keyed by a user address, not by a handle. |
| FIDL | Userspace IDL + wire format on a channel. Not a kernel object. |
| ordinal | 64-bit method id in the FIDL header. Epitaph is all ones. |
| Wire / natural | Overlay on the buffer vs owned language values. |
| `FIDL_HANDLE_PRESENT` | `0xffffffff` in the bytes; the real handle is in the packet. |
| Envelope (v2) | 8-byte wrapper for table/union fields; ≤4 bytes inlined. |
| Epitaph | Last FIDL message before close; body is a `zx_status_t`. |
| `fidl_next` | Rust bindings: codec + protocol + typed bind. |

