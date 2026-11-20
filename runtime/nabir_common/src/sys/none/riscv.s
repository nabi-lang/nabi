/*
 * riscv32 without an OS: _start, the weak board functions (Linux
 * syscalls: write 64, exit_group 94; morecore and clock_ns are Rust)
 * and the weak libm names of the inverse hyperbolic functions.
 */
.text
.weak nabi_bare_write
.type nabi_bare_write,@function
nabi_bare_write:
li a7, 64
ecall
ret
.weak nabi_bare_exit
.type nabi_bare_exit,@function
nabi_bare_exit:
li a7, 94
ecall
1: j 1b
.weak nabi_bare_morecore
.type nabi_bare_morecore,@function
nabi_bare_morecore:
tail nabir_linux_morecore
.weak nabi_bare_clock_ns
.type nabi_bare_clock_ns,@function
nabi_bare_clock_ns:
tail nabir_linux_clock_ns
.weak acosh
.type acosh,@function
acosh: tail nabir_acosh
.weak asinh
.type asinh,@function
asinh: tail nabir_asinh
.weak atanh
.type atanh,@function
atanh: tail nabir_atanh
.weak acoshf
.type acoshf,@function
acoshf: tail nabir_acoshf
.weak asinhf
.type asinhf,@function
asinhf: tail nabir_asinhf
.weak atanhf
.type atanhf,@function
atanhf: tail nabir_atanhf
.weak _start
.type _start,@function
_start:
.option push
.option norelax
la gp, __global_pointer$
.option pop
mv a0, sp
call nabir_bare_start
1: j 1b
