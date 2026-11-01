# Project: Secular

Secular is a Zircon-microkernel operating system. The product is the
smallest bootable system that still provides an interactive serial
console:

```text
zircon kernel + phys/boot-shim + BOOTFS + console + basic framework
```

Do not grow this tree toward a full Fuchsia product. Do not copy the
compile toolchain into this repository.

## Goals

1. **Zircon microkernel.** Privileged code lives under `zircon/`. Keep
   the kernel object/syscall ABI. User policy belongs in user space.
2. **Minimal runtime.** The supported image is:
   - Zircon kernel (`vmzircon`)
   - architecture boot-shim / physboot for QEMU
   - BOOTFS (userboot, vDSO, console, and the basic framework)
   - serial console (`console`, `console-launcher`, `zxsh`)
   - basic framework (`component_manager`, driver framework, QEMU
     UART/serial and board drivers required by the console)
3. **External toolchain.** Host tools come from `../secular-sdk`
   (override with `SECULAR_SDK`). The Makefile injects that path through
   environment variables and GN args. Never add `prebuilt/` to this
   source tree, and never vendor clang/gn/ninja/rustc/qemu here.
4. **Third-party sources.** Empty `third_party/*/src` directories are
   Fuchsia git submodules. Fill them from the remotes and revisions in
   `products/manifests/third_party/all` (see `tools/secular-boot/fetch-third-party.sh`).
   Do **not** copy third_party trees from smos or any other sibling
   checkout: those BUILD.gn files import `//release/...` and the sources
   are a different snapshot.
5. **Small tree.** Remove unused products, boards, drivers, docs, and
   host tools that are not required by the image above. Prefer deleting
   code over stubbing it. Do not reintroduce graphics, UI, netstack,
   WLAN, Bluetooth, camera, audio, Starnix, update, or recovery.
6. **QEMU console.** `qemu-system-aarch64` must boot the image, print
   kernel and userboot logs on serial, and present a usable console
   (interactive `zxsh` / dash prompt).

## Architecture

```text
QEMU (qemu-system-aarch64)
  │
  ▼
linux-arm64-boot-shim  →  secular.zbi
  │                         ├─ Zircon kernel
  │                         └─ BOOTFS
  ▼
userboot → component_manager → driver_manager / console-launcher
                                 └─ serial console + zxsh
```

Supported target: **arm64 QEMU virt**. Do not add other boards unless
the requester explicitly expands scope.

## Toolchain

```sh
export SECULAR_SDK=/absolute/path/to/secular-sdk   # default: ../secular-sdk
make build        # configure + compile arm64 console image
make run          # interactive QEMU serial console
make kick         # boot existing image via run-zircon; KICK_ARGS for its flags
make verify       # automated boot + console check
make clean        # remove out/
```

| Variable | Meaning |
| --- | --- |
| `SECULAR_SDK` | Standalone SDK root. Required. Must not equal the source root. |
| `SECULAR_PREBUILT` | Optional rust sysroot view (`linux-amd64` host+target rustlib). Default: `out/sdk-prebuilt`. |
| `ARCH` | Target architecture. Default: `arm64`. |
| `QEMU_ARGS` | Extra QEMU arguments forwarded by `make run` / `make kick`. |
| `KICK_ARGS` | Extra `zircon/scripts/run-zircon` flags for `make kick` (`-s`, `-m`, `-c`, …). |

The SDK is prepared outside this tree (`linux-amd64` clang/gn/ninja/rustc
and `qemu-system-aarch64`). GN uses `${SECULAR_SDK}/prebuilt/.../linux-amd64`
for host tools. Configure scripts may still merge rust host+target libs under
`out/sdk-prebuilt/third_party/rust/linux-amd64`. That view is a build output,
not source.

`SMOS_SDK` / `SMOS_SDK_ROOT` are accepted aliases of `SECULAR_SDK`.

## Build rules

- User-facing entry is **Makefile**. Do not add in-tree `prebuilt/`,
  Cargo/CMake/Android.bp, or a second toolchain tree.
- GN/Ninja remain the compiler graph. New GN args that point at the SDK
  belong in `products/build/config/secular_sdk.gni` and `out/*/args.gn`.
- Build artifacts go under `out/secular-boot-<arch>/`:
  `secular.zbi`, `linux-arm64-boot-shim.bin`, `artifacts.env`, logs.
- After implementation, `make build` must compile. After boot-path
  changes, `make verify` must pass (or `make run` must show a console
  if verification is not yet automated).
- Run `fx format-code` only when touching files that already follow
  Fuchsia formatting, and only from the repository that owns those
  files. Do not invent a parallel formatter.

## Source layout

| Path | Role |
| --- | --- |
| `zircon/` | Microkernel, phys/boot-shim, vDSO, zbi tool |
| `userspace/` | User-space tree (GN labels are `//userspace/...`) |
| `userspace/secular/` | userboot, console, console-launcher |
| `userspace/sys/` | component_manager and bootstrap framework |
| `userspace/devices/` | QEMU/console drivers |
| `userspace/zircon/bin/zxsh` | Interactive shell |
| `products/secular/` | Bringup-class product and ZBI |
| `products/boards/` | qemu-arm64 board |
| `products/build/` | GN templates; SDK path remapping |
| `products/bundles/` | Assembly / bringup bundles |
| `products/integration/` | Platform milestone pin (`MILESTONE`) |
| `products/manifests/` | Third-party pin manifests |
| `tools/secular-boot/` | configure / build / run / verify |
| `tools/ai-coding/` | Project-only Grok/Codex skills, rules, and agent defs |

Do not restore deleted Fuchsia products (`core`, `terminal`, `workbench`,
`starnix`, UI, graphics, connectivity stacks) without an explicit scope
change.

## Agent skills (Grok / Codex)

`tools/ai-coding/` is this repository's only skills library. Grok and Codex
must use it at session start. Do **not** recreate `.agents/`, `.grok/`, or
`.codex/` as a second copy.

| Path | Contents |
| --- | --- |
| `tools/ai-coding/skills/<name>/SKILL.md` | Task skills |
| `tools/ai-coding/rules/` | Extra coding rules |
| `tools/ai-coding/agents/` | Named subagent prompts |
| `tools/ai-coding/skills.json` | Skill index |

When a task matches a skill, read that `SKILL.md` before acting. New Secular
skills go under `tools/ai-coding/skills/<name>/`.

## Agent workflow

1. Keep the five goals above in view for every change.
2. Prefer the smallest change that preserves QEMU console boot.
3. Do not copy SDK files into the source tree.
4. Do not add dependencies unless the console image fails to build or
   boot without them.
5. After editing build or boot paths, run `make build` and, when the
   image exists, `make verify` or `make run`.
6. Report exact commands, output paths, and whether serial console
   input/output was verified.

## Language

Code comments, commit messages, and in-tree docs stay in English.
Terminal conversation with the requester may use Chinese when they do.
