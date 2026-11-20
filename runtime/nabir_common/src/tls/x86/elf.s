/*
 * nabi_tls and nabi_tls_addr, ELF: i686 Linux. Local exec.
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
nabi_tls_addr:
mov eax, dword ptr gs:[0]
lea eax, [eax + nabi_tls@NTPOFF]
ret
