/*
 * nabi_tls and nabi_tls_addr, COFF: aarch64 Windows. The block is in
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
.p2align 2
nabi_tls_addr:
adrp x8, _tls_index
ldr w8, [x8, :lo12:_tls_index]
ldr x9, [x18, #0x58]
ldr x9, [x9, x8, lsl #3]
add x9, x9, :secrel_hi12:nabi_tls
add x0, x9, :secrel_lo12:nabi_tls
ret
