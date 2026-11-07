# Rust

Secular is a Zircon-microkernel OS. A large and growing part of it is
written in **Rust**: kernel objects and syscalls under `zircon/kernel/`,
and userspace programs such as userboot and `component_manager`. This
note is a tutorial for that language, keyed to this tree. It is not a
substitute for [The Rust Programming Language](https://doc.rust-lang.org/stable/book/)
(the Book: edition 2024, rustc 1.85+), but it is enough to read the
code that boots the serial console.

The product stays:

```text
zircon kernel + linux-arm64-boot-shim + BOOTFS + serial console + zxsh
```

CPU / MMU / IPC / locks: [`zircon/cpu.md`](zircon/cpu.md),
[`zircon/memory.md`](zircon/memory.md), [`zircon/ipc.md`](zircon/ipc.md),
[`zircon/synchronization.md`](zircon/synchronization.md). Boot:
[`zircon/bootstrap.md`](zircon/bootstrap.md). Toolchain: `SECULAR_SDK`
(`../secular-sdk` by default); there is no in-tree `prebuilt/` rustc.

How this note maps onto the Book:

| Book | This note |
| --- | --- |
| Ch. 3 Common Programming Concepts | Variables, types, functions, control flow |
| Ch. 4 Understanding Ownership | Ownership, borrowing, slices, lifetimes |
| Ch. 5 Using Structs | `struct`, `impl`, methods |
| Ch. 6 Enums and Pattern Matching | `enum`, `match`, `if let` |
| Ch. 7 Packages, Crates, and Modules | `mod` / `pub` / GN crates (not Cargo) |
| Ch. 8 Common Collections | `Vec`, `String`, maps |
| Ch. 9 Error Handling | `panic!`, `Result`, `?`, `Status` |
| Ch. 10 Generics, Traits, and Lifetimes | bounds, `dyn`, `'a` |
| Ch. 11 Automated Tests | `#[test]` / `#[fuchsia::test]` |
| Ch. 13 Iterators and Closures | `Fn*` traits, iterator adapters |
| Ch. 15 Smart Pointers | `Box`, `Rc`/`Arc`, `RefCell`, `Pin`, `RefPtr` |
| Ch. 16 Fearless Concurrency | `Send`/`Sync`, mutex, threads |
| Ch. 17 OOP features | encapsulation, trait objects |
| Ch. 18 Patterns | pattern syntax |
| Ch. 19 Advanced Features | `unsafe`, FFI, macros |
| Cargo / crates.io chapters | skipped: this tree compiles with GN |

Start with **Framework** and **Flow**, then the language sections they
index. Kernel examples are `#![no_std]`; userspace examples may use
`std`.

## Rust

### Framework

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
image. rustc comes from `SECULAR_SDK`. New targets: `edition = "2024"`.

Rust's bet, which is why it fits a microkernel: **memory safety and
data-race freedom without a garbage collector**, by making ownership a
type-system fact. The same discipline as careful C++, checked by the
compiler, with an explicit `unsafe` hatch for FFI and page tables.

```text
 compile-time                         runtime
 let x = T          move / Drop       destructor (RAII)
 &T / &mut T        borrow checker    no extra cost
 match E            exhaustive        jump / tables
 T: Trait           monomorphize      static call
 dyn Trait          vtable            indirect call
 Result / ?         type-checked      early return
 unsafe { }         you promised      raw pointer / FFI
 async fn           state machine     poll / .await  (userspace only)
```

### Flow

A syscall is one function. Ownership, `Result`, and `match` are the
control flow; C++ is only at the edges.

```text
 EL0  zx::Job::create_child_job
        unsafe { sys::zx_job_create(...) }     // zx crate, once
        svc
 EL1  wrapper_job_create                       // generated C++
        sys_job_create                         // Rust, #[syscall]
          get_dispatcher_with_rights::<JobDispatcher>(...)?
          JobDispatcher::create(...)?
          *out = make_and_add_handle(...)?
          Ok(())                               // or Err(Status)
        eret
 EL0  ok(status)?;  Job owns the handle; Drop closes it
```

([`sdk/rust/zx/src/job.rs`](../../sdk/rust/zx/src/job.rs),
[`zircon/kernel/lib/syscalls/job.rs`](../../zircon/kernel/lib/syscalls/job.rs))

## Common programming concepts

Book Ch. 3. Immutable by default; types are static; `if` and `match`
are expressions.

### Variables, mutability, shadowing

```rust
let rights = ZX_RIGHT_MANAGE_JOB;   // immutable
let mut out: HandleValue = /* … */; // mutable; must be written before use
let out = out;                      // shadow: new binding, maybe new type
```

- `let` bindings are **immutable** unless marked `mut`. Mutation of a
  binding is not the same as interior mutability (`UnsafeCell`,
  `RefCell`, kernel `KCell`).
- **Shadowing** (`let x = x`) creates a new binding. It can change
  type. It is not `mut`.
- `const` is always immutable, always typed, always a compile-time
  value. `static` lives for the process (or kernel) lifetime; a
  mutable `static` is `unsafe` to touch.

```rust
const MAX_POLICY_COUNT: usize = 32;           // job.rs
const DEFAULT_NEXT_BOOT: &str = "bin/component_manager+--boot";
```

Naming: `CamelCase` types, `snake_case` functions and locals,
`SCREAMING_SNAKE_CASE` constants. Getters **without** `get_`
(`width()`, not `get_width()`). Conversions: `as_` cheap, `to_`
allocating, `into_` consuming.

### Comments

```rust
// line comment
/// rustdoc on the following item
//! rustdoc on the enclosing module / crate
/* block — rare in this tree */
```

Public items should document panic / `Err` / `unsafe` preconditions.
Comments are for maintainers, not for the agent that wrote the patch.

### Functions

```rust
fn job_set_policy_basic_v1(
    handle: HandleValue,
    options: u32,
    policy_ptr: UserInPtr<u8>,
    count: u32,
) -> Result<(), Status> {
    if options != ZX_JOB_POL_RELATIVE && options != ZX_JOB_POL_ABSOLUTE {
        return Err(Status::INVALID_ARGS);
    }
    // …
    Ok(())
}
```

Statements end in `;`. The last expression (no `;`) is the return
value. `return` is for early exits. Arguments and the return type are
annotated; locals are usually inferred.

Associated functions live in `impl` (`JobDispatcher::create`). Methods
take `self`, `&self`, or `&mut self`.

### Control flow

`if` is an expression. Branches must have a common type.

```rust
let policy = if options == ZX_JOB_POL_RELATIVE {
    Relative
} else {
    Absolute
};
```

| Form | When |
| --- | --- |
| `if` / `else if` / `else` | boolean condition; no truthy integers |
| `loop { }` | infinite; `break` / `break value` / `continue` |
| `while cond { }` | condition checked each time |
| `while let pat = expr { }` | keep going while the pattern matches |
| `for x in iter { }` | iterator; the usual form |
| `match expr { }` | exhaustive split; see Enums |

```rust
for (ix, item) in items[..count].iter().enumerate() {
    // object_wait.rs
}
```

There is no C-style `for (i = 0; i < n; i++)`. Use `0..n` or
`.enumerate()`. `loop` is the only construct that can return a value
from `break expr`.

## Ownership

Book Ch. 4. Every value has one owner. When the owner goes out of
scope, `Drop` runs (RAII). Assignment **moves** unless the type is
`Copy` (integers, `bool`, `Lifecycle`, …).

```text
 let h = JobDispatcher::create(…)?;   // h owns the handle
 let h2 = h;                          // move: h is gone
 // drop(h2) closes / frees
```

Three rules:

1. Each value has a single owner.
2. There can be only one owner at a time.
3. When the owner goes out of scope, the value is dropped.

In this tree, kernel objects are usually `fbl::RefPtr<T>` (shared
refcount, like `Arc`) or `KernelHandle<T>` (unique until inserted in a
handle table). Userspace `zx::Job` owns a handle; `Drop` closes it.

A move is a memcpy of the bits plus invalidation of the source. `Copy`
types skip the invalidation: both names remain valid. Do not implement
`Copy` for anything that must run a destructor (handles, `Box`,
locks).

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

A reference is a pointer plus a compile-time proof that it is valid.
You cannot store a `&T` to kernel or user memory that can move, be
unmapped, or outlive the stack frame it came from.

### Slices

A slice is a fat pointer `(ptr, len)` into contiguous `T`s. It does
not own the data.

```rust
let policy = policy_ptr
    .reinterpret::<zx_policy_basic_v1_t>()
    .copy_slice_from_user(slice)?;   // &[T] after the copy
```

| Type | Owns? | Notes |
| --- | --- | --- |
| `[T; N]` | yes (inline) | length in the type |
| `[T]` | no | DST; only behind a pointer |
| `&[T]` / `&mut [T]` | no | borrowed slice |
| `&str` | no | borrowed UTF-8 |
| `String` | yes | heap UTF-8 (`std` / `alloc`) |

User strings in syscalls are `UserInPtr<u8>` + length, copied into a
kernel buffer, not `&str` until UTF-8 is proven.

### Lifetimes

A reference must not outlive its owner. `'a` is a name for that span.
`'static` means "for the rest of the program" (string literals, kernel
`static`s). You rarely write lifetimes in this tree until you store a
reference in a struct.

```rust
fn first_word(s: &str) -> &str { /* elided: fn first_word<'a>(s: &'a str) -> &'a str */ }
```

Lifetime elision covers the common cases (one input borrow in, one
output borrow out, or `self`). When a struct holds a reference, the
struct must name the lifetime:

```rust
struct Parser<'a> { input: &'a str }
```

`UserInPtr<T>` is a pointer, not a Rust `&T`, because user memory can
page-fault and is not trusted. Do not transmute a user VA into `&T`.

## Structs

Book Ch. 5. A struct is a product type: named fields, private by
default.

```rust
#[derive(Default)]
struct Options {
    boot: ProgramInfo,
    bootfs_crc_check: bool,
}
```

([`userspace/secular/bin/userboot/main.rs`](../../userspace/secular/bin/userboot/main.rs))

Three shapes:

| Shape | Example | Use |
| --- | --- | --- |
| Named fields | `struct Options { boot: ProgramInfo, … }` | the usual form |
| Tuple struct | `pub struct FutexId(usize);` | newtype |
| Unit struct | `struct JobInfoQuery;` | marker / trait impl carrier |

Field init shorthand (`Options { boot, .. }`) and struct update
(`Options { bootfs_crc_check: true, ..old }`) work as in the Book.
Update **moves** non-`Copy` fields out of the old value.

`#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash)]` is the
usual set. Public types in this tree implement `Debug`. Do not derive
`Copy` if `Drop` must run.

### Methods and associated functions

```rust
impl Job {
    pub fn create_child_job(&self) -> Result<Job, Status> { /* … */ }
}

impl Lifecycle {
    pub(crate) fn from_raw(raw: u32) -> Option<Self> { /* … */ }
}
```

| Receiver | Meaning |
| --- | --- |
| `self` | consume; caller cannot use the value after |
| `&self` | shared borrow |
| `&mut self` | exclusive borrow |
| none | associated function (`Type::new()`, `Type::from_raw()`) |

Constructors are associated functions, typically `new` / `with_…` /
`create`. Multiple `impl` blocks on one type are allowed (and common
when some methods are `#[cfg(test)]`).

## Enums and pattern matching

Book Ch. 6. An enum is a tagged union. No C-style implicit integer
unless you ask (`#[repr(u32)]`).

```rust
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Lifecycle {
    Initial = 0,
    Initialized = 1,
    Running = 2,
    Suspended = 3,
    Dying = 4,
    Dead = 5,
}
```

([`zircon/kernel/object/thread_state.rs`](../../zircon/kernel/object/thread_state.rs))

Variants may carry data: `Err(Status)`, `Some(T)`, `B(u32)`. `match`
is exhaustive; the compiler rejects a missing arm.

```rust
match topic {
    zx_types::ZX_JOB_POL_BASIC_V1 => job_set_policy_basic_v1(…),
    zx_types::ZX_JOB_POL_BASIC_V2 => job_set_policy_basic_v2(…),
    zx_types::ZX_JOB_POL_TIMER_SLACK => job_set_policy_timer_slack(…),
    _ => Err(Status::INVALID_ARGS),
}
```

`if let` / `let else` / `while let` are `match` with one interesting
arm:

```rust
if let Ok(cmdline) = str::from_utf8(&payload) {
    parse_cmdline(cmdline, …)?;
}

let Some(lifecycle) = Lifecycle::from_raw(raw) else {
    return Err(Status::INVALID_ARGS);
};
```

`Option<T>` and `Result<T, E>` are ordinary enums. There is no null.

```rust
enum Option<T> { None, Some(T) }
enum Result<T, E> { Ok(T), Err(E) }
```

## Modules and crates

Book Ch. 7. The privacy boundary is the **module**, not the type. A
crate is a compilation unit; a package in the Book is a Cargo thing.
This tree has **GN targets** instead of Cargo packages.

```text
 rustc_library("object")          // one crate (rlib)
    source_root = lib.rs / mod.rs
       pub mod channel_dispatcher;
       pub mod futex_context;
       mod futex_context_ffi;     // private
```

```rust
// zircon/kernel/hypervisor/mod.rs
pub mod aspace;
mod aspace_ffi;     // FFI stays crate-private
pub mod trap_map;
```

| Item | Meaning |
| --- | --- |
| `mod foo;` | load `foo.rs` or `foo/mod.rs` |
| `mod foo { … }` | inline module |
| `pub` | visible outside this module |
| `pub(crate)` | visible inside this crate only |
| `pub(super)` | visible to the parent module |
| `use` | import a path; keep them **at file top** |
| `super::` / `crate::` | parent / crate root |

`use` brings names into scope so call sites do not write
`crate::object::handle::HandleValue` everywhere. Do not `use` inside
functions. Re-exports (`pub use`) are how a crate's public API is
shaped.

Paths the compiler searches: `crate::`, `super::`, `self::`, an
external crate name (`zx_status::Status`). Kernel FFI modules
(`*_ffi.rs`) stay private; only the safe wrapper is `pub`.

This is not Cargo. There is no `Cargo.toml` for the image. GN lists
`sources` and `deps`; rustc still follows `mod` declarations, and
`enforce_source_listing` requires those files to be listed.

## Collections

Book Ch. 8. Three everyday collections, plus what the kernel uses
instead.

| Collection | Owns | Kernel spelling | Userspace spelling |
| --- | --- | --- | --- |
| Growable array | heap `T`s | `alloc::vec::Vec` where alloc exists; often intrusive lists | `Vec<T>` |
| UTF-8 text | heap bytes | `&[u8]` / copied buffer until proven UTF-8 | `String` / `&str` |
| Map | keys → values | `fbl::HashTable` | `HashMap<K, V>` / `BTreeMap` |
| Linked list | nodes | `fbl::DoublyLinkedList` (`ChannelDispatcherState::messages`) | `VecDeque` / `LinkedList` (rare) |

```rust
let mut options = Options::default();
opts.boot.next = value.to_string();   // String from &str
```

`String` is a `Vec<u8>` that is always valid UTF-8. `&str` is a slice
of that. Indexing a `String` with `[]` panics if you land mid-code
point; iterate `.chars()` / `.bytes()` or slice at char boundaries.

`HashMap<K, V>` needs `K: Eq + Hash`. Kernel code that needs a map
uses `fbl::HashTable` with `HashTableKeyable`, not `std::collections`.
Do not allocate unbounded user-controlled `Vec`s in a syscall without
a cap (`MAX_POLICY_COUNT` in `job.rs`).

Updating a collection:

```rust
v.push(x);            // Vec
v[i] = x;             // panic if i >= len
map.insert(k, v);     // HashMap
for x in &v { }       // borrow elements
for x in v { }        // move elements out
```

## Error handling

Book Ch. 9. Two paths: **panic** (unrecoverable) and **`Result`**
(recoverable).

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

### Panic

`panic!`, `unwrap`, `expect`, array OOB, integer overflow in debug,
and `assert!` abort the **current thread's process** in userspace, or
the **kernel** if they run in EL1. Do not `unwrap()` on a
user-supplied handle. Checked math (`checked_add`, `checked_mul`) on
untrusted sizes.

`expect("…")` is still a panic; the string is for the crash log, not
an error channel. Kernel code uses `debug::ltracef` / kernel printf,
not `println!`.

### `Result` and `?`

```rust
let (handle, rights) = JobDispatcher::create(options, parent)?;
*out = up.make_and_add_handle(handle, rights)?;
Ok(())
```

`?` returns early on `Err` (or on `None` if the function returns
`Option`). It also runs `From` to convert error types. That is how
userboot writes `fn … -> Result<…, anyhow::Error>` and still `?` a
`zx::Status`.

| Layer | Error type | Rule |
| --- | --- | --- |
| Kernel syscall | `Status` (`zx_status_t`) | never panic on user input |
| `zx` crate | `zx::Status` | `ok(status)?` |
| userboot / tools | `anyhow::Error` | context on the way out |
| Libraries | a real error enum, `std::error::Error` in `std` | do not return `String` |

`peer().ok_or(Status::PEER_CLOSED)?` turns a missing peer into an
error. `map_err`, `ok_or`, `ok_or_else` are the usual adapters.
`unwrap_or` / `unwrap_or_else` / `unwrap_or_default` supply a fallback
without panicking.

## Generics, traits, and lifetimes

Book Ch. 10. Generics are compile-time type parameters. Traits are
the bounds. Lifetimes are generic parameters over regions.

```rust
fn make_and_add_handle<T: Dispatcher>(
    handle: KernelHandle<T>,
    rights: zx_rights_t,
) -> Result<HandleValue, Status>
```

`T: Dispatcher` is a bound. Multiple bounds: `T: Dispatcher + Send`.
`where` clauses are the same thing, written after the signature when
the bound list is long.

### Traits

```rust
impl From<sys::zx_info_job_t> for JobInfo { /* … */ }
```

A trait is an interface: methods a type must provide, plus optional
defaults. Implement `From<A> for B` rather than an ad-hoc `a_to_b()`.
`Into` comes for free from `From`.

Common traits in this tree:

| Trait | Meaning |
| --- | --- |
| `Clone` | explicit duplicate (may allocate) |
| `Copy` | implicit bit-copy; implies `Clone` |
| `Debug` | `{:?}` — required on public types |
| `Display` | `{}` user-facing |
| `Default` | `default()` |
| `PartialEq` / `Eq` | equality |
| `PartialOrd` / `Ord` | ordering |
| `Hash` | for maps / sets |
| `From` / `Into` / `TryFrom` | conversions |
| `AsRef` / `AsMut` | cheap borrow conversion |
| `Iterator` | `next()` |
| `Drop` | destructor; must not panic |
| `Send` / `Sync` | cross-thread (see Concurrency) |
| `Deref` / `DerefMut` | **smart pointers only** |

`impl Trait` in argument position is a generic bound
(`fn f(x: impl AsRef<str>)`). In return position it hides a concrete
type (`fn init(…) -> impl PinInit<Self, Infallible>`). `dyn Trait` is
a trait object (runtime dispatch). Kernel hot paths prefer static
generics.

A trait object is a fat pointer `(data, vtable)`. The trait must be
**object-safe** (no generic methods, no `Self: Sized` returns). Use
`dyn Trait` when the concrete type is not known at compile time; use
generics when it is.

### Lifetimes in structs and bounds

```rust
fn longest<'a>(x: &'a str, y: &'a str) -> &'a str {
    if x.len() > y.len() { x } else { y }
}
```

The signature is a contract: the output lives no longer than **both**
inputs. The compiler proves each call site. `'static` is a subtype of
every lifetime (a literal can be passed where `'a` is wanted; the
reverse is not true).

## Tests

Book Ch. 11. Tests are ordinary functions tagged to run in a test
harness.

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lifecycle_from_raw_rejects_unknown() {
        assert_eq!(Lifecycle::from_raw(99), None);
        assert_eq!(Lifecycle::from_raw(2), Some(Lifecycle::Running));
    }
}
```

| Attribute | Where |
| --- | --- |
| `#[test]` | kernel / host; `#[cfg(test)]` module |
| `#[fuchsia::test]` | Fuchsia userspace; async + logging |
| `#[should_panic]` | asserts that it panics |
| `#[ignore]` | skip unless explicitly requested |

