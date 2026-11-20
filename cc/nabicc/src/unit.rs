//! One translation unit: preprocessed, parsed, checked, lowered to MIR,
//! optimized and compiled to an object by the native backend.

use crate::flags::{Flags, Mode};
use crate::pp::{Error, Pp};
use crate::target::{Target, TargetInfo};
use crate::{headers, parse, sysinc};
use nabic_target::TargetSpec;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

/// What a unit compiles to.
pub struct Output {
    /// The object (`-c`), the assembly text (`-S`) or the preprocessed
    /// source (`-E`).
    pub bytes: Vec<u8>,
    /// Every file read, for `-M*`: `(path, system)`.
    pub files: Vec<(PathBuf, bool)>,
}

/// The C view of target `spec`.
pub fn target_info(spec: &TargetSpec, triple: &str) -> Result<Target, String> {
    Target::new(&TargetInfo {
        triple,
        arch: &spec.arch,
        os: &spec.os,
        env: &spec.env,
        vendor: &spec.vendor,
        pointer_width: spec.pointer_width,
        big_endian: spec.endian == "big",
    })
}

/// The macros `nabi cc` defines past the target's: the standard, and no
/// vector extensions (it has no intrinsics), no `__int128`.
fn defines(f: &Flags, t: &Target) -> String {
    let mut s = String::new();
    s.push_str("#define __nabicc__ 1\n");
    s.push_str("#undef __STDC_VERSION__\n");
    if let Some(v) = f.std.version() {
        let _ = writeln!(s, "#define __STDC_VERSION__ {v}");
    }
    if !f.std.gnu {
        s.push_str("#define __STRICT_ANSI__ 1\n");
    }
    for m in [
        "__SIZEOF_INT128__",
        "__MMX__",
        "__SSE__",
        "__SSE2__",
        "__SSE3__",
        "__SSSE3__",
        "__SSE4_1__",
        "__SSE4_2__",
        "__AVX__",
        "__AVX2__",
        "__AVX512F__",
        "__F16C__",
        "__FMA__",
        "__AES__",
        "__PCLMUL__",
        "__SHA__",
        "__POPCNT__",
        "__LZCNT__",
        "__BMI__",
        "__BMI2__",
        "__ARM_NEON",
        "__ARM_NEON__",
        "__ARM_NEON_FP",
        "__ARM_FEATURE_SVE",
        "__ARM_FEATURE_CRC32",
        "__ARM_FEATURE_CRYPTO",
        "__ARM_FEATURE_AES",
        "__ARM_FEATURE_SHA2",
        "__ARM_FEATURE_SHA3",
        "__ARM_FEATURE_SM4",
        "__ARM_FEATURE_DOTPROD",
        "__ARM_FEATURE_FP16_VECTOR_ARITHMETIC",
        "__ARM_FEATURE_FP16_SCALAR_ARITHMETIC",
        "__ARM_FEATURE_BF16",
        "__ARM_FEATURE_MATMUL_INT8",
        "__wasm_simd128__",
        "__riscv_vector",
        "__VEC__",
        "__ALTIVEC__",
        "__VSX__",
        "__VX__",
        "__ARM_FP16_FORMAT_IEEE",
        "__FLT16_MAX__",
    ] {
        let _ = writeln!(s, "#undef {m}");
    }
    let _ = t;
    s
}

