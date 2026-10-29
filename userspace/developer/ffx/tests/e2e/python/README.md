# Test execution

## Set up
```shell
$ fx set core.x64 \
    --with-host //userspace/developer/ffx/tests/e2e/python:tests

$ fx build
```

## Local execution
```shell
$ fx test //userspace/developer/ffx/tests/e2e/python:<test> --e2e --output
```
where `<test>` is one of:

* `ffx_host_tool_e2e_test`
* `ffx_strict_host_tool_e2e_test`
* `direct_conn_e2e_test`
