# Products

This directory holds the Secular product and the GN/board/bundle/manifest
trees that used to sit at the repository root:

| Path | Role |
| --- | --- |
| `secular/` | qemu-arm64 bringup image (`secular.zbi`) |
| `boards/` | qemu-arm64 board |
| `build/` | GN templates and SDK remapping |
| `bundles/` | Assembly input bundles |
| `manifests/` | Third-party pin manifests |

Secular is the only in-tree product. It matches Fuchsia **bringup**:

- Zircon kernel and phys/boot-shim
- BOOTFS with userboot, component_manager, and bootstrap
- Interactive serial console (`console`, `console-launcher`, `zxsh`)

The GN target is `//products/secular:secular` (the ZBI) plus
`//products/secular:boot_shim` for QEMU.
