/*
 * Weak rust_eh_personality and the EHABI personality routines libgcc
 * would bring (ARM unwind tables name them), ELF: armv7 Linux.
 * Nothing here unwinds.
 */
.weak rust_eh_personality
.type rust_eh_personality, %function
rust_eh_personality:
.weak __aeabi_unwind_cpp_pr0
.type __aeabi_unwind_cpp_pr0, %function
__aeabi_unwind_cpp_pr0:
.weak __aeabi_unwind_cpp_pr1
.type __aeabi_unwind_cpp_pr1, %function
__aeabi_unwind_cpp_pr1:
.weak __aeabi_unwind_cpp_pr2
.type __aeabi_unwind_cpp_pr2, %function
__aeabi_unwind_cpp_pr2:
bx lr