Assertions: `assert!`, `assert_eq!`, `assert_ne!`. Prefer
`assert_matches` for enums. `with_unit_tests = true` on the GN
`rustc_library` / `rustc_binary` generates the test target. Tests may
enable `std` via `cfg(test)` even in a `no_std` crate.

Do not `unwrap()` in production paths "because the test did". Tests
may panic; syscalls may not.

## Closures and iterators

Book Ch. 13. A closure is an anonymous function that may capture its
environment.

```rust
let job = ProcessDispatcher::with_current(|up| {
    up.handle_table().get_dispatcher_with_rights::<JobDispatcher>(
        up, handle, ZX_RIGHT_SET_POLICY,
    )
})?;
```

| Trait | Capture | Callable |
| --- | --- | --- |
| `FnOnce` | moves captured values; call once | every closure |
| `FnMut` | `&mut` captures; call many times | if it does not move out |
| `Fn` | `&` captures; call many times, even shared | if it only reads |

The compiler picks the most restrictive trait the body allows. Move
captures with `move |…|`. Closures that do not capture are coercible
to `fn` pointers (`fn(u32) -> u32`).

Iterators are lazy. Adapters (`map`, `filter`, `enumerate`, `take`,
`zip`) build a pipeline; a consumer (`next`, `collect`, `for`,
`sum`, `any`, `find`) drives it.

