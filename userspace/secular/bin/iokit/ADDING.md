# Adding an IOKit command

IOKit binaries are small Rust programs that land in `/boot/bin` on the Secular
console image. zxsh finds them on `PATH`. Each command is its own
`rustc_binary` with a single source file under `src/`.

Copy an existing tool rather than inventing a new layout:

| Example | When to copy it |
| --- | --- |
| `src/clear.rs`, `src/cat.rs` | Pure std: files, stdout, no kernel |
| `src/bootstat.rs` | vDSO syscalls only (`zx_sys::zx_*`) |
| `src/cpustat.rs` | FIDL + `zx` (`InfoResource`, `ZX_INFO_*`) |
| `src/kdmips.rs` | Host-runnable compute with unit tests for scoring |

Do **not** add a command here if it needs a component manifest, a package, or
drivers. Those belong under `userspace/secular/bin/<name>/` as a
`fuchsia_component`. Do **not** reintroduce graphics, netstack, or host
toolchains.

## Layout

```text
userspace/secular/bin/iokit/
  BUILD.gn          rustc_binary + group("iokit")
  README.md         command list
  ADDING.md         this file
  src/<name>.rs     one crate root per command
```

The binary name is the command name: `src/cpustat.rs` → `/boot/bin/cpustat`.

## Source skeleton

Match the style of `bootstat.rs` / `clear.rs`.

```rust
// Copyright 2026 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! One-line description.

use std::io::{self, Write};
use std::process::ExitCode;

const USAGE: &str = "Usage: example [-f] [file]";

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Options {
    flag: bool,
}

fn parse_args(args: &[String]) -> Result<Options, String> {
    // Treat `--` as end of flags. Unknown `-*` → invalid option.
    // Extra operands → extra operand. Always append `\n{USAGE}` on error.
    Ok(Options::default())
}

fn run(args: &[String]) -> i32 {
    let options = match parse_args(args) {
        Ok(options) => options,
        Err(message) => {
            let _ = writeln!(io::stderr(), "{message}");
            return 1;
        }
    };
    let _ = options;
    0
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if run(&args) == 0 { ExitCode::SUCCESS } else { ExitCode::FAILURE }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_args_defaults() {
        let args: [String; 0] = [];
        assert_eq!(parse_args(&args).unwrap(), Options::default());
    }
}
```

Rules that keep the set consistent:

- English comments and user-facing strings.
- `parse_args` takes `&[String]` **after** `argv[0]` (`std::env::args().skip(1)`).
- Errors go to stderr as `<name>: ...\nUsage: ...`. Return `1`.
- Keep parsing, formatting, and Fuchsia syscalls separable so host unit tests
  can cover parse/format without a board.
- Gate kernel/FIDL I/O with `#[cfg(target_os = "fuchsia")]`. Provide a host
  stub that still compiles tests (`bootstat::take_snapshot`,
  `cpustat::run_samples`).
- Do not pull `argh` / `clap`. Flags are a few characters, parsed by hand.
- `-D unused-crate-dependencies` is on: do not list a GN dep you never `use`.

## BUILD.gn

Add a `rustc_binary` next to the others, then list it on `group("iokit")`.

vDSO-only (like `bootstat`):

```gn
rustc_binary("example") {
  output_name = "example"
  edition = "2024"
  source_root = "src/example.rs"
  sources = [ "src/example.rs" ]
  configs += [ "//products/build/config/rust:bootfs" ]
  if (is_fuchsia) {
    deps = [ "//sdk/rust/zx-sys" ]
  }
  with_unit_tests = true
}
```

FIDL / `zx` (like `cpustat`):

```gn
  if (is_fuchsia) {
    deps = [
      "//sdk/fidl/fuchsia.kernel:fuchsia.kernel_rust",
      "//sdk/rust/zx",
      "//sdk/rust/zx-sys",
      "//userspace/lib/fuchsia-async",
      "//userspace/lib/fuchsia-component",
    ]
  }
```

