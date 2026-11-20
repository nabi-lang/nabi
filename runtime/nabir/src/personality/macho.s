/*
 * Weak rust_eh_personality, Mach-O: Apple targets. Nothing here
 * unwinds.
 */
.globl _rust_eh_personality
.weak_definition _rust_eh_personality
.p2align 2
_rust_eh_personality:
ret