```rust
for (ix, item) in items[..count].iter().enumerate() { /* … */ }

up.make_and_add_handle_from_ref(thread, rights).map(|h| *out = h)
```

`iter()` borrows, `iter_mut()` borrows mutably, `into_iter()`
consumes. Implement `Iterator` (just `next`) and `FromIterator` /
`Extend` if you write a collection. Do not write a `for i in 0..n`
index loop when `.iter()` states the intent.

## Smart pointers

Book Ch. 15. A smart pointer owns extra behaviour (`Drop`,
`Deref`, refcount, pin). `Deref` / `DerefMut` are **only** for smart
pointers, not for inheritance.

| Type | Layer | Role |
| --- | --- | --- |
| `&T` / `&mut T` | both | borrow; compiler-checked |
| `*const T` / `*mut T` | FFI, user copy | raw; only in `unsafe` |
| `Box<T>` | userspace / some kernel | unique heap |
| `UniquePtr<T>` | kernel (`fbl`) | unique heap, C++-shaped |
| `Rc<T>` | userspace, single-thread | shared, not `Send` |
| `Arc<T>` | userspace | atomic shared |
| `fbl::RefPtr<T>` | kernel | intrusive refcount |
| `RefCell<T>` | userspace, single-thread | interior mutability, runtime borrow check |
| `Mutex<T>` / `RwLock<T>` | userspace | interior mutability, blocking |
| `UnsafeCell<T>` | both | the primitive; `KCell` / `RefCell` wrap it |
| `Pin<P>` | kernel objects, async | address must not move (`pin_init!`) |
| `UserInPtr<T>` / `UserOutPtr<T>` | kernel | user VA; copy may fault |

