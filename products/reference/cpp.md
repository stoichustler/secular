# C++

Secular is a Zircon-microkernel OS. The privileged half of the kernel
is still largely **C++**: `Thread`, VM, GIC, leftover dispatchers, and
the generated syscall wrappers. Rust is taking objects and `sys_*`
([`rust.md`](rust.md)); the two meet at `extern "C"` FFI. This note is
the map of that C++: the language subset the kernel actually allows,
the Fuchsia style this tree follows, and how the same ideas are
spelled in Rust. It is not a substitute for
[C/C++ on Fuchsia](https://fuchsia.dev/fuchsia-src/development/languages/c-cpp)
or the [Google C++ Style Guide](https://google.github.io/styleguide/cppguide.html).

The product stays:

```text
zircon kernel + linux-arm64-boot-shim + BOOTFS + serial console + zxsh
```

CPU / MMU / IPC / locks: [`zircon/cpu.md`](zircon/cpu.md),
[`zircon/memory.md`](zircon/memory.md), [`zircon/ipc.md`](zircon/ipc.md),
[`zircon/synchronization.md`](zircon/synchronization.md). Boot:
[`zircon/bootstrap.md`](zircon/bootstrap.md). Toolchain: `SECULAR_SDK`
(`../secular-sdk` by default); there is no in-tree `prebuilt/` clang.

How this note maps onto the Fuchsia C++ pages and [`rust.md`](rust.md):

| Fuchsia / this tree | This note | Rust twin |
| --- | --- | --- |
| [C++ in Zircon](https://fuchsia.dev/fuchsia-src/development/languages/c-cpp/cxx) | Language subset (`ktl`, no exceptions) | `no_std` / `core` |
| [C++ style](https://fuchsia.dev/fuchsia-src/development/languages/c-cpp/cpp-style) | Google style + Fuchsia exceptions | rustfmt + rubric |
| [Naming](https://fuchsia.dev/fuchsia-src/development/languages/c-cpp/naming) | `PascalCase` types/methods, `snake_case` files | `CamelCase` types, `snake_case` fns |
| [Comments](https://fuchsia.dev/fuchsia-src/development/languages/c-cpp/comments) | `//` by default, Doxygen `///` | `//` / `///` rustdoc |
| [Library restrictions](https://fuchsia.dev/fuchsia-src/development/languages/c-cpp/library_restrictions) | `ktl` / `fbl` / `fit` / `zx` | `core` / `fbl` / `zx` / `anyhow` |
| [Logging](https://fuchsia.dev/fuchsia-src/development/languages/c-cpp/logging) | `FX_LOGS` / kernel `dprintf` | `tracing` / `ltracef` |
| [Lint](https://fuchsia.dev/fuchsia-src/development/languages/c-cpp/lint) | clang-tidy + `fx format-code` | clippy + rustfmt |
| [Thread-safe async](https://fuchsia.dev/fuchsia-src/development/languages/c-cpp/thread-safe-async) | `async::Loop`, no thread-unsafe posts | `fuchsia_async`, `Send` |
| Templates / classes | Generics, RAII, `RefPtr` | traits, ownership, `Arc` |
| [Fuchsia Rust](https://fuchsia.dev/fuchsia-src/development/languages/rust) | FFI and the C++/Rust split | the rest of [`rust.md`](rust.md) |

Start with **Framework** and **Flow**. Kernel examples are the Zircon
subset (`ktl`, no exceptions). Userspace examples may use libc++.

## C++

### Framework

```text
 QEMU virt
        │
        ▼
 zircon kernel EL1
   C++  Thread, VmAspace, GIC, leftover Dispatcher, wrapper_*
   Rust sys_*, ChannelDispatcher, FutexContext, scheduler.rs
        │  extern "C"  cpp_* / rust_*
        │  vDSO  svc
        ▼
 userspace EL0
   Rust  userboot, component_manager, zx crate
   C++   libzx, FIDL C++ bindings, some drivers  (not the console path)
   C     zxsh / musl
```

| Layer | Standard library | Typical headers | Exceptions / RTTI |
| --- | --- | --- | --- |
| Kernel (`zircon/kernel/**/*.cc`) | **`ktl::`**, never `std::` | `fbl/`, `ktl/`, `kernel/` | **off** |
| Userspace Fuchsia C++ | libc++ (`std::`) with restrictions | `lib/zx`, `lib/fit`, FIDL | on (do not throw across FFI) |
| `zxsh` | musl C | POSIX-ish | n/a |

Build is **GN** (`source_set` / `executable` / `sdk_source_set` in
[`products/build/cpp/`](../build/cpp/)), not CMake. clang comes from
`SECULAR_SDK`. This tree compiles C++ as **C++23**
(`fuchsia_cxx_version` in
[`products/build/config/fuchsia_cxx_version.gni`](../build/config/fuchsia_cxx_version.gni)).
The Fuchsia Zircon page still documents C++17 as the historical
kernel floor; do not take that as the current `-std=` here.

C++'s bet in this kernel is **RAII and templates without a runtime**:
destructors close handles and drop refcounts, `Guard<Mutex>` owns a
lock, `fbl::RefPtr` is an intrusive `Arc`. There is no borrow checker.
Lifetime and aliasing are comments, thread annotations, and review.

```text
 compile-time                         runtime
 T x;               ctor / dtor       RAII (close, unlock, unref)
 T* / T&            you promised      dangling is UB
 template <class T> monomorphize      static call
 virtual            vtable            indirect call
 zx_status_t        convention        early return (no exceptions)
 extern "C"         ABI               Rust / assembly
```

### Flow

A syscall that is still C++ on one side of the FFI:

```text
 EL0  zx_job_create  (vDSO / libzx)
        svc
 EL1  wrapper_job_create              // generated C++
        sys_job_create                // Rust today; some sys_* still C++
          get_dispatcher_with_rights
          JobDispatcher::Create       // C++ static, or Rust JobDispatcher::create
          make_and_add_handle
        eret
 EL0  zx::Job owns the handle; ~Job() zx_handle_close
```

([`zircon/kernel/object/include/object/job_dispatcher.h`](../../zircon/kernel/object/include/object/job_dispatcher.h),
[`sdk/rust/zx/src/job.rs`](../../sdk/rust/zx/src/job.rs))

## Language subset

[C++ in Zircon](https://fuchsia.dev/fuchsia-src/development/languages/c-cpp/cxx)
is the kernel contract. Userspace is looser; the kernel is not.

### Kernel: allowed

- C++23 (this tree) with Clang
- Templates, `constexpr`, `static_assert`, type traits
- RAII, `ktl::unique_ptr`, `fbl::RefPtr`
- `ktl::optional`, `ktl::span`, `ktl::string_view`, `ktl::array`,
  `ktl::variant`, `ktl::atomic`
- Lambdas that do not capture heap-unfriendly state in hot paths
- `enum class`
- Thread annotations (`TA_GUARDED`, `TA_ACQ_EXCL`, …)

### Kernel: forbidden

| Feature | Why | What to use |
| --- | --- | --- |
| Exceptions | no runtime, no unwind through IRQ/syscall | `zx_status_t` / `zx::result` |
| RTTI (`dynamic_cast`, `typeid`) | no vtable typeinfo | `enum` / `zx_obj_type_t` |
| `std::` | kernel must not pull libc++ | `ktl::` ([`zircon/kernel/lib/ktl/README.md`](../../zircon/kernel/lib/ktl/README.md)) |
| Throwing `new` | OOM must be handled | `new (ac) T` + `fbl::AllocChecker` |
| iostreams / `<string>` | heap + exceptions | `ktl::string_view`, `printf` / `dprintf` |
| `thread_local` as C++ ABI | kernel TLS is `Thread*` / `TPIDR_EL1` | `Thread::Current` |
| Virtual inheritance | layout / cost | composition |
| Compiler GNU extensions that clang-tidy rejects | portability | write it in standard C++ |

`.cc` files that include `<ktl/*.h>` end the include list with
`<ktl/enforce.h>`. After that, `std::` is a compile error. Headers
do **not** include `enforce.h`.

### Userspace

libc++ is allowed. Still avoid exceptions in FIDL / driver paths that
cross `extern "C"`. Do not use the deprecated C libraries listed in
the Fuchsia C++ docs (old `fdio` helpers, old `fbl` string APIs that
have `std::string` replacements in userspace). Prefer `zx::` wrappers
over raw `zx_handle_t` in C++ userspace, matching how Rust uses the
`zx` crate rather than `fuchsia_zircon`.

## Style

Google C++ Style Guide, plus Fuchsia exceptions
([C++ style](https://fuchsia.dev/fuchsia-src/development/languages/c-cpp/cpp-style)):

- **Never omit parameter names** in a declaration. `void Foo(int);` is
  rejected; `void Foo(int length);` states the contract.
- **Braced initialization** for integer types: `uint32_t n{0};` not
  `uint32_t n = 0;` when writing new code. Existing kernel files mix
  both; match the file.
- **Trailing return types** (`auto Foo() -> int`) only for lambdas, or
  when the return type depends on a template parameter in a way the
  prefix form cannot spell.
- **C headers** keep `#ifndef PATH_TO_FILE_H_` include guards, not
  `#pragma once`, in Zircon. Example:
  `ZIRCON_KERNEL_OBJECT_INCLUDE_OBJECT_JOB_DISPATCHER_H_`.
- **Format** with the tree's clang-format. Do not invent a second
  style. `fx format-code` only on files that already follow Fuchsia
  formatting, from the repository that owns them.

Line length, two-space indent, pointer `T*` with the star on the type,
and `const T*` for read-only pointers are Google defaults. Match the
surrounding file when it disagrees.

## Naming

[Naming](https://fuchsia.dev/fuchsia-src/development/languages/c-cpp/naming)
is Google C++ naming. Contrast [`rust.md`](rust.md): Rust methods are
`snake_case` and getters drop `get_`; C++ methods are `PascalCase` and
getters are often `name()` without `Get` as well.

| Kind | C++ (this tree) | Rust twin |
| --- | --- | --- |
| Type | `JobDispatcher`, `AffinityType` | `JobDispatcher`, `Lifecycle` |
| Method / function | `Create`, `Kill`, `OnJob` | `create`, `kill` |
| Data member | `parent_`, `policy_` | `parent`, `policy` |
| Local / parameter | `parent_job`, `count` | `parent_job`, `count` |
| Constant | `kPolicyBasicInlineCount` | `MAX_POLICY_COUNT` / `SCREAMING` |
| Enum class enumerator | `AffinityType::Hard` | `Lifecycle::Running` |
| Macro | `ZX_JOB_NO_CHILDREN`, `DISALLOW_COPY_ASSIGN_AND_MOVE` | `define_kcounter!` |
| Namespace | `internal`, `ktl`, `fbl`, `zx` | `mod` / crate |
| File | `job_dispatcher.cc`, `job_dispatcher.h` | `job.rs` |
| Include guard | `ZIRCON_KERNEL_…_JOB_DISPATCHER_H_` | n/a |
| FFI symbol | `cpp_dispatcher_get_koid`, `rust_handle_make` | `extern "C"` |

Getters: `job->get_koid()` exists in older C++; prefer `koid()` /
`rights()` on new APIs when the surrounding type already does. Rust
never uses `get_`. Conversions in both languages: `as_` cheap, `to_`
allocating, `into_` consuming — in C++ that is `as_*` / `To*` /
`std::move` into a converting constructor.

Do not abbreviate names (`cnt`, `ptr` as a type name, `proc` is
already a local convention for `ProcessDispatcher*` — do not invent
new ones).

## Headers and includes

Order (clang-format will sort within groups):

```text
1. corresponding ".h" for this ".cc"
2. C system  <stdint.h>
3. C++ / ktl <ktl/unique_ptr.h>
4. fbl / fit / zx
5. project   <object/dispatcher.h>
6. <ktl/enforce.h>     // kernel .cc only, last
```

Related headers: [Include what you use](https://fuchsia.dev/fuchsia-src/development/languages/c-cpp/include_what_you_use)
and [restrict includes](https://fuchsia.dev/fuchsia-src/development/languages/c-cpp/restrict_includes).
Do not include a header just because a nested header happens to
provide a name today.

Forward-declare `class Dispatcher;` when you only hold a pointer.
Include the header when you need the complete type (size, members,
inheritance).

Kernel public C API stays `extern "C"` in `zircon/system/public/`
(`zx_status_t`, `zx_handle_t`). C++ types do not leak into the vDSO.

## Comments

[Comments](https://fuchsia.dev/fuchsia-src/development/languages/c-cpp/comments):
`//` is the default. `/* */` for the copyright block and for
commenting out. Public APIs use Doxygen `///` (or `//!` on the
enclosing file, rare).

```cpp
// Walks child jobs. Returning false from OnJob stops the walk.
class JobEnumerator {
 public:
  virtual bool OnJob(JobDispatcher* job) { return true; }

 protected:
  virtual ~JobEnumerator() = default;
};
```

Document:

- Error returns (`ZX_ERR_*`)
- Lock requirements (`TA_REQ(mutex_)`)
- Lifetime of pointers you hand out
- Why an `extern "C"` trampoline is sound (pair with Rust `// SAFETY:`)

Comments are for maintainers, not for the agent that wrote the patch.
No `TODO` without a bug (`TODO(https://fxbug.dev/…)`).

## Types, RAII, pointers

C++ does not check aliasing. The discipline is the same as Rust's
ownership, written by hand.

| C++ | Meaning | Rust twin |
| --- | --- | --- |
| `T x;` | owned automatic; dtor at `}` | `let x: T` |
| `T*` | nullable, non-owning | `*mut T` / `Option<&mut T>` (you promised) |
| `T&` | non-null, non-owning | `&mut T` or `&T` (you promised) |
| `const T*` / `const T&` | read-only | `&T` |
| `ktl::unique_ptr<T>` | unique heap | `Box<T>` / `UniquePtr<T>` |
| `HandleOwner` | `unique_ptr<Handle, HandleDestroyer>` | `KernelHandle<T>` until inserted |
| `fbl::RefPtr<T>` | intrusive refcount | `Arc<T>` / kernel `RefPtr<T>` |
| `ktl::optional<T>` | presence | `Option<T>` |
| `ktl::span<T>` | `(ptr, len)` | `&[T]` / `&mut [T]` |
| `ktl::string_view` | non-owning text | `&str` (UTF-8 not implied) |
| `zx::Job` (userspace) | handle; dtor closes | `zx::Job` |

```cpp
using HandleOwner = ktl::unique_ptr<Handle, HandleDestroyer>;
```

([`zircon/kernel/object/include/object/handle.h`](../../zircon/kernel/object/include/object/handle.h))

Rules:

- A raw `T*` in a signature does not own. The callee must not `delete`
  it unless the name says otherwise (`release()`, `leak()`).
- `RefPtr<T>` is the kernel shared pointer. `T` inherits
  `fbl::RefCounted<T>` (or the upgradeable variant). Intrusive: no
  separate control block.
- `DISALLOW_COPY_ASSIGN_AND_MOVE(Type)` when the type is pinned to a
  list node or a lock. That is "this type is not `Copy` and not
  movable".
- Destructors **must not throw** and should not block without a
  non-blocking alternative (same as Rust `Drop`).
- `new (std::nothrow)` is not the kernel pattern. Use
  `fbl::AllocChecker`:

```cpp
fbl::AllocChecker ac;
auto* obj = new (&ac) MyObject();
if (!ac.check()) {
  return ZX_ERR_NO_MEMORY;  // obj is nullptr
}
```

Forgetting `check()` panics in debug when the checker is destroyed
still armed.

User memory is never a C++ reference. Copy through the user-copy
helpers (`UserInPtr` on the Rust side, `make_user_in_ptr` /
`user_in_ptr<T>` on the C++ side,
[`lib/user_copy/user_ptr.h`](../../zircon/kernel/lib/user_copy/include/lib/user_copy/user_ptr.h)).
A dangling `T&` is UB; a failed user copy is `ZX_ERR_INVALID_ARGS` /
`ZX_ERR_FAULT`.

## Error handling

No exceptions in the kernel. Recoverable failure is a value.

```cpp
zx_status_t JobDispatcher::Create(/* … */, fbl::RefPtr<JobDispatcher>* out);
zx::result<power_cpu_state> platform_get_cpu_state(cpu_num_t cpu_id);
```

| Layer | Type | Rule |
| --- | --- | --- |
| Kernel C++ | `zx_status_t` | `ZX_OK` or negative `ZX_ERR_*` |
| Kernel C++ (newer) | `zx::result<T>` | `fit::result<zx_status_t, T>`; use `.is_error()` / `value()` |
| Kernel Rust | `Result<T, Status>` | `?` |
| Userspace C++ | `zx::result<T>` / `zx_status_t` | do not throw across FFI |
| Userspace Rust | `zx::Status`, `anyhow::Error` | `?` |

```cpp
zx_status_t status = JobDispatcher::Create(options, parent, &job);
if (status != ZX_OK) {
  return status;
}
```

The Rust twin is `JobDispatcher::create(...)?`. Out-parameters (`T*
out`) are the old C++ shape; `zx::result<T>` is the new one. Do not
mix `ZX_OK` with `true` as a success code.

`ZX_DEBUG_ASSERT` / `ZX_ASSERT` abort the kernel. They are not an
error channel. Do not assert on user-supplied handles.

## Templates

C++ templates are compile-time generics. They are how
`SoloDispatcher<JobDispatcher, Rights, …>`, `Guard<Mutex>`,
`RefPtr<T>`, and `ktl::unique_ptr<T>` exist. The Rust twin is
`fn f<T: Dispatcher>(…)` and `impl<T>`.

### Function and class templates

```cpp
template <typename T>
class KernelHandle;

template <typename T>
zx_status_t get_dispatcher_with_rights(zx_handle_t handle, zx_rights_t rights,
                                       fbl::RefPtr<T>* out);
```

`typename` and `class` in a template parameter list mean the same
thing; this tree uses `typename` for types. Non-type parameters are
ordinary:

```cpp
class JobDispatcher final
    : public SoloDispatcher<JobDispatcher, ZX_DEFAULT_JOB_RIGHTS, 0u,
                            lockdep::LockFlagsNestable>,
      public fbl::ContainableBaseClasses<
          fbl::TaggedDoublyLinkedListable<JobDispatcher*, internal::JobDispatcherRawListTag>,
          fbl::TaggedSinglyLinkedListable<fbl::RefPtr<JobDispatcher>,
                                          internal::JobDispatcherListTag>> {};
```

That is CRTP (`SoloDispatcher<JobDispatcher, …>`): the base knows the
derived type at compile time. Rust would be a trait bound plus a
macro (`impl_peered_dispatcher_facade_with_state!`). Prefer CRTP or
templates on hot paths over `virtual` (same advice as `dyn Trait`).

### Specialization and `if constexpr`

Full specialization and partial specialization exist. Use them for
traits (`ktl::is_same_v<T, U>`), not for copying a class twice.
`if constexpr` (C++17+) drops the unused branch at compile time:

```cpp
if constexpr (ktl::is_same_v<Lock, SpinLock>) {
  // IRQ-off path; this branch does not exist in the Mutex instantiation
}
```

### Template hygiene

- Put heavy templates in headers; the compiler needs the body at the
  instantiation site. Keep non-template methods in `.cc`.
- Do not instantiate a kernel template on a userspace-only type.
- Error messages are long. `static_assert(has_bar_v<T>, "T must implement Bar()");`
  (`DECLARE_HAS_MEMBER_FN` in `fbl/macros.h`) is how this tree makes
  them readable.
- Concepts (`std::integral`, …) exist in C++20/23. Kernel code still
  spells many bounds as `static_assert` + traits; new code may use
  `ktl` concepts where the header already does.

### What templates are not

They are not a substitute for `virtual` when the type varies at
runtime (`JobEnumerator::OnJob`). They are not RTTI. They must not
allocate unless the instantiation is obviously heap-safe
(`AllocChecker` / `RefPtr`).

## Classes

```cpp
class JobDispatcher final
    : public SoloDispatcher<JobDispatcher, ZX_DEFAULT_JOB_RIGHTS, …> {
 public:
  static zx_status_t Create(/* … */);

 private:
  // members; Trailing underscore.
};
```

| Feature | Use |
| --- | --- |
| `final` | type is not a further base (Job, most dispatchers) |
| `override` / `final` on methods | required on overrides |
| `virtual` destructor | only if deleted through a base (`JobEnumerator`) |
| `public` / `protected` / `private` | Google order: public API, then protected, then private |
| composition | default; inheritance is "is-a Dispatcher" |
| `enum class AffinityType : bool` | scoped enum; no implicit int |

`JobEnumerator` is the classic interface: a visitor with defaulted
virtuals, protected destructor so you cannot delete through the base
by accident.

Do not overload `operator*` / `operator->` except on smart pointers
(`RefPtr`, `unique_ptr`). That matches Rust: `Deref` is for smart
pointers only.

## Libraries

[Library restrictions](https://fuchsia.dev/fuchsia-src/development/languages/c-cpp/library_restrictions).
In this tree:

| Library | Layer | Role | Rust twin |
| --- | --- | --- | --- |
| `ktl` | kernel | curated libc++ API in namespace `ktl` | `core` |
| `fbl` | kernel + some user | `RefPtr`, intrusive lists, `AllocChecker`, `Canary` | `fbl` crate / `alloc` |
| `fit` | both | `fit::result`, `fit::function`, `fit::defer` | `Result`, closures |
| `zx` / libzx | userspace | `zx::Job`, `zx::Channel`, RAII handles | `zx` crate |
| `zx::result` | both | `fit::result<zx_status_t, T>` | `Result<T, Status>` |
| `lockdep` + `Guard` | kernel | annotated RAII locks | `KMutex` + `#[guarded_by]` |
| FIDL C++ | userspace | wire / natural bindings | `fidl_next` |
| `async` | userspace | `async::Loop`, waits on ports | `fuchsia_async` |

### `ktl`

[`zircon/kernel/lib/ktl/README.md`](../../zircon/kernel/lib/ktl/README.md):
`<foo>` → `<ktl/foo.h>`, `std::foo` → `ktl::foo`. Not every libc++
header is mirrored. `<memory>` is split (`<ktl/unique_ptr.h>`, not
`<ktl/memory.h>`). There is no `ktl::vector` in the kernel; use
`fbl::Vector` / `fbl::Array` / intrusive lists with a cap.

### `fbl` containers

Intrusive. The node lives **in** the object (`DoublyLinkedListable`,
WAVL node). No extra allocation per insert. Kernel objects sit on
job lists, handle tables, and wait queues this way.

| Container | When |
| --- | --- |
| `DoublyLinkedList` | queues (channel messages, waiters) |
| `SinglyLinkedList` | simple stacks |
| `HashTable` | koid / futex maps |
| `WAVLTree` | ordered maps (VMAR tree) |
| `fbl::Vector` / `Array` | contiguous, bounded or checker-allocated |
| `fbl::Canary<"JOBD">` | use-after-free detection |

Do not use `std::list` / `std::unordered_map` in the kernel. Do not
put a `RefPtr` in a container that also owns via the intrusive node
without documenting which pointer is the owner.

## Concurrency

Kernel: blocking, not C++ `std::thread`. A syscall runs on the
current `Thread`. Locks are RAII.

| Primitive | C++ spelling | When |
| --- | --- | --- |
| Spinlock | `Guard<SpinLock, IrqSave>` | IRQ off, never block |
| Mutex | `Guard<Mutex>` / `CriticalMutex` | may block |
| Thread annotation | `TA_GUARDED(mutex_)` on the member | clang static check |
| Atomic | `ktl::atomic<T>` / `RelaxedAtomic` | no lock |
| Wait | `WaitQueue` / `Event` | sleep the `Thread` |

```text
 Guard<Mutex> guard{&mutex_};   // lock
 // … members TA_GUARDED(mutex_)
 // unlock at }  even on early return
```

That is Rust `let _g = mutex.lock();`. There is no borrow checker:
`TA_GUARDED` is how Clang notices a missing guard. `lockdep` catches
lock-order cycles at runtime.

Userspace async
([thread-safe async](https://fuchsia.dev/fuchsia-src/development/languages/c-cpp/thread-safe-async)):
an `async_dispatcher_t` (usually `async::Loop`) is **not**
thread-safe by default. Posting from another thread requires
`async::Loop::Loop(kAsyncLoopConfigAttachToCurrentThread)` plus an
explicit thread-safe dispatcher, or `async::PostTask` on a loop that
documents it. Do not share an `async::Wait` across threads. The Rust
twin is `spawn` (needs `Send`) vs `spawn_local`.

FIDL C++ clients/servers on this image are rare on the console path
(`component_manager` is Rust). If you write one, run it on a single
async dispatcher; do not block that dispatcher on a Zircon syscall
that can wait forever without an executor-aware wait.

## Logging

[Logging](https://fuchsia.dev/fuchsia-src/development/languages/c-cpp/logging):

| Layer | API | Notes |
| --- | --- | --- |
| Userspace C++ | `FX_LOGS(INFO) << …` / `FX_LOGST` | syslog; no `printf` to stdout as the log |
| Kernel | `dprintf`, `TRACE`, `kprintf` | `LOCAL_TRACE` gates |
| Kernel Rust | `debug::ltracef` | same debuglog |

Do not log user secrets. Do not `printf` in a driver as the primary
log. Severity: `ERROR` / `WARNING` / `INFO` / `DEBUG` / `TRACE`.

## Lint, tests, UB

[Lint](https://fuchsia.dev/fuchsia-src/development/languages/c-cpp/lint):
clang-tidy configs live with the GN targets. New code should not
add `// NOLINT` without a reason on the same line.

Tests:

| Harness | Where |
| --- | --- |
| Kernel `UNITTEST` | `zircon/kernel/**/*_tests.cc` |
| `zxtest` | Zircon userspace |
| `gtest` | some userspace; match the target |
| Rust `#[test]` / `#[fuchsia::test]` | [`rust.md`](rust.md) |

Undefined behavior the Fuchsia C++ rubric hunts first:

- Signed overflow
- Null dereference
- Out-of-bounds
- Use-after-free (a `T&` or `T*` that outlives the `RefPtr`)
- Data races (a `TA_GUARDED` member read without the lock)
- Uninitialized reads

`ktl::atomic` with `memory_order_relaxed` needs a comment about what
it pairs with. `memory_order_seq_cst` is the default and is often
stronger than the lock already provides — do not sprinkle it.

## FFI with Rust

Two directions, one ABI (AAPCS64, `extern "C"`). [`rust.md`](rust.md)
FFI is the other half.

```text
 exceptions.S  svc
      │
      ▼
 wrapper_job_create        generated C++
      │
      ▼
 sys_job_create            Rust #[syscall]  or leftover C++ sys_*
      │
      ├─ JobDispatcher::Create     C++ or Rust
      └─ cpp_thread_*              Rust calls C++
         rust_handle_make          C++ calls Rust
```

```cpp
extern "C" {
zx_koid_t cpp_dispatcher_get_koid(const Dispatcher* disp);
Handle* rust_handle_make(Dispatcher* dispatcher, zx_rights_t rights);
}
```

Rules:

- `extern "C"` functions take POD / opaque pointers, not `RefPtr` by
  value (the Rust side cannot run a C++ destructor).
- A `Dispatcher*` passed across FFI is either borrowed (caller keeps
  the ref) or transferred (documented on both sides).
- `#[repr(C)]` / `#[repr(u32)]` on the Rust enum must match the C++
  `enum` / `uint32_t` (see `Lifecycle` in [`rust.md`](rust.md)).
- Bindgen generates headers → Rust. Do not hand-write a second layout.
- Every unsafe use on the Rust side needs `// SAFETY:`. The C++ side
  documents the same invariant next to the trampoline.

## C++ vs Rust

[Fuchsia Rust](https://fuchsia.dev/fuchsia-src/development/languages/rust)
and [`rust.md`](rust.md) are the other language note. Same objects,
different spelling.

| Topic | C++ | Rust |
| --- | --- | --- |
| Ownership | RAII dtor; you track aliases | move / `Drop`; compiler tracks |
| Shared | `fbl::RefPtr<T>` | `fbl::RefPtr<T>` / `Arc<T>` |
| Unique heap | `ktl::unique_ptr<T>` | `Box<T>` |
| Absence | `T*` / `ktl::optional<T>` | `Option<T>` (no null) |
| Errors | `zx_status_t`, `zx::result<T>` | `Result<T, Status>` + `?` |
| Generics | `template <typename T>` | `fn f<T: Trait>` |
| Interface | `virtual` / CRTP | `trait` / `dyn Trait` |
| Exhaustiveness | `switch` + `-Wswitch` | `match` (hard error) |
| Lock | `Guard<Mutex>` + `TA_GUARDED` | `KMutex` + `#[guarded_by]` |
| Async | `async::Loop` (userspace) | `fuchsia_async` (userspace) |
| Kernel lib | `ktl`, no `std` | `core`, `#![no_std]` |
| Syscalls | `wrapper_*` C++, some `sys_*` | `#[syscall] sys_*` |
| Tests | `UNITTEST` / zxtest / gtest | `#[test]` / `#[fuchsia::test]` |
| Format | clang-format | rustfmt |
| Edition / std | C++23 here | edition 2024 |

When adding a kernel object: prefer **Rust** for new `sys_*` and new
dispatchers if the neighbours are already Rust (`channel`, `futex`,
`job` syscalls). Prefer **C++** when extending an existing C++ type
(`Thread`, `VmAspace`) rather than translating it mid-patch. Do not
leave a second implementation of the same object.

## How to work in this tree

### Build

```sh
export SECULAR_SDK=/absolute/path/to/secular-sdk   # default: ../secular-sdk
make build    # GN + ninja, including kernel C++
```

Add C++ with `source_set` / `executable` in `BUILD.gn`. List
`sources` and `deps`. Do not add a CMake/Cargo tree. Do not vendor
clang.

### Kernel `.cc`

```cpp
#include "job_dispatcher.h"

#include <ktl/algorithm.h>
#include <object/process_dispatcher.h>

#include <ktl/enforce.h>  // last
```

No `std::`. No exceptions. `AllocChecker` on `new`. `Guard<>` on
locks. `TA_GUARDED` on the data.

### Userspace `.cc`

`zx::Channel`, `zx::Vmo`, FIDL C++ bindings. Log with `FX_LOGS`.
Tests: `zxtest` or `gtest` as the target already does. Do not block
an async dispatcher.

### A first reading path

1. [`zircon/kernel/object/include/object/handle.h`](../../zircon/kernel/object/include/object/handle.h) —
   `HandleOwner`, `KernelHandle<T>`, FFI to Rust handles.
2. [`zircon/kernel/object/include/object/job_dispatcher.h`](../../zircon/kernel/object/include/object/job_dispatcher.h) —
   CRTP dispatcher, `RefPtr`, enumerator interface.
3. [`zircon/kernel/include/kernel/thread.h`](../../zircon/kernel/include/kernel/thread.h) —
   kernel `Thread`, `ktl::unique_ptr`, `fit::function`.
4. [`zircon/kernel/lib/ktl/README.md`](../../zircon/kernel/lib/ktl/README.md) —
   why `ktl` exists.
5. [`zircon/kernel/object/include/object/dispatcher.h`](../../zircon/kernel/object/include/object/dispatcher.h) —
   `extern "C" cpp_dispatcher_*`.
6. [`rust.md`](rust.md) — the same objects on the Rust side.

## Where in the tree

| Path | Why |
| --- | --- |
| [`zircon/kernel/include/kernel/thread.h`](../../zircon/kernel/include/kernel/thread.h) | scheduler entity in C++ |
| [`zircon/kernel/object/include/object/`](../../zircon/kernel/object/include/object/) | dispatcher headers (C++ and FFI) |
| [`zircon/kernel/lib/ktl/`](../../zircon/kernel/lib/ktl/) | kernel C++ standard subset |
| [`zircon/system/ulib/fbl/`](../../zircon/system/ulib/fbl/) | `RefPtr`, containers, `AllocChecker` |
| [`zircon/system/public/zircon/`](../../zircon/system/public/zircon/) | C ABI: status, rights, types |
| [`sdk/lib/zx/`](../../sdk/lib/zx/) | userspace C++ handle wrappers |
| [`sdk/lib/fit/`](../../sdk/lib/fit/) | `fit::result`, `fit::function` |
| [`sdk/lib/async/`](../../sdk/lib/async/) | userspace async loop |
| [`products/build/cpp/`](../build/cpp/) | GN C++ templates |
| [`tools/ai-coding/skills/fuchsia-cpp-guidelines/`](../../tools/ai-coding/skills/fuchsia-cpp-guidelines/) | review rubric used in this tree |
| [`rust.md`](rust.md) | Rust half of the same kernel |

## Terminology

| Term | Meaning in this note |
| --- | --- |
| RAII | Constructor acquires, destructor releases. Locks, handles, refcounts. |
| `ktl` | Kernel Template Library. `std::` API under `ktl::`. |
| `fbl` | Fuchsia Base Library. `RefPtr`, intrusive containers, `AllocChecker`. |
| `RefPtr<T>` | Intrusive shared pointer. Kernel objects. |
| `HandleOwner` | Unique `Handle`; dtor frees the table slot. |
| `zx_status_t` | Integer error; `ZX_OK` or `ZX_ERR_*`. |
| `zx::result<T>` | `fit::result<zx_status_t, T>`. |
| CRTP | `Base<Derived>`. Static polymorphism; `SoloDispatcher<JobDispatcher, …>`. |
| `virtual` | Runtime polymorphism. Use for enumerators, not hot paths. |
| `TA_GUARDED` | Clang thread annotation: this member is under that lock. |
| `Guard<Lock>` | RAII lock. Unlock on every exit path. |
| `AllocChecker` | Fallible `new`. Must `check()`. |
| `DISALLOW_COPY_ASSIGN_AND_MOVE` | Delete copy and move. Type is pinned. |
| Include guard | `#ifndef PATH_TO_FILE_H_`. Not `#pragma once` in Zircon. |
| `extern "C"` | C ABI. The Rust/C++ boundary. |
| `cpp_*` / `rust_*` | FFI trampolines, C++-owned vs Rust-owned. |
| UB | Undefined behavior. Signed overflow, dangling `T&`, data races. |
| clang-format / clang-tidy | Format and lint. Fuchsia configs. |
| C++23 | `-std=` for this tree (`fuchsia_cxx_version`). |
| Google C++ style | Baseline naming and formatting; Fuchsia exceptions on top. |

---

Everyday's a Hustle.
