/*
 * nabi_tls and nabi_tls_addr, ELF: aarch64 Linux. Initial exec, so a
 * shared object's block may sit anywhere in the static TLS area.
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
.type nabi_tls_addr,%function
nabi_tls_addr:
adrp x0, :gottprel:nabi_tls
ldr x0, [x0, :gottprel_lo12:nabi_tls]
mrs x16, tpidr_el0
add x0, x16, x0
ret
