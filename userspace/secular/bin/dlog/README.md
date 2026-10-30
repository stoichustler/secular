# dlog

Print the kernel debuglog to stdout.

```sh
dlog            # dump existing messages and exit
dlog -f         # follow new messages
dlog -t         # text only (no timestamps)
dlog -p <pid>   # only messages from pid
```

`dlog` reads `fuchsia.boot.ReadOnlyLog`, which the serial console already
receives from component_manager.