/// Compile `path` as `f` and `spec` say.
pub fn compile(path: &Path, f: &Flags, spec: &TargetSpec) -> Result<Output, String> {
    let triple = spec.c_triple();
    let t = target_info(spec, &triple)?;
    let mut opts = sysinc::Opts::parse(&f.pp).map_err(|e| e.replace("bindgen", "`nabi cc`"))?;
    let (host, sdk) = if opts.sysroot.is_some() || opts.no_std_inc || opts.no_stdlib_inc {
        (false, None)
    } else {
        let (host, sdk, sysroot) = headers::system(spec)?;
        opts.sysroot = sysroot;
        (host, sdk)
    };
    let dirs = sysinc::dirs(&t, &opts, host, sdk.as_deref());
    let mut pp = Pp::new(&t, dirs);
    let pre = t.predefines() + &defines(f, &t) + &opts.defines;
    pp.run_text(&pre).map_err(|e| e.0)?;
    pp.enter_main(path, &opts.include).map_err(|e| e.0)?;
    if f.mode == Mode::Preprocess || f.deps.as_ref().is_some_and(|d| d.only) {
        let text = preprocess(&mut pp).map_err(|e| e.0)?;
        return Ok(Output {
            bytes: text.into_bytes(),
            files: files_of(&pp),
        });
    }
    let types = parse::ty::Types::new(t.clone());
    let mut p = parse::Parser::new(&mut pp, types);
    p.bodies_on = true;
    p.unit().map_err(|e: Error| e.0)?;
    let unit = nabicc_sema::check(&mut p).map_err(|e| e.0)?;
    drop(p);
    let mut sources = nabic_diagnostic::SourceMap::default();
    let lopts = nabicc_lower::Options {
        target: spec,
        files: &pp.files,
        debug: f.debug,
    };
    let mut mir = nabicc_lower::lower(&unit, &lopts, &mut sources).map_err(|e| e.0)?;
    let level = nabic_codegen_native::OptLevel::parse(&f.opt).unwrap_or_default();
    let opt = match f.opt.as_str() {
        "1" => Some(nabic_mir::Opt::Speed(1)),
        "2" => Some(nabic_mir::Opt::Speed(2)),
        "3" => Some(nabic_mir::Opt::Speed(3)),
        "s" | "z" => Some(nabic_mir::Opt::Size),
        _ => None,
    };
    if let Some(o) = opt {
        nabic_mir::optimize(&mut mir, o);
    }
    let copts = nabic_codegen_native::Options {
        opt_level: level,
        overflow_checks: false,
        debug: f.debug,
        repl: None,
        fresh_fns: None,
        asm: (f.mode == Mode::Asm).then_some(nabic_codegen_native::Text::Source),
    };
    let out = nabic_codegen_native::compile(&mir, &copts, &sources).map_err(|d| d.message)?;
    let bytes = if f.mode == Mode::Asm {
        out.asm.unwrap_or_default().into_bytes()
    } else {
        out.object
    };
    Ok(Output {
        bytes,
        files: files_of(&pp),
    })
}

fn files_of(pp: &Pp<'_>) -> Vec<(PathBuf, bool)> {
    pp.files
        .iter()
        .filter(|f| !f.builtin)
        .map(|f| (f.path.clone(), f.system))
        .collect()
}

/// The preprocessed tokens as text, a line marker where the file or
/// line jumps.
fn preprocess(pp: &mut Pp<'_>) -> Result<String, Error> {
    let mut out = String::new();
    let (mut file, mut line) = (u32::MAX, 0u32);
    loop {
        let t = pp.next()?;
        if t.kind == crate::lex::Kind::Eof {
            break;
        }
        if t.kind == crate::lex::Kind::Pragma {
            continue;
        }
        if t.loc.file != file || t.loc.line < line || t.loc.line > line + 8 {
            if !out.is_empty() {
                out.push('\n');
            }
            let path = pp
                .files
                .get(t.loc.file as usize)
                .map(|f| f.path.display().to_string())
                .unwrap_or_default();
            let _ = writeln!(out, "# {} \"{path}\"", t.loc.line);
            file = t.loc.file;
            line = t.loc.line;
        } else {
            while line < t.loc.line {
                out.push('\n');
                line += 1;
            }
            if t.has(crate::lex::SPACE) {
                out.push(' ');
            }
        }
        out.push_str(pp.names.get(t.sym));
    }
    out.push('\n');
    Ok(out)
}
