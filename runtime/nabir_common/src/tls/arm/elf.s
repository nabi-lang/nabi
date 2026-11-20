/*
 * nabi_tls and nabi_tls_addr, ELF: armv7 Linux. Local exec, the
 * thread pointer in TPIDRURO. size: the block's size.
 */
.pushsection .tbss,"awT",%nobits
.p2align 3
.globl nabi_tls
.type nabi_tls,%object
.size nabi_tls,{size}
nabi_tls:
.zero {size}
.popsection
.globl nabi_tls_addr
.hidden nabi_tls_addr
.type nabi_tls_addr,%function
.p2align 2
nabi_tls_addr:
mrc p15, 0, r0, c13, c0, 3
ldr r1, 1f
add r0, r0, r1
bx lr
1: .long nabi_tls(TPOFF)
