# BoringSSL (Secular)

Fuchsia pin and roll notes: [README.fuchsia](README.fuchsia).

Secular only boots QEMU linux-arm64. Unused copies in this checkout were
removed. They are not required by `//third_party/boringssl:crypto` /
`:ssl` / `:crypto-static`.

## Removed trees

### `apple-*` and `win-*` generated assembly

Fuchsia-packaged per-OS copies of Perl-generated asm. Empty on disk and
not listed in `BUILD.gn` or `BUILD.generated.gni`. Current GN sources are
`src/gen/bcm/*.S` (each file is `#if`-guarded by `OPENSSL_*` plus ABI:
`__ELF__`, `__APPLE__`, `_WIN32`).

Removed:

- `apple-aarch64/`
- `apple-arm/`
- `apple-x86/`
- `apple-x86_64/`
- `win-aarch64/`
- `win-x86/`
- `win-x86_64/`

Kept (also unused by current GN, Linux ABI copies of the same files):

- `linux-aarch64/`
- `linux-arm/`
- `linux-x86/`
- `linux-x86_64/`

Do not delete `src/gen/bcm/`. Those files are in `crypto_sources_asm`.

### `src/third_party/wycheproof_testvectors/`

Google Wycheproof crypto test vectors (~138MB). Only referenced from
`BUILD.generated_tests.gni` `crypto_test_data` (BoringSSL unit tests).
`BUILD.gn` skips those paths when declaring `resource()` test data.
`:crypto` / `:ssl` do not link this directory.

Do not delete `src/third_party/fiat/` (production crypto).
