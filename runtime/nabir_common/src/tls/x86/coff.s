/*
 * nabi_tls and nabi_tls_addr, COFF: i686 Windows. The block is in
 * .tls$, reached through _tls_index (nabic's startup object defines
 * it with the TLS directory); C names carry a leading underscore.
 * size: the block's size.
 */
.section .tls$,"dw"
.p2align 3
.globl _nabi_tls
_nabi_tls:
.zero {size}
.text
.globl _nabi_tls_addr
.p2align 4
_nabi_tls_addr:
mov eax, dword ptr [__tls_index]
mov ecx, dword ptr fs:[0x2c]
mov ecx, dword ptr [ecx + 4*eax]
lea eax, [ecx + _nabi_tls@SECREL32]
ret