```rust
#[pin_data(PinnedDrop)]
pub struct ChannelDispatcherState {
    #[pin]
    peered: PeeredState<ChannelDispatcher>,
    #[pin]
    messages: DoublyLinkedList<MessagePacketPtr, …>,
    owner: UnsafeCell<zx_koid_t>,
    // …
}
```

([`zircon/kernel/object/channel_dispatcher.rs`](../../zircon/kernel/object/channel_dispatcher.rs))

`Box<T>` puts `T` on the heap and is `Unpin` if `T` is. `Pin<Box<T>>`
(and `pin_init!` in this kernel) is for types that must not move
after construction: intrusive lists, wait queues, self-referential
futures.

Interior mutability: a `&T` that can still mutate through `T`. Safe
forms (`RefCell`, `Mutex`) check the aliasing rule at runtime. Kernel
`UnsafeCell` fields (`ChannelDispatcherState::owner`) document which
lock stands in for the borrow checker; that is `unsafe` territory.

`Rc<RefCell<T>>` is the Book's single-thread shared-mutable pattern.
It is **not** `Send`. Kernel shared objects are `RefPtr` plus a
declared mutex (`#[guarded_by(channel_lock)]`), not `Rc`.

`Drop` runs when the last owner goes away. Destructors must not panic
and must not block without a non-blocking alternative. Handle close
lives in `Drop` for `zx::Job`; message-list cleanup lives in
`PinnedDrop` for `ChannelDispatcherState`.

