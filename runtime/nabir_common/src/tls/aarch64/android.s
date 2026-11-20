/*
 * nabi_tls and nabi_tls_addr, ELF: aarch64 Android. A TLS descriptor,
 * so a dlopened library works (the linker relaxes it to local exec in
 * an executable). Aligned to 64: bionic keeps its own slots in the
 * first 8 words after the thread pointer. size: the block's size.
 */
.pushsection .tbss,"awT",@nobits
.p2align 6
.globl nabi_tls
.type nabi_tls,@object
.size nabi_tls,{size}
nabi_tls:
.zero {size}
.popsection
.globl nabi_tls_addr
.hidden nabi_tls_addr
.type nabi_tls_addr,%function
nabi_tls_addr:
stp x29, x30, [sp, #-16]!
mov x29, sp
adrp x0, :tlsdesc:nabi_tls
ldr x1, [x0, :tlsdesc_lo12:nabi_tls]
add x0, x0, :tlsdesc_lo12:nabi_tls
.tlsdesccall nabi_tls
blr x1
mrs x1, tpidr_el0
add x0, x1, x0
ldp x29, x30, [sp], #16
ret
