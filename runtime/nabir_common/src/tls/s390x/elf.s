/*
 * nabi_tls and nabi_tls_addr, ELF: s390x Linux. Local exec as GCC's:
 * the thread pointer is a0:a1, the offset a literal after the code.
 * size: the block's size.
 */
.pushsection .tbss,"awT",@nobits
.p2align 3
.globl nabi_tls
.type nabi_tls,@object
.size nabi_tls,{size}
nabi_tls:
.zero {size}
.popsection
.globl nabi_tls_addr
.hidden nabi_tls_addr
.type nabi_tls_addr,@function
.p2align 3
nabi_tls_addr:
ear %r1, %a0
sllg %r2, %r1, 32
ear %r2, %a1
larl %r1, 1f
ag %r2, 0(%r1)
br %r14
.p2align 3
1: .quad nabi_tls@ntpoff
