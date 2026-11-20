/*
 * Weak rust_eh_personality, ELF: every ELF target whose return
 * instruction is ret (x86_64, aarch64, x86, riscv). Nothing here
 * unwinds.
 */
.weak rust_eh_personality
.type rust_eh_personality, %function
rust_eh_personality:
ret