## Concurrency

Book Ch. 16. The type system extends the single-thread borrow rule
across threads.

### `Send` and `Sync`

| Trait | Meaning |
| --- | --- |
| `Send` | this value may move to another thread |
| `Sync` | `&T` may be used from another thread (`T: Sync` iff `&T: Send`) |

`Rc` / `RefCell` are neither (single-thread). `Arc` / `Mutex` are
both. A type with a raw pointer is `Send`/`Sync` only if you
`unsafe impl` and document why. `UnsafeCell<T>` is `Sync` only when
you wrap it in a lock that restores the aliasing rule.

### Kernel: blocking, not threads-as-tasks

A syscall runs on the current `Thread`. If it must wait, that thread
goes `BLOCKED` on a `WaitQueue`
([`zircon/synchronization.md`](zircon/synchronization.md)). There is
no `std::thread::spawn` in `sys_*`. Locks:

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

### Userspace: threads, channels, mutexes

The Book's `std::thread::spawn`, `mpsc`, and `Mutex<T>` exist in
Fuchsia userspace. Uncontended locks are still user atomics on a
`zx_futex_t` ([`zircon/synchronization.md`](zircon/synchronization.md)
Futex). `sync_mutex_t` is that protocol.

When a mutex is required in Fuchsia userspace, prefer
`fuchsia_sync::Mutex` over `std::sync::Mutex`. `std::sync::Mutex`
poisons on panic; that is rarely the policy you want here.

