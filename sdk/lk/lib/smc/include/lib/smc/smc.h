/*
 * Copyright (c) 2020, Google Inc. All rights reserved
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

#pragma once

#include <shared/lk/compiler.h>
#include <sys/types.h>

/* Unknown SMC (defined by ARM DEN 0028A(0.9.0) */
#define SMC_UNKNOWN 0xFFFFFFFFU

struct smc_ret8 {
    ulong r0;
    ulong r1;
    ulong r2;
    ulong r3;
    ulong r4;
    ulong r5;
    ulong r6;
    ulong r7;
};

#define ARM_FFA_MSG_EXTENDED_ARGS_COUNT 14

struct smc_ret18 {
    ulong r0;
    ulong r1;
    ulong r2;
    ulong r3;
    union {
        struct {
            ulong r4;
            ulong r5;
            ulong r6;
            ulong r7;
            ulong r8;
            ulong r9;
            ulong r10;
            ulong r11;
            ulong r12;
            ulong r13;
            ulong r14;
            ulong r15;
            ulong r16;
            ulong r17;
        };
        ulong req2_params[ARM_FFA_MSG_EXTENDED_ARGS_COUNT];
    };
};

STATIC_ASSERT(sizeof(struct smc_ret18) == sizeof(ulong) * 18);

struct smc_ret8 smc8(ulong r0,
                     ulong r1,
                     ulong r2,
                     ulong r3,
                     ulong r4,
                     ulong r5,
                     ulong r6,
                     ulong r7);

struct smc_ret8 hvc8(ulong r0,
                     ulong r1,
                     ulong r2,
                     ulong r3,
                     ulong r4,
                     ulong r5,
                     ulong r6,
                     ulong r7);

/*
 * same as smc8 but returns 18 registers with x8 and above set to zero on entry
 */
struct smc_ret18 smc8_ret18(ulong r0,
                            ulong r1,
                            ulong r2,
                            ulong r3,
                            ulong r4,
                            ulong r5,
                            ulong r6,
                            ulong r7);

/*
 * same as hvc8 but returns 18 registers with x8 and above set to zero on entry
 */
struct smc_ret18 hvc8_ret18(ulong r0,
                            ulong r1,
                            ulong r2,
                            ulong r3,
                            ulong r4,
                            ulong r5,
                            ulong r6,
                            ulong r7);

struct smc_ret18 smc18(ulong r0,
                       ulong r1,
                       ulong r2,
                       ulong r3,
                       ulong r4,
                       ulong r5,
                       ulong r6,
                       ulong r7,
                       ulong r8,
                       ulong r9,
                       ulong r10,
                       ulong r11,
                       ulong r12,
                       ulong r13,
                       ulong r14,
                       ulong r15,
                       ulong r16,
                       ulong r17);

struct smc_ret18 hvc18(ulong r0,
                       ulong r1,
                       ulong r2,
                       ulong r3,
                       ulong r4,
                       ulong r5,
                       ulong r6,
                       ulong r7,
                       ulong r8,
                       ulong r9,
                       ulong r10,
                       ulong r11,
                       ulong r12,
                       ulong r13,
                       ulong r14,
                       ulong r15,
                       ulong r16,
                       ulong r17);
