# Secular

Secular is a Zircon-microkernel console system. The product is Fuchsia
bringup renamed: kernel, BOOTFS, interactive serial console (`zxsh`),
and the basic component/driver framework. It boots on
`qemu-system-aarch64`.

- User-space lives under `userspace/` (GN labels are `//userspace/...`).

- Bringup programs live under `userspace/secular/`.

- Boards, GN templates, bundles, integration, and third-party manifests live under `products/`.

The compile toolchain lives in a separate SDK. Do not copy it into this
tree.

```sh
make build
make verify
make run
make qemu
make kick
```

`make run` builds and starts an interactive QEMU serial console. Extra QEMU
flags can be passed with `make run QEMU_ARGS='-d guest_errors'`.

`make kick` boots the existing image through `zircon/scripts/run-zircon`
without rebuilding. Extra run-zircon flags go in `KICK_ARGS`:

```sh
make kick
make kick KICK_ARGS='-s 4 -m 2048'
make kick KICK_ARGS='--dry-run'
make kick KICK_ARGS='-s 2' QEMU_ARGS='-d guest_errors'
```

Refer to [reference](products/reference).

Build outputs are written under `out/secular-boot-arm64/`, including
`secular.zbi`, the QEMU boot-shim, and `artifacts.env`.

---

Everyday's a Hustle.
