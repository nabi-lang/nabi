/*
 * nabi_tls and nabi_tls_addr, ELF: riscv64 Linux and riscv32 without
 * an OS (one image; the startup code points tp at the block). Local
 * exec. size: the block's size.
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
nabi_tls_addr:
lui a0, %tprel_hi(nabi_tls)
add a0, a0, tp, %tprel_add(nabi_tls)
addi a0, a0, %tprel_lo(nabi_tls)
ret
