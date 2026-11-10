/*
 * Copyright (c) 2014 Travis Geiselbrecht
 *
 * Use of this source code is governed by a MIT-style
 * license that can be found in the LICENSE file or at
 * https://opensource.org/licenses/MIT
 */
#include <lk/asm.h>
#include <arch/asm_macros.h>

// stay in sync with arm64/thread.c arm64_context_switch()

/* void arm64_context_switch(vaddr_t *old_sp, vaddr_t new_sp); */
FUNCTION(arm64_context_switch)
    /* save old frame */
    push x29, lr
    push x27, x28
    push x25, x26
    push x23, x24
    push x21, x22
    push x19, x20
    // skip x18, it is our per cpu pointer
    mrs  x16, tpidr_el0
    mrs  x17, tpidrro_el0
    push x16, x17

    /* save old sp */
    mov  x15, sp
    str  x15, [x0]

    /* load new sp */
    mov  sp, x1

    /* restore new frame */
    pop  x16, x17
    msr  tpidr_el0, x16
    msr  tpidrro_el0, x17
    pop  x19, x20
    pop  x21, x22
    pop  x23, x24
    pop  x25, x26
    pop  x27, x28
    pop  x29, lr

    ret

    /* drop from whatever EL we may already be in to EL1.
     * carefully avoids using x0-x3 since this is called from start.S
     * which is trying to preserve them.
     */
FUNCTION(arm64_elX_to_el1)
    mrs x4, CurrentEL

    cmp x4, #(0b01 << 2)
    bne .notEL1
    /* Already in EL1 */
    ret

.notEL1:
    cmp x4, #(0b10 << 2)
    beq .inEL2

    /* set EL2 to 64bit */
    mrs x4, scr_el3
    orr x4, x4, #(1<<10)
    msr scr_el3, x4

    /* prep this mode's ELR and SPSR to drop into EL1 */
    adr x4, .Ltarget
    msr elr_el3, x4

    mov x4, #((0b1111 << 6) | (0b0101)) /* EL1h runlevel */
    msr spsr_el3, x4
    b   .confEL1

.inEL2:
    /* prep this mode's ELR and SPSR to drop into EL1 */
    adr x4, .Ltarget
    msr elr_el2, x4
    mov x4, #((0b1111 << 6) | (0b0101)) /* EL1h runlevel */
    msr spsr_el2, x4

.confEL1:
    /* disable EL2 coprocessor traps */
    mov x4, #0x33ff
    msr cptr_el2, x4

    /* set EL1 to 64bit and disable EL2 instruction traps */
    mov x4, #(1<<31)
    msr hcr_el2, x4

    /* set up the EL1 bounce interrupt */
    mov x4, sp
    msr sp_el1, x4

    /* make sure MPIDR_EL1 and MIDR_EL1 are set with the proper values */
    mrs x4, mpidr_el1
    msr vmpidr_el2, x4
    mrs x4, midr_el1
    msr vpidr_el2, x4

    isb
    eret

.Ltarget:
    ret

/*
 * Stay at EL2 with VHE (HCR_EL2.{RW,E2H,TGE}=1) so EL1-named sysregs work
 * and Secular is entered at EL2, matching ATF BL33 / MTK lk_jump64.
 * If ATF entered us below EL2, return and leave CurrentEL as-is (lkboot
 * panics before chain-load). If EL2 but no VHE, still stay at EL2 as a
 * classic hypervisor (HCR.RW) rather than dropping to EL1.
 * Does not touch x0-x3 (boot args).
 */
FUNCTION(arm64_el2_boot)
    mrs     x4, CurrentEL
    cmp     x4, #(0b10 << 2)
    b.ne    .Lboot_el2_done

    /* ID_AA64MMFR1_EL1.VH, bits [11:8] */
    mrs     x4, id_aa64mmfr1_el1
    ubfx    x4, x4, #8, #4
    cbz     x4, .Lboot_el2_no_vhe

    /* Make EL1-named ID registers match the real CPU for later EL1 guests. */
    mrs     x4, mpidr_el1
    msr     vmpidr_el2, x4
    mrs     x4, midr_el1
    msr     vpidr_el2, x4

    /* HCR_EL2 = RW (31) | TGE (27) | E2H (34) */
    mov     x4, #((1 << 31) | (1 << 27))
    movk    x4, #4, lsl #32
    msr     hcr_el2, x4
    isb

    /* ICC_SRE_EL2: SRE | Enable, so GICv3 sysregs work for the next image. */
    mrs     x4, s3_4_c12_c9_5
    orr     x4, x4, #1          /* SRE */
    orr     x4, x4, #(1 << 3)   /* Enable */
    msr     s3_4_c12_c9_5, x4
    isb

    /* E2H=1: CPTR_EL2 uses the CPACR layout; FPEN=0b11, no FP trap. */
    mov     x4, #(3 << 20)
    msr     cptr_el2, x4

    /* E2H=1 CNTHCTL: EL1PTEN (11) | EL1PCTEN (10) | EL0VCTEN (1) | EL0PCTEN (0). */
    mov     x4, #0xc03
    msr     cnthctl_el2, x4

    msr     spsel, #1
    ret

.Lboot_el2_no_vhe:
    /* No FEAT_VHE: keep EL2, EL1 is AArch64. Do not drop to EL1. */
    mov     x4, #(1 << 31)
    msr     hcr_el2, x4
    isb
    mrs     x4, s3_4_c12_c9_5
    orr     x4, x4, #1
    orr     x4, x4, #(1 << 3)
    msr     s3_4_c12_c9_5, x4
    isb
    msr     spsel, #1
    ret

.Lboot_el2_done:
    ret
END_FUNCTION(arm64_el2_boot)

/* void arm64_chain_load(paddr_t entry, ulong arg0, ulong arg1, ulong arg2, ulong arg3) */
FUNCTION(arm64_chain_load)
    /* x0 = entry, x1-x4 = linux boot args. Shuffle to x0-x3 args, x5 = entry. */
    mov     x5, x0
    mov     x0, x1
    mov     x1, x2
    mov     x2, x3
    mov     x3, x4

#if WITH_KERNEL_VM
    /* Caller already identity-mapped this trampoline. Drop MMU and caches.
     * With VHE, SCTLR_EL1 aliases SCTLR_EL2; same write turns off EL2&0. */
    mrs     x6, sctlr_el1
    bic     x6, x6, #(1 << 0)   /* M */
    bic     x6, x6, #(1 << 2)   /* C */
    bic     x6, x6, #(1 << 12)  /* I */
    msr     sctlr_el1, x6
    isb
    tlbi    vmalle1
    ic      iallu
    dsb     sy
    isb
#endif
    /* Linux boot protocol: x1-x3 reserved 0; x18 is the platform register. */
    mov     x18, xzr
    br      x5
END_FUNCTION(arm64_chain_load)
