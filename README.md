# Secular

Secular is a Zircon-microkernel console system. The product is Fuchsia
bringup renamed: kernel, BOOTFS, interactive serial console (`zxsh`),
and the basic component/driver framework. It boots on
`qemu-system-aarch64`.

User-space lives under `userspace/` (GN labels are `//userspace/...`).
Bringup programs live under `userspace/secular/`. Boards, GN templates,
bundles, and third-party manifests live under `products/`. C++ still uses
`#include "src/..."` and `#include "build/..."`; `gn gen` maps those onto
the moved trees.

The compile toolchain lives in a separate SDK. Do not copy it into this
tree.

```sh
export SECULAR_SDK=$PWD/../secular-sdk
make build
make verify
make run
```

`make run` starts an interactive QEMU serial console. Extra QEMU flags
can be passed with `make run QEMU_ARGS='-d guest_errors'`.

Build outputs are written under `out/secular-boot-arm64/`, including
`secular.zbi`, the QEMU boot-shim, and `artifacts.env`.
