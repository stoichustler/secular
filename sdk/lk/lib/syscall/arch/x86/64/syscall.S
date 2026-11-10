/*
 * Copyright (c) 2018 Intel Corporation
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

#define SYSCALL_ENTRY_SIZE    0x08
#define SYSCALL_STACK_OFF     0x08

/*
 * Entry state:
 * %rax: syscall number set by syscall stub
 * %rdi: 1st argument
 * %rsi: 2nd argument
 * %rdx: 3rd argument
 * %rcx: return address saved by syscall instructions (4th argument in ABI)
 * %r8: 5th argument (unused?)
 * %r9: 6th argument (unused?)
 * %r10: 4th argument copied from %rcx by syscall stub
 * %r11: flags saved by syscall instructions
 * %rsp: user-space stack (ignored)
 * %r15: user-space stack saved by stub
 */
FUNCTION (x86_syscall)
    /* clear user-space controlled stack pointer */
    xor %esp, %esp

    /* switch to kernel gs */
    swapgs
    movq  %gs:SYSCALL_STACK_OFF, %rsp

    sti
    pushq %rcx /* Ring 3 return address */
    pushq %r15 /* Ring 3 RSP */
    movq  %r10, %rcx

    pushq %r11 /* saved flags */
    subq  $0x8, %rsp /* align stack */

    /* Check if syscall index (%rax) is in range */
    cmp $nr_syscalls, %rax
    jae .Lundefined_syscall

    /* Load entry from syscall table */
    movq syscall_table(,%rax,SYSCALL_ENTRY_SIZE), %rax

    /* Check if syscall entry is NULL */
    test %rax, %rax
    jz .Lundefined_syscall

    /* Call syscall handler now in %rax */
.Lsyscall_addr_ready:
    call *%rax

    /*
     * Clear non-callee saved registers to avoid leaking kernel data to
     * user-space.
     * Skip floating/vector registers since the kernel is not allowed to use
     * them.
     * Skip 4th argument (%rcx) and %r11 since they get overwritten with
     * user-space pointers or flags below.
     */
    xor %edi, %edi /* clear 1st argument/temporary register */
    xor %esi, %esi /* clear 2nd argument/temporary register */
    xor %edx, %edx /* clear 3rd argument/temporary register */
    xor %r8d, %r8d /* clear 5th argument/temporary register */
    xor %r9d, %r9d /* clear 6th argument/temporary register */
    xor %r10d, %r10d /* clear temporary register */

    addq  $0x8, %rsp /* remove stack alignment padding */
    popq %r11 /* saved flags */
    popq %r15 /* pop RSP */
    popq %rcx /* pop RIP */

    /*
     * switch to user gs
     * Have to make sure there is no interrupt triggered between swapgs and sysexit instructions,
     * if this happens, GS.base is switch to user level GS.base, but current context is still in
     * kernel level, it leads to get global states failure on current processor.
     */
    cli
    swapgs

    /*
     * Clear kernel stack pointer to avoid leaking ASLR info. We don't restore
     * the user-space stack pointer here, since sysret can trigger a fault and
     * we don't want that fault handler to use a user-space controlled stack
     * pointer. A 0 stack pointer will trigger a double fault instead.
     */
    xor %esp, %esp

    /*
     * Exit state:
     * %rax: return code
     * %rcx: return address used by sysret instruction
     * %r11: saved flags used by sysret instruction
     * %r15: user-space stack used by stub
     */
    sysretq

    /*
     * Use sys_undefined as the syscall handler if the index in %rax is out of
     * range of the syscall table or if %rax is in range, but the entry in the
     * syscall table is NULL.
     */
.Lundefined_syscall:
    movq $sys_undefined, %rax
    jmp .Lsyscall_addr_ready
