/*
 * nabi_tls and nabi_tls_addr, Mach-O: aarch64-apple-darwin.
 * The block is a TLV descriptor in __thread_vars; the accessor
 * tail-calls its thunk. size: the block's size.
 */
.tbss _nabi_tls$tlv$init, {size}, 3
.section __DATA,__thread_vars,thread_local_variables
.globl _nabi_tls
_nabi_tls:
.quad __tlv_bootstrap
.quad 0
.quad _nabi_tls$tlv$init
.text
.private_extern _nabi_tls_addr
.globl _nabi_tls_addr
.p2align 2
_nabi_tls_addr:
adrp x0, _nabi_tls@TLVPPAGE
ldr x0, [x0, _nabi_tls@TLVPPAGEOFF]
ldr x16, [x0]
/* The thunk returns the address in x0. */
br x16
