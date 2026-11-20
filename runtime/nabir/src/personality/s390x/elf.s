/*
 * Weak rust_eh_personality, ELF: s390x Linux. Nothing here unwinds.
 */
.weak rust_eh_personality
.type rust_eh_personality, %function
rust_eh_personality:
br %r14
