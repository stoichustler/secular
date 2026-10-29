# Protocol Buffers GN Build Support

This repository contains
[GN build system](https://gn.googlesource.com/gn/+/HEAD/) support for
[Protocol Buffers](https://github.com/protocolbuffers/protobuf). It's in its own
repository because it needs to be shared by
[Fuchsia](https://fuchsia.googlesource.com/fuchsia/) and
[Cobalt](https://fuchsia.googlesource.com/cobalt/).

This repo should be checked out such that:

* It is in `//build/secondary/third_party/protobuf`.
* Protobuf is in `//third_party/protobuf/src`.
* `//.gn` contains `secondary_source = "//build/secondary/"`

See the
[GN documentation on secondary_source](https://gn.googlesource.com/gn/+/master/docs/reference.md#other-help-topics-gn-file-variables).

## Updating Build Files

**WARNING: DO NOT MODIFY THE `BUILD.gn` FILE DIRECTLY!**

The `BUILD.gn` file in this directory is generated automatically. Any manual changes will be overwritten.

To make changes to the build configuration:

1.  Edit the `BUILD.input.gn` file to suit your needs, for example managing visibility, suppressing warnings, etc.
2.  Run the `gen.py` script located in this directory to regenerate the `BUILD.gn` file:
    ```
    ./gen.py [commit_hash]
    ```
    If no commit hash is specified, it defaults to `HEAD` of `//third_party/protobuf/src`.
3.  Commit both the updated `BUILD.input.gn` and the newly generated `BUILD.gn` file.

Failing to modify `BUILD.input.gn` will result in your changes being lost the next time the build file is regenerated.



