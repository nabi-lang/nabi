/*
 * nabi_tls and nabi_tls_addr, COFF: x86_64 Windows. The block is in
 * .tls$, reached through _tls_index (nabic's startup object defines
 * it with the TLS directory). size: the block's size.
 */
.section .tls$,"dw"
.p2align 3
.globl nabi_tls
nabi_tls:
.zero {size}
.text
.globl nabi_tls_addr
.p2align 4
nabi_tls_addr:
mov eax, dword ptr [rip + _tls_index]
mov rcx, qword ptr gs:[0x58]
mov rcx, qword ptr [rcx + 8*rax]
lea rax, [rcx + nabi_tls@SECREL32]
ret
