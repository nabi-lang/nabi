//! The mingw-w64 module definitions the COFF linker embeds
//! (`windows/<arch>/*.def`, `importlibs.sh`), one import library each.

use nabic_target::TargetSpec;

/// `(link name, module definition)`, the UCRT first: `ntdll` repeats some
/// C functions, and the first import library that has a symbol wins.
pub type Defs = [(&'static str, &'static str); 11];

macro_rules! defs {
    ($arch:literal) => {
        defs!($arch; "ucrtbase" "kernel32" "synchronization" "ws2_32" "advapi32" "bcrypt"
            "userenv" "shell32" "ole32" "user32" "ntdll")
    };
    ($arch:literal; $($name:literal)*) => {
        [$(($name, include_str!(concat!("../../windows/", $arch, "/", $name, ".def")))),*]
    };
}

const X86_64: Defs = defs!("x86_64");
const AARCH64: Defs = defs!("aarch64");
const I686: Defs = defs!("i686");

/// The module definitions of `t`'s architecture.
pub fn of(t: &TargetSpec) -> Result<&'static Defs, String> {
    match t.arch.as_str() {
        "x86_64" => Ok(&X86_64),
        "aarch64" => Ok(&AARCH64),
        "x86" => Ok(&I686),
        _ => Err(format!("no Windows libraries for {}", t.name)),
    }
}