`Arc<Mutex<T>>` is the shared-mutable pattern across threads. Do not
hold a mutex across `.await` unless the mutex is async-aware.

## Async

The Book treats async as a sibling of concurrency: `async fn` compiles
to a state machine (`Future`). Something must `poll` it.
`fuchsia_async` waits on a Zircon **port** (`zx_object_wait_async` /
`zx_port_wait`, [`zircon/ipc.md`](zircon/ipc.md)). Kernel Rust does
not drive that loop.

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

`.await` yields if the `Future` is `Pending`. The future is often
`!Unpin` (self-referential); the executor holds it in `Pin`. That is
the same pin story as `pin_init!` in the kernel, used for a different
reason.

Rules of thumb:

- Kernel / syscall path: **sync**, block the thread.
- `component_manager` / FIDL servers: **async** on `fuchsia_async`.
- `spawn` vs `spawn_local`: `spawn` needs `Send`; `spawn_local` for
  `Rc`. Do not `.detach()` tasks.
- `async fn run(log: &mut Log)` in userboot is this pattern
  ([`userspace/secular/bin/userboot/main.rs`](../../userspace/secular/bin/userboot/main.rs)).

## Object-oriented features

Book Ch. 17. Rust is not Java. Encapsulation is modules + private
fields. Polymorphism is traits, not inheritance.

- **Encapsulation.** Fields are private by default. Invariants live in
  constructors (`JobDispatcher::create`, `Lifecycle::from_raw`). The
  module, not the `impl` block, is the privacy boundary: a free
  function in the same module can touch private fields.
- **Trait objects.** `dyn Dispatcher` (or a sealed trait) when the
  concrete type varies at runtime. Prefer generics on hot paths.
- **No implementation inheritance.** Share code with generics, default
  trait methods, composition, or macros (`impl_peered_dispatcher_facade_with_state!`).
  Do not implement `Deref` to "inherit" methods.

