/*
 * nabi_tls and nabi_tls_addr, ELF: x86_64 Android. General dynamic,
 * so a dlopened library works (the linker relaxes it to local exec in
 * an executable). size: the block's size.
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
push rbp
mov rbp, rsp
.byte 0x66
lea rdi, [rip + nabi_tls@TLSGD]
.byte 0x66, 0x66, 0x48
call __tls_get_addr@PLT
pop rbp
ret
