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

#include <asm.h>
#include <arch/asm_macros.h>

.macro smc8_hvc8, instr
    /*
     * Save x8 (return value ptr) and lr. The SMC calling convention says el3
     * does not need to preserve x8. The normal ABI does not require either x8
     * or lr to be preserved.
     */
    push    x8, lr
    \instr  #0
    pop     x8, lr

    /* Copy 8-register smc return value to struct smc_ret8 return value */
    stp     x0, x1, [x8], #16
    stp     x2, x3, [x8], #16
    stp     x4, x5, [x8], #16
    stp     x6, x7, [x8], #16

    ret
.endm

FUNCTION(smc8)
    smc8_hvc8 smc

FUNCTION(hvc8)
    smc8_hvc8 hvc

.macro smc8_hvc8_ret18, instr
    /*
     * Save x19 and lr. The SMC calling convention says el3 does not need to
     * preserve x8 (return value ptr). The aarch64 calling convention (AAPCS64)
     * says x8 is caller-saved even when used as the return value pointer.
     */
    push    x19, lr
    mov     x19, x8 /* preserve return value pointer in x19  */

    mov     x8,  xzr
    mov     x9,  xzr
    mov     x10, xzr
    mov     x11, xzr
    mov     x12, xzr
    mov     x13, xzr
    mov     x14, xzr
    mov     x15, xzr
    mov     x16, xzr
    mov     x17, xzr

    \instr  #0

    /* Copy 8-register smc return value plus x8-x17 into struct smc_ret18 */
    stp     x0,  x1,  [x19], #16
    stp     x2,  x3,  [x19], #16
    stp     x4,  x5,  [x19], #16
    stp     x6,  x7,  [x19], #16
    stp     x8,  x9,  [x19], #16
    stp     x10, x11, [x19], #16
    stp     x12, x13, [x19], #16
    stp     x14, x15, [x19], #16
    stp     x16, x17, [x19], #16

    pop     x19, lr

    ret
.endm

FUNCTION(smc8_ret18)
    smc8_hvc8_ret18 smc

FUNCTION(hvc8_ret18)
    smc8_hvc8_ret18 hvc

.macro smc18_hvc18, instr
    push x19, lr
    mov  x19, x8

    ldp x8,  x9,  [sp, #16]
    ldp x10, x11, [sp, #32]
    ldp x12, x13, [sp, #48]
    ldp x14, x15, [sp, #64]
    ldp x16, x17, [sp, #80]

    \instr  #0

    stp     x0,  x1,  [x19], #16
    stp     x2,  x3,  [x19], #16
    stp     x4,  x5,  [x19], #16
    stp     x6,  x7,  [x19], #16
    stp     x8,  x9,  [x19], #16
    stp     x10, x11, [x19], #16
    stp     x12, x13, [x19], #16
    stp     x14, x15, [x19], #16
    stp     x16, x17, [x19], #16

    pop     x19, lr

    ret
.endm

FUNCTION(smc18)
    smc18_hvc18 smc

FUNCTION(hvc18)
    smc18_hvc18 hvc