A type is "object-oriented" here if it owns its state and exposes a
small method set. `zx::Job` is that: the raw handle is private,
`Drop` closes it, methods wrap syscalls.

## Patterns

Book Ch. 18. A pattern shows up in `let`, `match`, `if let`,
`while let`, `for`, and function arguments.

| Pattern | Matches |
| --- | --- |
| `x` | anything; binds `x` |
| `_` | anything; discard |
| `0` / `"a"` / `Lifecycle::Running` | literal / unit variant |
| `Some(x)` / `Err(e)` | enum with payload |
| `JobInfo { return_code, .. }` | struct; `..` ignores the rest |
| `(a, b)` | tuple |
| `[a, b, ..]` | slice / array |
| `ref x` / `ref mut x` | bind by reference |
| `p @ 0..=1` | bind and test (`@`) |
| `p1 \| p2` | or-pattern |
| `x if pred` | match guard |

```rust
match *this.max_message_count.as_mut() {
    0..=1 => CHANNEL_PACKET_DEPTH_1.add(1),
    2..=4 => CHANNEL_PACKET_DEPTH_4.add(1),
    5..=16 => CHANNEL_PACKET_DEPTH_16.add(1),
    17..=64 => CHANNEL_PACKET_DEPTH_64.add(1),
    65..=256 => CHANNEL_PACKET_DEPTH_256.add(1),
    _ => CHANNEL_PACKET_DEPTH_UNBOUNDED.add(1),
}
```

Destructuring in `let` is irrefutable (must always match). `if let` /
`match` take refutable patterns. `let else` is the refutable `let`
with an diverging `else`.

## Advanced features

Book Ch. 19. The hatch, the extra type-system knobs, and macros.

### `unsafe`

The language's safety story stops at `unsafe`. Inside it you may:

- dereference raw pointers
- call `unsafe fn` / `extern "C"`
- access a mutable `static`
- implement an `unsafe trait`
- build a slice from a raw pointer (`slice::from_raw_parts`)

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

Format is exactly `// SAFETY:`. Unsafe operations inside an `unsafe
fn` still need an inner `unsafe { }` block in this project. `unsafe
fn` means the **caller** must uphold a contract; it is not itself an
unsafe context here.

```rust
// SAFETY: `process_raw` is a valid pointer carrying an acquired refcount.
let handle = unsafe {
    KernelHandle::create(|out| cpp_thread_dispatcher_alloc(process_raw, flags, out))
}?;
```

Raw pointers (`*const T`, `*mut T`) may be null, dangling, or
aliased. Creating one is safe; dereferencing it is not. `NonNull<T>`
is a `*mut T` that is never null; still `unsafe` to dereference.

### Advanced traits and types

| Feature | What it is | In this tree |
| --- | --- | --- |
| Associated type | `trait Iterator { type Item; }` | one concrete type per impl |
| Supertrait | `trait Sub: Super` | `T: Sub` implies `Super` |
| Newtype | `struct FutexId(usize);` | koids, rights, ids |
| Type alias | `type HandleValue = …` | shorthand, not a new type |
| Never type `!` | diverges (`panic!`, `loop {}`) | coerces to any type |
| DST | `[T]`, `str`, `dyn Trait` | always behind a pointer |
| Function pointer | `fn(u32) -> u32` | vDSO-shaped callbacks, `extern "C"` |
| `impl Trait` | existential / generic bound | `PinInit`, closures |
| Sealed trait | trait others cannot impl | keep dispatcher traits closed |

Operator overloading is `std::ops::{Add, Index, …}`. Only when the
meaning is unsurprising. `bitflags!` is how sets of rights/options
are spelled, not a pile of `bool`s.

### Macros

Two families:

| Kind | Syntax | In this tree |
| --- | --- | --- |
| Declarative (`macro_rules!`) | pattern → code | `define_kcounter!`, `impl_handle_based!` |
| Procedural — derive | `#[derive(Debug)]` | ordinary |
| Procedural — attribute | `#[syscall]`, `#[guarded]`, `#[pin_data]` | veneers, lock annotations |
| Procedural — function-like | `pin_init!(…)`, `bitflags!` | in-place init |

`#[syscall]` on `sys_*` generates the C-callable veneer
([`zircon/kernel/lib/syscalls/macros`](../../zircon/kernel/lib/syscalls/macros)).
`pin_init!` constructs pinned kernel objects in place. Macro input
should look like the code it produces; generated items must accept
`pub` and doc comments.

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

C++ still owns some types; Rust sees them as `unsafe extern "C" { fn
cpp_thread_dispatcher_alloc(...); }` and `OpaqueStorage`. Bindgen
(`products/build/rust/rustc_bindgen.gni`) turns headers into Rust. Do
not hand-write duplicate layouts.

