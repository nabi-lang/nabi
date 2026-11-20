//! The command line, as gcc and clang take it (`CC="nabi cc"` in a
//! Makefile or `CMake`).

use std::path::PathBuf;

/// What the command asks for.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Mode {
    /// Compile and link.
    #[default]
    Link,
    /// `-c`: objects.
    Compile,
    /// `-S`: assembly.
    Asm,
    /// `-E`: preprocessed source.
    Preprocess,
}

/// A C standard: `-std=`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Std {
    /// 1989, 1999, 2011, 2017 or 2023.
    pub year: u32,
    pub gnu: bool,
}

impl Default for Std {
    fn default() -> Self {
        Self {
            year: 2017,
            gnu: true,
        }
    }
}

impl Std {
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        let (gnu, rest) = match s.strip_prefix("gnu") {
            Some(r) => (true, r),
            None => (
                false,
                s.strip_prefix('c').or_else(|| s.strip_prefix("iso9899:"))?,
            ),
        };
        let year = match rest {
            "89" | "90" | "1990" | "199409" => 1989,
            "99" | "9x" | "1999" => 1999,
            "11" | "1x" | "2011" => 2011,
            "17" | "18" | "2017" | "2018" => 2017,
            "23" | "2x" | "2023" => 2023,
            _ => return None,
        };
        Some(Self { year, gnu })
    }

    /// `__STDC_VERSION__`, if the standard has one.
    #[must_use]
    pub const fn version(self) -> Option<&'static str> {
        match self.year {
            1999 => Some("199901L"),
            2011 => Some("201112L"),
            2017 => Some("201710L"),
            2023 => Some("202311L"),
            _ => None,
        }
    }
}

/// The parsed command line.
#[derive(Debug, Default)]
#[allow(clippy::struct_excessive_bools)]
pub struct Flags {
    pub mode: Mode,
    pub output: Option<PathBuf>,
    /// C sources.
    pub sources: Vec<PathBuf>,
    /// Objects, archives and libraries for the link, `-l`/`-L` and
    /// `-Wl,` options among them, in order.
    pub link: Vec<String>,
    /// Options the preprocessor takes (`-I`, `-D`, `-U`, `-include`, ...),
    /// in order, as `nabicc::sysinc::Opts` reads them.
    pub pp: Vec<String>,
    pub std: Std,
    /// `-O`: `0`, `1`, `2`, `3`, `s`, `z`.
    pub opt: String,
    pub debug: bool,
    pub target: Option<String>,
    /// `-M*`: where the dependency list goes, its target names, whether
    /// system headers count, and phony targets.
    pub deps: Option<Deps>,
    pub verbose: bool,
    pub version: bool,
    pub help: bool,
    /// `-dumpmachine`, `-dumpversion`, `-print-...`: answered and done.
    pub dump: Option<String>,
    pub shared: bool,
    pub stat: bool,
    /// `-x c` seen (stdin or unusual suffixes are C).
    pub lang_c: bool,
}

#[derive(Debug, Default, Clone)]
pub struct Deps {
    pub file: Option<PathBuf>,
    pub targets: Vec<String>,
    pub system: bool,
    pub phony: bool,
    /// `-M` / `-MM`: the list instead of compiling.
    pub only: bool,
}

const HELP_FLAGS: &str = "-c, -S, -E, -o <file>, -I <dir>, -isystem <dir>, -D <name>[=<value>], \
                          -U <name>, -include <file>, -std=<c89..c23, gnu89..gnu23>, -O<0-3,s,z>, \
                          -g, -l<lib>, -L<dir>, -Wl,<opts>, -M/-MD/-MMD/-MF/-MT/-MP, \
                          --target=<triple>";

