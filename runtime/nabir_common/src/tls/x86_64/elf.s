/*
 * nabi_tls and nabi_tls_addr, ELF: x86_64 Linux and FreeBSD (both
 * TLS variant II, fs:0 is the TCB). Initial exec. size: the block's
 * size.
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
mov rax, qword ptr fs:[0]
add rax, qword ptr [rip + nabi_tls@GOTTPOFF]
ret