`#[repr(C)]` / `#[repr(u32)]` make the layout match C++. `Lifecycle`
discriminants are an explicit contract with
`thread_dispatcher_ffi.cc`. Changing them without the C++ side is an
ABI break.

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
Do not use `lazy_static!`; use `std::sync::LazyLock`.

### A first reading path

1. [`sdk/rust/zx/src/job.rs`](../../sdk/rust/zx/src/job.rs) — safe
   wrapper around `zx_job_create`; `Drop` closes the handle.
2. [`zircon/kernel/lib/syscalls/job.rs`](../../zircon/kernel/lib/syscalls/job.rs) —
   `#[syscall] sys_job_create`, `Result<(), Status>`, `?`, `match`.
3. [`zircon/kernel/object/thread_state.rs`](../../zircon/kernel/object/thread_state.rs) —
   `enum Lifecycle`, `from_raw`, `#[repr(u32)]`.
4. [`zircon/kernel/object/channel_dispatcher.rs`](../../zircon/kernel/object/channel_dispatcher.rs) —
   `RefPtr`, `pin_init!`, `UnsafeCell`, `PinnedDrop`.
5. [`zircon/kernel/object/futex_context.rs`](../../zircon/kernel/object/futex_context.rs) —
   `FutexId` newtype, wait/wake, `unsafe` user copy.
6. userboot `main.rs` — `struct Options`, `match`, `anyhow`, `async fn`,
   `zx::Vmo`.

## Where in the tree

| Path | Why |
| --- | --- |
| [`zircon/kernel/lib/syscalls/`](../../zircon/kernel/lib/syscalls/) | `sys_*`, `#[syscall]` |
| [`zircon/kernel/lib/syscalls/macros/`](../../zircon/kernel/lib/syscalls/macros/) | proc-macro for those veneers |
| [`zircon/kernel/object/`](../../zircon/kernel/object/) | dispatchers in Rust (channel, futex, job, …) |
| [`zircon/kernel/object/thread_state.rs`](../../zircon/kernel/object/thread_state.rs) | `Lifecycle` / `Exception` enums |
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
| Lifetime | How long a borrow is valid. `'static` outlives everything. |
| Slice | Fat pointer to contiguous `T` (`&[T]`, `&str`). |
| `Copy` | Assignment duplicates bits (integers); not a move. |
| `Clone` | Explicit duplicate; may allocate. |
| `Drop` | Destructor. Must not panic. Handle close lives here. |
| Shadowing | New `let` binding with the same name; not `mut`. |
| `const` / `static` | Compile-time value / process-lifetime object. |
| `struct` | Product type. Fields private by default. |
| `enum` | Tagged union. `match` is exhaustive. |
| `impl` | Methods and associated functions on a type. |
| Trait | Interface + generics. `dyn Trait` is a vtable. |
| Generic | Compile-time type (or lifetime) parameter. |
| Newtype | Tuple struct wrapping a primitive (`FutexId`). |
| `Result` / `Option` | Error and absence. Prefer `?` over `unwrap`. |
| `?` | Early return on `Err` / `None`, with `From` conversion. |
| `Status` | Zircon error type (`zx_status_t`) in both kernel and `zx`. |
| Panic | Abort this process (or the kernel). Not an error channel. |
| Closure | Anonymous function; `FnOnce` / `FnMut` / `Fn`. |
| Iterator | Lazy `next()`; adapters vs consumers. |
| Smart pointer | Owns a value plus behaviour (`Box`, `Arc`, `RefPtr`, `Pin`). |
| Interior mutability | Mutation through `&T` (`UnsafeCell`, `RefCell`, `Mutex`). |
| `Pin` | Value must not move. Kernel objects and async futures. |
| `no_std` | No Rust standard library. Kernel crates. `core` remains. |
| `unsafe` | Opt out of the checker. Needs `// SAFETY:`. |
| FFI | Calling C/C++ (`extern "C"`) or being called from it. |
| `#[repr(C)]` / `#[repr(u32)]` | Layout contract with C++. |
| `#[syscall]` | Marks `sys_*`; generates the kernel entry from user `svc`. |
| `Send` / `Sync` | Cross-thread move / shared reference. |
| `Future` / `.await` | Userspace async state machine. Kernel syscalls do not use it. |
| `fuchsia_async` | Executor that polls futures using Zircon ports/timers. |
| `RefPtr` | Kernel intrusive shared pointer (`fbl`). |
| `UserInPtr` | Pointer to user memory; copy, do not dereference as `&T`. |
| Crate / module | Compilation unit / privacy boundary. |
| GN `rustc_library` | How this repo compiles Rust. Not Cargo for the image. |
| `zx` crate | Userspace bindings. Not `fuchsia_zircon`. |
| Edition | Language version. New targets: 2024. The Book assumes 1.85+. |

---

Everyday's a Hustle.