`configs += [ "//products/build/config/rust:bootfs" ]` is required: these
binaries go in the ZBI. Prefer vDSO over FIDL, and FIDL over new libraries.
`fuchsia-async` + `fuchsia-component` is the size cost of talking to `/svc`.

`with_unit_tests = true` produces `//userspace/secular/bin/iokit:<name>_test`.

## Wire it into the image and tests

Four places. Missing any one means the command either does not boot, or tests
never run.

1. `BUILD.gn` — `rustc_binary` + `group("iokit")` deps.
2. `README.md` — one row in the command table.
3. `products/secular/BUILD.gn` — `zbi_input("bootfs_files")` deps:

   ```gn
   "//userspace/secular/bin/iokit:example",
   ```

   That is what puts the ELF in `/boot/bin`. `group("iokit")` alone is not
   enough.
4. `userspace/secular/bin/BUILD.gn` — `group("tests")` deps:

   ```gn
   "iokit:example_test",
   ```

If the tool needs a protocol in the zxsh namespace (for example
`fuchsia.kernel.InfoResource`), confirm it is already `use`d by
`console-launcher` and offered from `products/secular/meta/console.shard.cml`.
Do not add new capabilities unless launch fails without them.

## Kernel and `/svc` access

| Need | How |
| --- | --- |
| Clock, CPU count, page size, physmem, version | `zx_sys::zx_*` vDSO. No routing. See `bootstat.rs`. |
| `ZX_INFO_CPU_STATS`, `ZX_INFO_KMEM_STATS` | `fuchsia.kernel.InfoResource` → `zx::Resource::{cpu_stats,mem_stats}`. See `cpustat.rs`. |
| `GetCpuLoad` / `GetCpuStats` tables | `fuchsia.kernel.Stats` (not used by current iokit tools; `InfoResource` is enough). |
| Files under `/boot`, `/tmp` | `std::fs`. No extra routing. |

Connect to a protocol with a short executor; do not add `#[fuchsia::main]`
unless the binary is a component:

```rust
fuchsia_async::LocalExecutorBuilder::new().build().run_singlethreaded(async {
    let proxy = fuchsia_component::client::connect_to_protocol::<
        fidl_fuchsia_kernel::InfoResourceMarker,
    >()?;
    proxy.get().await
})
```

zxsh inherits console-launcher's `/svc`. If `connect_to_protocol` fails on
device, the offer is missing — fix CML, do not hard-code handles.

## Build and check

From the Secular source root (`SECULAR_SDK` as in the Makefile):

```sh
# Compile just the new command (arm64 console toolchain)
ninja -C out/secular-boot-arm64 userspace/secular/bin/iokit:example

# Full console image, then serial shell
make build
make run
# in zxsh:
example -h   # or just `example`; unknown flags print Usage
```

Unit tests live on the test graph, not the default product graph:

```sh
ninja -C out/secular-boot-arm64 //userspace/secular/bin/iokit:example_test
```

Host-side `parse_args` / format tests should run without QEMU. Fuchsia-only
paths stay behind `cfg(target_os = "fuchsia")`.

After `make run`, confirm `/boot/bin/example` exists and that stdout matches
the unit-test fixtures.

## Checklist

- [ ] `src/<name>.rs` with `USAGE`, `parse_args`, `run`, `ExitCode`, tests
- [ ] `rustc_binary("<name>")` + `group("iokit")`
- [ ] `configs += bootfs`; Fuchsia deps under `if (is_fuchsia)`
- [ ] `products/secular/BUILD.gn` `bootfs_files`
- [ ] `userspace/secular/bin/BUILD.gn` `iokit:<name>_test`
- [ ] `README.md` row
- [ ] `/svc` protocol already offered to console-launcher, or CML updated
- [ ] `ninja …: <name>` builds; unknown flag prints `Usage`
- [ ] No new `prebuilt/`, no extra product/board, comments in English
