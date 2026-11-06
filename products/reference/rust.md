# Rust

Secular is a Zircon-microkernel OS. A large and growing part of it is
written in **Rust**: kernel objects and syscalls under `zircon/kernel/`,
and userspace programs such as userboot and `component_manager`. This
note is a tutorial for that language, keyed to this tree. It is not a
substitute for [The Rust Book](https://doc.rust-lang.org/book/), but it
is enough to read the code that boots the serial console.

The product stays:

```text
zircon kernel + linux-arm64-boot-shim + BOOTFS + serial console + zxsh
```

CPU / MMU / IPC / locks: [`zircon/cpu.md`](zircon/cpu.md),
[`zircon/memory.md`](zircon/memory.md), [`zircon/ipc.md`](zircon/ipc.md),
[`zircon/synchronization.md`](zircon/synchronization.md). Boot:
[`zircon/bootstrap.md`](zircon/bootstrap.md). Toolchain: `SECULAR_SDK`
(`../secular-sdk` by default); there is no in-tree `prebuilt/` rustc.

## Where Rust sits

```text
 QEMU virt
        │
        ▼
 zircon kernel EL1
   C++  Thread, VM, GIC, …
   Rust sys_*, ChannelDispatcher, FutexContext, scheduler.rs
        │  vDSO  svc
        ▼
 userspace EL0
   Rust  userboot, component_manager, zx crate
   C     zxsh / musl
```

| Layer | `std`? | Typical crates | Blocking model |
| --- | --- | --- | --- |
| Kernel (`zircon/kernel/**/*.rs`) | **no** (`#![no_std]`) | `core`, `zx_status`, `ksync`, `fbl` | thread blocks in the syscall |
| Userspace | yes (`target_os = "fuchsia"`) | `zx`, `anyhow`, FIDL, `fuchsia_async` | syscall or async executor on a port |

Build is **GN** (`rustc_library` / `rustc_binary` in
[`products/build/rust/`](../build/rust/)), not Cargo, for the product
image. rustc comes from `SECULAR_SDK`.

## Design core

Rust's bet: **memory safety and data-race freedom without a garbage
collector**, by making ownership a type-system fact. That is why it
fits a microkernel: the same discipline as careful C++, checked by the
compiler, with an explicit `unsafe` hatch for FFI and page tables.

### Ownership

Every value has one owner. When the owner goes out of scope, `Drop`
runs (RAII). Assignment **moves** unless the type is `Copy` (integers,
`bool`, …).

```text
 let h = JobDispatcher::create(…)?;   // h owns the handle
 let h2 = h;                          // move: h is gone
 // drop(h2) closes / frees
```

In this tree, kernel objects are usually `fbl::RefPtr<T>` (shared
refcount, like `Arc`) or `KernelHandle<T>` (unique until inserted in a
handle table). Userspace `zx::Job` owns a handle; `Drop` closes it.

### Borrowing

You may lend a value without giving it away:

| Form | Meaning | Alias |
| --- | --- | --- |
| `&T` | shared, read-only | many at once |
| `&mut T` | exclusive, writable | **one** at a time |
| `T` | owned | move |

The compiler rejects two `&mut` to the same place, or a `&mut` while
`&` exists. That is the data-race rule for a single thread; `Send` /
`Sync` extend it across threads.

```rust
fn sys_job_create(
    parent_job: HandleValue,
    options: u32,
    out: &mut HandleValue,   // exclusive borrow of the out-slot
) -> Result<(), Status> {
    // …
    *out = up.make_and_add_handle(handle, rights)?;
    Ok(())
}
```

([`zircon/kernel/lib/syscalls/job.rs`](../../zircon/kernel/lib/syscalls/job.rs))

### Lifetimes

A reference must not outlive its owner. `'a` is a name for that span.
You rarely write lifetimes in this tree until you store a reference in
a struct; `UserInPtr<T>` is a pointer, not a Rust `&T`, because user
memory can page-fault and is not trusted.

### `unsafe`

The language's safety story stops at `unsafe`. Inside it you may
dereference raw pointers, call `extern "C"`, implement `unsafe trait`s.
**Every** `unsafe` block in this project needs a `// SAFETY:` comment
that says why the operation is sound (see
[`tools/ai-coding/skills/rust_best_practices`](../../tools/ai-coding/skills/rust_best_practices/SKILL.md)).

```text
 safe Rust          ownership, borrows, no data races
        │
        ▼
 unsafe { … }       you promised the same invariants
        │
        ▼
 C++ / hardware     Thread*, PTE, user copy
```

## Language features

| Feature | What it is | In this tree |
| --- | --- | --- |
| `struct` / `enum` | product and sum types | `Lifecycle`, `FutexId`, `Status` |
| `impl` | methods on a type | `ChannelDispatcher::write` |
| `trait` | interface + generics | `DispatcherOps`, `From<Status>` |
| `match` | exhaustive split on enums | thread lifecycle, ESR classes |
| `mod` | privacy boundary (not the type) | `zircon/kernel/lib/syscalls/` |
| `use` | import; keep them at file top | — |
| macros | `#[syscall]`, `define_kcounter!`, `pin_init!` | generate veneers / constructors |
| `const` / `static` | compile-time / process-lifetime | `ZX_CHANNEL_MAX_MSG_BYTES` |
| `Result` / `Option` | errors and absence, not null | almost every syscall |

Naming: `CamelCase` types, `snake_case` functions, getters **without**
`get_` (`width()`, not `get_width()`). Conversions: `as_` cheap,
`to_` allocating, `into_` consuming.

## Data types

### Scalars and compounds

```rust
u8 u16 u32 u64 usize     unsigned (usize = pointer width; 64-bit here)
i8 … isize               signed
bool  char               Unicode scalar, not a C char
f32 f64                  kernel almost never uses floats in hot paths

(T, U)                   tuple
[T; N]                   array, size in the type
[T]                      slice; always behind & or Box
struct S { x: u32 }      named fields, private by default
enum E { A, B(u32) }     tagged union; no C-style implicit int
```

User strings in syscalls are `UserInPtr<u8>` + length, copied into a
kernel buffer, not `&str` until UTF-8 is proven.

### `Option` and `Result`

```rust
enum Option<T> { None, Some(T) }
enum Result<T, E> { Ok(T), Err(E) }
```

No null. `peer().ok_or(Status::PEER_CLOSED)?` turns a missing peer
into an error. `?` returns early on `Err` / `None` (with `?` on
`Option` needing a compatible return type).

Kernel syscalls return `Result<(), Status>` or `Result<T, Status>`.
`Status` is the Zircon `zx_status_t` (`ZX_OK`, `ZX_ERR_PEER_CLOSED`,
…). Userspace `zx::Job::create_child_job` is the same idea.

### Newtypes

A tuple struct wraps a primitive so you cannot mix it up:

```rust
pub struct FutexId(usize);      // VA >> 2, not a random integer
pub struct HandleValue(/* … */);
```

Prefer this over raw `u64` for koids, rights, and pointers.

### Pointers you will see

| Type | Layer | Role |
| --- | --- | --- |
| `&T` / `&mut T` | both | borrow; compiler-checked |
| `*const T` / `*mut T` | FFI, user copy | raw; only in `unsafe` |
| `Box<T>` | userspace / some kernel | unique heap |
| `Arc<T>` | userspace | atomic shared |
| `fbl::RefPtr<T>` | kernel | intrusive refcount |
| `UserInPtr<T>` / `UserOutPtr<T>` | kernel | user VA; copy may fault |
| `Pin<P>` | kernel objects | address must not move (`pin_init!`) |

`String` vs `&str`: `String` owns UTF-8 bytes; `&str` borrows them.
Kernel code often stays on `&[u8]` until it must interpret text.

## Traits, generics, `impl`

```rust
fn make_and_add_handle<T: Dispatcher>(
    handle: KernelHandle<T>,
    rights: zx_rights_t,
) -> Result<HandleValue, Status>
```

`T: Dispatcher` is a bound. Common traits: `Clone`, `Copy`, `Debug`,
`Default`, `From` / `Into`, `Send` / `Sync`. Implement `From<A> for B`
rather than an ad-hoc `a_to_b()`.

`dyn Trait` is a trait object (runtime dispatch). Kernel hot paths
prefer static generics.

## Error handling

```text
 syscall body
    get_dispatcher_with_rights(...)?;   // Err → return that Status
    JobDispatcher::create(...)?;
    Ok(())
         │
         ▼
 C++ wrapper  zx_status_t  (negative errno-style)
         │
         ▼
 vDSO / zx crate  Result<T, Status>
```

- Kernel: `Status`, not `std::io::Error`. Do not `unwrap()` on a
  user-supplied handle.
- Userspace: `zx::Status`; `anyhow::Error` in programs like userboot
  for context. `?` is the default control flow.
- Panic (`unwrap`, array OOB) kills the **process** (userspace) or the
  **kernel** (don't). Checked math (`checked_add`) on untrusted sizes.

## Sync and async

### `Send` and `Sync`

| Trait | Meaning |
| --- | --- |
| `Send` | this value may move to another thread |
| `Sync` | `&T` may be used from another thread (`T: Sync` iff `&T: Send`) |

`Rc` / `RefCell` are neither (single-thread). `Arc` / `Mutex` are
both. A type with a raw pointer is `Send`/`Sync` only if you
`unsafe impl` and document why.

### Kernel: blocking, not async

A syscall runs on the current `Thread`. If it must wait, that thread
goes `BLOCKED` on a `WaitQueue` ([`zircon/synchronization.md`](zircon/synchronization.md)).
There is no `async fn` in `sys_*`. Locks:

| Primitive | Rust spelling here | When |
| --- | --- | --- |
| Spinlock | `ksync` + `IrqSave` | IRQ off, never block |
| Mutex | `CriticalMutex` / `KMutex` | dispatcher; may block |
| Futex | `FutexContext` | **user** word; kernel only sleeps waiters |

```text
 EL0  zx_channel_read  (may be Rust zx crate)
        svc
 EL1  sys_channel_read   (Rust, #[syscall])
        empty?  thread BLOCKED
        peer write → READABLE → READY
        eret
```

### Userspace: futex + optional async

Uncontended locks are user atomics on a `zx_futex_t`
([`zircon/synchronization.md`](zircon/synchronization.md) Futex).
`sync_mutex_t` is that protocol.

Async is a **userspace** executor. `async fn` compiles to a state
machine (`Future`). Something must `poll` it: `fuchsia_async` waits on
a Zircon **port** (`zx_object_wait_async` / `zx_port_wait`,
[`zircon/ipc.md`](zircon/ipc.md)). Kernel Rust does not drive that
loop.

```text
 userspace async
   async fn handle_fidl() { channel.read().await; }
        │
        ▼
 fuchsia_async executor
   zx_port_wait  →  wake the Future
        │
        ▼
 same syscalls as sync code (channel_read, …)
```

Rules of thumb:

- Kernel / syscall path: **sync**, block the thread.
- `component_manager` / FIDL servers: **async** on `fuchsia_async`.
- `spawn` vs `spawn_local`: `spawn` needs `Send`; `spawn_local` for
  `Rc`. Do not `.detach()` tasks.
- Prefer `fuchsia_sync::Mutex` over `std::sync::Mutex` in Fuchsia
  userspace when a mutex is required.

## FFI

Two directions, one ABI (AAPCS64, `extern "C"`).

### Userspace → kernel (syscalls)

The vDSO is assembly (`svc`). The `zx` crate wraps it:

```rust
// sdk/rust/zx/src/job.rs
let status = unsafe { sys::zx_job_create(parent_job_raw, options, &mut out) };
ok(status)?;
```

You almost never write that `unsafe` yourself; call `Job::create_child_job`.

### Kernel C++ ↔ kernel Rust

```text
 exceptions.S  svc
      │
      ▼
 wrapper_job_create   (generated C++)
      │
      ▼
 sys_job_create       (Rust, #[syscall])
      │
      ├─ JobDispatcher::create     Rust
      └─ cpp_thread_*              unsafe extern "C" into leftover C++
```

`#[syscall]` on `sys_*` generates the C-callable veneer
([`zircon/kernel/lib/syscalls/macros`](../../zircon/kernel/lib/syscalls/macros)).
C++ still owns some types; Rust sees them as `unsafe extern "C" { fn
cpp_thread_dispatcher_alloc(...); }` and `OpaqueStorage`.

Bindgen (`products/build/rust/rustc_bindgen.gni`) turns headers into
Rust. Do not hand-write duplicate layouts.

### Safety comments

```rust
// SAFETY: `process_raw` is a valid pointer carrying an acquired refcount.
let handle = unsafe {
    KernelHandle::create(|out| cpp_thread_dispatcher_alloc(process_raw, flags, out))
}?;
```

Format is exactly `// SAFETY:`. Unsafe operations inside an `unsafe
fn` still need an inner `unsafe { }` block in this project.

## How to work in this tree

### Build

```sh
export SECULAR_SDK=/absolute/path/to/secular-sdk   # default: ../secular-sdk
make build    # GN + ninja, including rustc_library targets
```

Add Rust with `rustc_library` / `rustc_binary` in `BUILD.gn`. New
targets: `edition = "2024"`. List `sources`. Do not add a second
Cargo/CMake tree. Do not vendor rustc.

Host rustc and target libs come from the SDK. `SECULAR_PREBUILT`
(`out/sdk-prebuilt`) is a **build output** rustlib view, not source.

### Kernel crate

```rust
#![cfg_attr(not(test), no_std)]
```

No `std::vec::Vec` unless the crate provides alloc. No `println!`;
use `debug::ltracef` / kernel printf. Tests may enable `std` via
`cfg(test)`.

### Userspace crate

`target_os = "fuchsia"` (Zircon). Use `zx` for syscalls, not the
deprecated `fuchsia_zircon` crate. FIDL: `fidl_next` / generated
bindings. userboot:
[`userspace/secular/bin/userboot/main.rs`](../../userspace/secular/bin/userboot/main.rs).

### A first reading path

1. [`sdk/rust/zx/src/job.rs`](../../sdk/rust/zx/src/job.rs) — safe
   wrapper around `zx_job_create`.
2. [`zircon/kernel/lib/syscalls/job.rs`](../../zircon/kernel/lib/syscalls/job.rs) —
   `#[syscall] sys_job_create`, `Result<(), Status>`, `?`.
3. [`zircon/kernel/object/channel_dispatcher.rs`](../../zircon/kernel/object/channel_dispatcher.rs) —
   `RefPtr`, peered state, `CriticalMutex`.
4. [`zircon/kernel/object/futex_context.rs`](../../zircon/kernel/object/futex_context.rs) —
   `FutexId`, wait/wake, `unsafe` user copy.
5. userboot `main.rs` — `anyhow`, `zx::Vmo`, FIDL, no kernel locks.

## Where in the tree

| Path | Why |
| --- | --- |
| [`zircon/kernel/lib/syscalls/`](../../zircon/kernel/lib/syscalls/) | `sys_*`, `#[syscall]` |
| [`zircon/kernel/lib/syscalls/macros/`](../../zircon/kernel/lib/syscalls/macros/) | proc-macro for those veneers |
| [`zircon/kernel/object/`](../../zircon/kernel/object/) | dispatchers in Rust (channel, futex, job, …) |
| [`zircon/kernel/kernel/scheduler.rs`](../../zircon/kernel/kernel/scheduler.rs) | scheduler in Rust |
| [`sdk/rust/zx/`](../../sdk/rust/zx/) | userspace syscall bindings |
| [`userspace/secular/bin/userboot/`](../../userspace/secular/bin/userboot/) | first user process |
| [`products/build/rust/`](../build/rust/) | `rustc_library.gni`, bindgen, clippy |
| [`tools/ai-coding/skills/rust_best_practices/`](../../tools/ai-coding/skills/rust_best_practices/) | rubric used in this tree |

## Terminology

| Term | Meaning in this note |
| --- | --- |
| Ownership | Unique owner; move on assign; `Drop` at end of scope. |
| Borrow | `&T` shared, `&mut T` exclusive. |
| Lifetime | How long a borrow is valid. |
| `Copy` | Assignment duplicates bits (integers); not a move. |
| `Drop` | Destructor. Must not panic. Handle close lives here. |
| `Result` / `Option` | Error and absence. Prefer `?` over `unwrap`. |
| `Status` | Zircon error type (`zx_status_t`) in both kernel and `zx`. |
| `no_std` | No Rust standard library. Kernel crates. `core` remains. |
| `unsafe` | Opt out of the checker. Needs `// SAFETY:`. |
| FFI | Calling C/C++ (`extern "C"`) or being called from it. |
| `#[syscall]` | Marks `sys_*`; generates the kernel entry from user `svc`. |
| `Send` / `Sync` | Cross-thread move / shared reference. |
| `Future` / `.await` | Userspace async state machine. Kernel syscalls do not use it. |
| `fuchsia_async` | Executor that polls futures using Zircon ports/timers. |
| `RefPtr` | Kernel intrusive shared pointer (`fbl`). |
| `UserInPtr` | Pointer to user memory; copy, do not dereference as `&T`. |
| GN `rustc_library` | How this repo compiles Rust. Not Cargo for the image. |
| `zx` crate | Userspace bindings. Not `fuchsia_zircon`. |
| Edition | Language version. New targets: 2024. |

---

Everyday's a Hustle.
