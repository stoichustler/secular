# Protobuf (Secular)

Fuchsia pin and roll notes: [README.fuchsia](README.fuchsia).

Secular uses the C++ runtime, `protoc`, and Python helpers under
`src/src/`, `src/upb/`, and `src/python/`. Language runtimes and extra
compatibility trees were removed from this checkout. They are not in
`//products/build/secondary/third_party/protobuf/BUILD.gn`.

## Removed trees

- `src/java/` — Java runtime and tests
- `src/csharp/` — C# runtime, tools, and compatibility tests
- `src/php/` — PHP runtime
- `src/objectivec/` — Objective-C runtime
- `src/compatibility/` — Java gencode compatibility smoke tests

The C++ `protoc` plugins remain under
`src/src/google/protobuf/compiler/{java,csharp,php,objectivec,...}`.
Those are host compiler sources, not the deleted language trees.