impl Flags {
    /// Parse `args` (without the program name).
    pub fn parse(args: &[String]) -> Result<Self, String> {
        let mut f = Self {
            opt: "0".to_owned(),
            ..Self::default()
        };
        let mut i = 0;
        let value = |i: &mut usize, flag: &str, a: &str| -> Result<String, String> {
            let rest = &a[flag.len()..];
            let rest = rest.strip_prefix('=').unwrap_or(rest);
            if !rest.is_empty() {
                return Ok(rest.to_owned());
            }
            *i += 1;
            args.get(*i)
                .cloned()
                .ok_or_else(|| format!("`{flag}` needs a value after it"))
        };
        while i < args.len() {
            let a = args[i].as_str();
            match a {
                "-c" => f.mode = Mode::Compile,
                "-S" => f.mode = Mode::Asm,
                "-E" => f.mode = Mode::Preprocess,
                "-v" => f.verbose = true,
                "--version" | "-V" => f.version = true,
                "--help" | "-h" => f.help = true,
                "-g" | "-ggdb" | "-g2" | "-g3" | "-gdwarf" | "-gdwarf-4" | "-gdwarf-5"
                | "-gfull" | "-gcodeview" => f.debug = true,
                "-g0" => f.debug = false,
                "-shared" => f.shared = true,
                "-static" => f.stat = true,
                "-pthread" | "-pthreads" | "-w" | "-pedantic" | "-pedantic-errors" | "-pipe"
                | "-fPIC" | "-fpic" | "-fPIE" | "-fpie" | "-no-pie" | "-pie" | "-rdynamic"
                | "-Qunused-arguments" | "-s" => {}
                "-ansi" => {
                    f.std = Std {
                        year: 1989,
                        gnu: false,
                    }
                }
                "-M" | "-MM" => {
                    let d = f.deps.get_or_insert_with(Deps::default);
                    d.only = true;
                    d.system = a == "-M";
                }
                "-MD" | "-MMD" => {
                    let d = f.deps.get_or_insert_with(Deps::default);
                    d.system = a == "-MD";
                }
                "-MP" => f.deps.get_or_insert_with(Deps::default).phony = true,
                "-MF" | "-MT" | "-MQ" => {
                    let v = value(&mut i, a, a)?;
                    let d = f.deps.get_or_insert_with(Deps::default);
                    if a == "-MF" {
                        d.file = Some(v.into());
                    } else {
                        d.targets.push(v);
                    }
                }
                "-dumpmachine"
                | "-dumpversion"
                | "-dumpfullversion"
                | "-print-search-dirs"
                | "-print-libgcc-file-name"
                | "-print-multiarch" => f.dump = Some(a.to_owned()),
                "-o" => f.output = Some(value(&mut i, "-o", a)?.into()),
                "-target" => {
                    i += 1;
                    f.target = Some(
                        args.get(i)
                            .cloned()
                            .ok_or("`-target` needs a triple after it")?,
                    );
                }
                "-x" => {
                    i += 1;
                    let l = args.get(i).ok_or("`-x` needs a language after it")?;
                    if l != "c" && l != "c-header" && l != "none" {
                        return Err(format!(
                            "`-x {l}`: `nabi cc` compiles C (`-x c`) only; C++ and \
                             Objective-C sources need their own compiler"
                        ));
                    }
                    f.lang_c = l == "c";
                }
                "-include" | "-isystem" | "-iquote" | "-idirafter" | "-isysroot" | "--sysroot" => {
                    i += 1;
                    let v = args
                        .get(i)
                        .ok_or_else(|| format!("`{a}` needs a value after it"))?;
                    f.pp.push(a.to_owned());
                    f.pp.push(v.clone());
                }
                "-nostdinc" | "-nostdlibinc" | "-nobuiltininc" => f.pp.push(a.to_owned()),
                "-nostdlib" | "-nodefaultlibs" | "-nostartfiles" => f.link.push(a.to_owned()),
                "-I" | "-D" | "-U" | "-L" | "-l" => {
                    let v = value(&mut i, a, a)?;
                    if matches!(a, "-L" | "-l") {
                        f.link.push(format!("{a}{v}"));
                    } else {
                        f.pp.push(format!("{a}{v}"));
                    }
                }
                _ if a.starts_with("--target=") => {
                    f.target = Some(a["--target=".len()..].to_owned());
                }
                _ if a.starts_with("-std=") => {
                    let s = &a[5..];
                    f.std = Std::parse(s).ok_or_else(|| {
                        format!(
                            "unknown C standard `{s}`: `nabi cc` takes -std=c89, c99, c11, c17, \
                             c23 and their gnu forms"
                        )
                    })?;
                }
                _ if a.starts_with("-O") => {
                    let l = &a[2..];
                    let lv = match l {
                        "" | "1" | "g" => "1",
                        "0" => "0",
                        "2" => "2",
                        "3" | "fast" => "3",
                        "s" => "s",
                        "z" => "z",
                        _ => {
                            return Err(format!(
                                "unknown optimization level `{a}`: use -O0, -O1, -O2, -O3, -Os or -Oz"
                            ));
                        }
                    };
                    lv.clone_into(&mut f.opt);
                }
                _ if a.starts_with("-I") || a.starts_with("-D") || a.starts_with("-U") => {
                    f.pp.push(a.to_owned());
                }
                _ if a.starts_with("-isystem")
                    || a.starts_with("-iquote")
                    || a.starts_with("-idirafter")
                    || a.starts_with("--sysroot=")
                    || a.starts_with("-isysroot") =>
                {
                    f.pp.push(a.to_owned());
                }
                _ if a.starts_with("-l") || a.starts_with("-L") || a.starts_with("-Wl,") => {
                    f.link.push(a.to_owned());
                }
                _ if a.starts_with("-Wp,") => {
                    f.pp.extend(a[4..].split(',').map(str::to_owned));
                }
                // Warnings, code generation and target tuning: accepted,
                // nothing to do.
                _ if a.starts_with("-W")
                    || a.starts_with("-f")
                    || a.starts_with("-m")
                    || a.starts_with("-g")
                    || a.starts_with("--param") => {}
                _ if a.starts_with('-') && a != "-" => {
                    return Err(format!(
                        "unknown option `{a}`: `nabi cc` takes {HELP_FLAGS}"
                    ));
                }
                _ => {
                    let p = PathBuf::from(a);
                    let ext = p.extension().and_then(|e| e.to_str()).unwrap_or("");
                    match ext {
                        "c" | "h" | "i" => f.sources.push(p),
                        "s" | "S" | "asm" => {
                            return Err(format!(
                                "{a}: `nabi cc` does not assemble; write the function in C, or \
                                 link an object built elsewhere"
                            ));
                        }
                        "cc" | "cpp" | "cxx" | "C" | "m" | "mm" => {
                            return Err(format!(
                                "{a}: `nabi cc` compiles C only; C++ and Objective-C need their \
                                 own compiler"
                            ));
                        }
                        _ if f.lang_c => f.sources.push(p),
                        _ => f.link.push(a.to_owned()),
                    }
                }
            }
            i += 1;
        }
        Ok(f)
    }
}
