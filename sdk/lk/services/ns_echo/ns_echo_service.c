/*
 * Copyright (c) 2026 Google Inc. All rights reserved
 *
 * Permission is hereby granted, free of charge, to any person obtaining
 * a copy of this software and associated documentation files
 * (the "Software"), to deal in the Software without restriction,
 * including without limitation the rights to use, copy, modify, merge,
 * publish, distribute, sublicense, and/or sell copies of the Software,
 * and to permit persons to whom the Software is furnished to do so,
 * subject to the following conditions:
 *
 * The above copyright notice and this permission notice shall be
 * included in all copies or substantial portions of the Software.
 *
 * THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND,
 * EXPRESS OR IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF
 * MERCHANTABILITY, FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT.
 * IN NO EVENT SHALL THE AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY
 * CLAIM, DAMAGES OR OTHER LIABILITY, WHETHER IN AN ACTION OF CONTRACT,
 * TORT OR OTHERWISE, ARISING FROM, OUT OF OR IN CONNECTION WITH THE
 * SOFTWARE OR THE USE OR OTHER DEALINGS IN THE SOFTWARE.
 */

#include <err.h>
#include <lib/ktipc/ktipc.h>
#include <lib/trusty/ipc.h>
#include <lib/trusty/uuid.h>
#include <lk/init.h>
#include <lk/trace.h>

#define ECHO_PORT "com.android.trusty.kernel.echo"
#define ECHO_MAX_MSG 256

static int echo_on_message(const struct ktipc_port* port,
                           struct handle* chan,
                           void* ctx) {
    uint8_t buf[ECHO_MAX_MSG];
    int rc;

    (void)port;
    (void)ctx;

    rc = ktipc_recv(chan, 1, buf, sizeof(buf));
    if (rc < 0) {
        TRACEF("echo: recv failed (%d)\n", rc);
        return rc;
    }
    return ktipc_send(chan, buf, (size_t)rc);
}

static const struct ktipc_srv_ops echo_ops = {
        .on_message = echo_on_message,
};

static const struct ktipc_port_acl echo_acl = {
        .flags = IPC_PORT_ALLOW_NS_CONNECT | IPC_PORT_ALLOW_TA_CONNECT,
        .uuids = NULL,
        .uuid_num = 0,
        .extra_data = NULL,
};

static const struct ktipc_port echo_port = {
        .name = ECHO_PORT,
        .uuid = &kernel_uuid,
        .msg_max_size = ECHO_MAX_MSG,
        .msg_queue_len = 1,
        .acl = &echo_acl,
        .priv = NULL,
};

static struct ktipc_server echo_server =
        KTIPC_SERVER_INITIAL_VALUE(echo_server, "ns_echo_ktipc_server");

static void ns_echo_init(uint level) {
    int rc;

    rc = ktipc_server_start(&echo_server);
    if (rc < 0) {
        panic("Failed (%d) to start ns echo server\n", rc);
    }
    rc = ktipc_server_add_port(&echo_server, &echo_port, &echo_ops);
    if (rc < 0) {
        panic("Failed (%d) to create ns echo port\n", rc);
    }
}

LK_INIT_HOOK(ns_echo, ns_echo_init, LK_INIT_LEVEL_APPS);
