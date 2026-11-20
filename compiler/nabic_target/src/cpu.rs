//! CPU names and features, in clang's names.
//!
//! What `-C target-cpu` and `-C target-feature` turn on for the code
//! generator: a CPU name stands for its features; `+f` turns `f` on with
//! what it needs, `-f` turns it off with what needs it.

#[cfg(target_arch = "aarch64")]
use std::arch::is_aarch64_feature_detected as has;
#[cfg(target_arch = "x86_64")]
use std::arch::is_x86_feature_detected as has;

/// Features of one architecture, each with the ones it needs.
type Table = &'static [(&'static str, &'static [&'static str])];

/// CPU names of one architecture, each with its features.
type Cpus = &'static [(&'static str, &'static [&'static str])];

const X86: Table = &[
    ("sse2", &[]),
    ("sse3", &["sse2"]),
    ("ssse3", &["sse3"]),
    ("sse4.1", &["ssse3"]),
    ("sse4.2", &["sse4.1"]),
    ("popcnt", &[]),
    ("avx", &["sse4.2"]),
    ("avx2", &["avx"]),
    ("fma", &["avx"]),
    ("f16c", &["avx"]),
    ("bmi", &[]),
    ("bmi2", &[]),
    ("lzcnt", &[]),
    ("movbe", &[]),
    ("avx512f", &["avx2", "fma", "f16c"]),
    ("avx512bw", &["avx512f"]),
    ("avx512cd", &["avx512f"]),
    ("avx512dq", &["avx512f"]),
    ("avx512vl", &["avx512f"]),
];

const V2: &[&str] = &["sse4.2", "popcnt"];
const V3: &[&str] = &[
    "sse4.2", "popcnt", "avx2", "fma", "f16c", "bmi", "bmi2", "lzcnt", "movbe",
];
const V4: &[&str] = &[
    "sse4.2", "popcnt", "avx2", "fma", "f16c", "bmi", "bmi2", "lzcnt", "movbe", "avx512f",
    "avx512bw", "avx512cd", "avx512dq", "avx512vl",
];

const X86_CPUS: Cpus = &[
    ("x86-64", &["sse2"]),
    ("x86-64-v2", V2),
    ("x86-64-v3", V3),
    ("x86-64-v4", V4),
    ("pentium4", &["sse2"]),
    ("core2", &["ssse3"]),
    ("nehalem", V2),
    ("westmere", V2),
    ("sandybridge", &["sse4.2", "popcnt", "avx"]),
    ("ivybridge", &["sse4.2", "popcnt", "avx", "f16c"]),
    ("haswell", V3),
    ("broadwell", V3),
    ("skylake", V3),
    ("alderlake", V3),
    ("skylake-avx512", V4),
    ("cascadelake", V4),
    ("icelake-client", V4),
    ("icelake-server", V4),
    ("tigerlake", V4),
    ("sapphirerapids", V4),
    ("znver1", V3),
    ("znver2", V3),
    ("znver3", V3),
    ("znver4", V4),
];

const ARM64: Table = &[
    ("neon", &[]),
    ("fp-armv8", &[]),
    ("crc", &[]),
    ("lse", &[]),
    ("rcpc", &[]),
    ("rdm", &["neon"]),
    ("dotprod", &["neon"]),
    ("fullfp16", &["fp-armv8"]),
    ("aes", &["neon"]),
    ("sha2", &["neon"]),
    ("sha3", &["sha2"]),
    ("sve", &["fullfp16"]),
    ("sve2", &["sve"]),
];

const A76: &[&str] = &[
    "neon", "fp-armv8", "crc", "lse", "rcpc", "rdm", "dotprod", "fullfp16",
];
const APPLE: &[&str] = &[
    "neon", "fp-armv8", "crc", "lse", "rcpc", "rdm", "dotprod", "fullfp16", "aes", "sha2", "sha3",
];

const ARM64_CPUS: Cpus = &[
    ("generic", &["neon", "fp-armv8"]),
    ("cortex-a53", &["neon", "fp-armv8", "crc"]),
    ("cortex-a72", &["neon", "fp-armv8", "crc"]),
    ("cortex-a76", A76),
    ("neoverse-n1", A76),
    (
        "neoverse-v1",
        &[
            "neon", "fp-armv8", "crc", "lse", "rcpc", "rdm", "dotprod", "fullfp16", "sve",
        ],
    ),
    (
        "neoverse-n2",
        &[
            "neon", "fp-armv8", "crc", "lse", "rcpc", "rdm", "dotprod", "fullfp16", "sve2",
        ],
    ),
    // The iOS device baseline: ARMv8.0, no LSE.
    ("apple-a7", &["neon", "fp-armv8", "aes", "sha2"]),
    ("apple-m1", APPLE),
    ("apple-m2", APPLE),
    ("apple-m3", APPLE),
    ("apple-m4", APPLE),
];

const ARM: Table = &[
    ("vfp3", &[]),
    ("d32", &["vfp3"]),
    ("neon", &["d32"]),
    ("vfp4", &["vfp3"]),
    ("read-tp-tpidruro", &[]),
];

const PPC: Table = &[
    ("altivec", &[]),
    ("vsx", &["altivec"]),
    ("power8-vector", &["vsx"]),
    ("power9-vector", &["power8-vector"]),
];

const PPC_CPUS: Cpus = &[
    ("ppc64le", &["power8-vector"]),
    ("pwr8", &["power8-vector"]),
    ("pwr9", &["power9-vector"]),
    ("pwr10", &["power9-vector"]),
];

const S390X: Table = &[("vector", &[]), ("vector-enhancements-1", &["vector"])];

const S390X_CPUS: Cpus = &[
    ("z10", &[]),
    ("z196", &[]),
    ("zEC12", &[]),
    ("arch10", &[]),
    ("z13", &["vector"]),
    ("arch11", &["vector"]),
    ("z14", &["vector-enhancements-1"]),
    ("arch12", &["vector-enhancements-1"]),
    ("z15", &["vector-enhancements-1"]),
    ("z16", &["vector-enhancements-1"]),
];

const RISCV: Table = &[
    ("m", &[]),
    ("a", &[]),
    ("f", &[]),
    ("d", &["f"]),
    ("c", &[]),
    ("v", &["d"]),
    ("zbb", &[]),
];

const WASM: Table = &[
    ("simd128", &[]),
    ("atomics", &[]),
    ("bulk-memory", &[]),
    ("mutable-globals", &[]),
];

/// The features and CPU names of architecture `arch`.
fn tables(arch: &str) -> Option<(Table, Cpus)> {
    Some(match arch {
        "x86_64" | "x86" => (X86, X86_CPUS),
        "aarch64" => (ARM64, ARM64_CPUS),
        "arm" => (ARM, &[]),
        "powerpc64" => (PPC, PPC_CPUS),
        "s390x" => (S390X, S390X_CPUS),
        "riscv64" | "riscv32" => (RISCV, &[]),
        "wasm32" => (WASM, &[]),
        _ => return None,
    })
}

/// The features the native code generators use, by architecture: the
/// others change no code.
#[must_use]
pub fn used(arch: &str) -> &'static [&'static str] {
    match arch {
        "x86_64" => &["avx2", "popcnt", "lzcnt", "bmi", "f16c", "fma"],
        "aarch64" => &["lse", "rcpc", "dotprod"],
        "arm" => &["neon", "vfp4"],
        "powerpc64" => &["vsx"],
        "wasm32" => &["simd128"],
        "riscv64" => &["zbb", "v"],
        "riscv32" => &["zbb"],
        "s390x" => &["vector", "vector-enhancements-1"],
        _ => &[],
    }
}

/// Whether two feature sets differ in what the code generators of
/// `arch` use.
#[must_use]
pub fn same_code(arch: &str, a: &Features, b: &Features) -> bool {
    used(arch).iter().all(|f| a.has(f) == b.has(f))
}

/// The features on.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Features(Vec<&'static str>);

impl Features {
    #[must_use]
    pub fn has(&self, f: &str) -> bool {
        self.0.iter().any(|x| *x == f)
    }

    fn on(&mut self, t: Table, f: &str) {
        let Some((name, needs)) = t.iter().find(|(n, _)| *n == f) else {
            return;
        };
        if !self.has(name) {
            self.0.push(name);
        }
        for n in *needs {
            self.on(t, n);
        }
    }

    fn off(&mut self, t: Table, f: &str) {
        self.0.retain(|x| *x != f);
        for (name, needs) in t {
            if needs.contains(&f) && self.has(name) {
                self.off(t, name);
            }
        }
    }
}

/// `bmi1` is clang's `bmi`.
fn canon(f: &str) -> &str {
    if f == "bmi1" { "bmi" } else { f }
}

/// The features of `cpu` on `arch` with `features` (`+f,-g,..`) over
/// them. Unknown names are left out.
#[must_use]
pub fn features(arch: &str, cpu: &str, features: &str) -> Features {
    let mut out = Features::default();
    let Some((t, cpus)) = tables(arch) else {
        return out;
    };
    if let Some((_, fs)) = cpus.iter().find(|(n, _)| *n == cpu) {
        for f in *fs {
            out.on(t, f);
        }
    }
    for f in features.split(',').map(str::trim) {
        if let Some(f) = f.strip_prefix('+') {
            out.on(t, canon(f));
        } else if let Some(f) = f.strip_prefix('-') {
            out.off(t, canon(f));
        }
    }
    out
}

/// Whether `cpu` names a CPU of `arch` (any name where none are known).
pub fn known_cpu(arch: &str, cpu: &str) -> Result<(), String> {
    let Some((_, cpus)) = tables(arch).filter(|(_, c)| !c.is_empty()) else {
        return Ok(());
    };
    if cpus.iter().any(|(n, _)| *n == cpu) {
        return Ok(());
    }
    let names: Vec<&str> = cpus.iter().map(|(n, _)| *n).collect();
    let near = nabic_core::suggest::suggest(cpu, names.iter().copied())
        .map(|s| format!("did you mean `{s}`? "))
        .unwrap_or_default();
    Err(format!(
        "-C target-cpu: unknown {arch} CPU `{cpu}`; {near}the known ones: {}, or `native`",
        names.join(", ")
    ))
}

/// Whether feature `f` is one `arch` knows.
#[must_use]
pub fn known_feature(arch: &str, f: &str) -> bool {
    tables(arch).is_some_and(|(t, _)| t.iter().any(|(n, _)| *n == canon(f)))
}

/// The features of the machine nabic runs on, as `+f` words, if it is
/// `arch` (else none).
#[must_use]
pub fn host(arch: &str) -> Vec<String> {
    let all: Vec<(&str, bool)> = match arch {
        #[cfg(target_arch = "x86_64")]
        "x86_64" | "x86" => vec![
            ("sse3", has!("sse3")),
            ("ssse3", has!("ssse3")),
            ("sse4.1", has!("sse4.1")),
            ("sse4.2", has!("sse4.2")),
            ("popcnt", has!("popcnt")),
            ("avx", has!("avx")),
            ("avx2", has!("avx2")),
            ("fma", has!("fma")),
            ("f16c", has!("f16c")),
            ("bmi", has!("bmi1")),
            ("bmi2", has!("bmi2")),
            ("lzcnt", has!("lzcnt")),
            ("movbe", has!("movbe")),
            ("avx512f", has!("avx512f")),
            ("avx512bw", has!("avx512bw")),
            ("avx512cd", has!("avx512cd")),
            ("avx512dq", has!("avx512dq")),
            ("avx512vl", has!("avx512vl")),
        ],
        #[cfg(target_arch = "aarch64")]
        "aarch64" => vec![
            ("neon", has!("neon")),
            ("crc", has!("crc")),
            ("lse", has!("lse")),
            ("rcpc", has!("rcpc")),
            ("rdm", has!("rdm")),
            ("dotprod", has!("dotprod")),
            ("fullfp16", has!("fp16")),
            ("aes", has!("aes")),
            ("sha2", has!("sha2")),
            ("sha3", has!("sha3")),
            ("sve", has!("sve")),
            ("sve2", has!("sve2")),
        ],
        _ => Vec::new(),
    };
    all.into_iter()
        .filter(|x| x.1)
        .map(|(f, _)| format!("+{f}"))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cpus_and_features() {
        let f = features("x86_64", "x86-64", "");
        assert!(f.has("sse2") && !f.has("avx2"));
        let f = features("x86_64", "x86-64", "+avx2");
        assert!(f.has("avx2") && f.has("avx") && f.has("sse4.1"));
        let f = features("x86_64", "haswell", "-avx");
        assert!(!f.has("avx2") && !f.has("fma") && f.has("bmi") && f.has("sse4.2"));
        let f = features("x86_64", "x86-64", "+bmi1");
        assert!(f.has("bmi"));
        let f = features("aarch64", "apple-m1", "");
        assert!(f.has("lse") && f.has("neon") && f.has("rcpc") && f.has("dotprod"));
        assert!(!features("aarch64", "cortex-a72", "").has("rcpc"));
        assert!(features("riscv64", "generic-rv64", "+v").has("d"));
        let f = features("s390x", "z10", "+vector");
        assert!(f.has("vector"));
        assert!(!features("s390x", "z10", "").has("vector"));
        assert!(features("powerpc64", "ppc64le", "").has("vsx"));
        assert!(known_cpu("x86_64", "znver3").is_ok());
        let e = known_cpu("x86_64", "haswel").unwrap_err();
        assert!(e.contains("did you mean `haswell`?"), "{e}");
        assert!(known_cpu("riscv64", "anything").is_ok());
        assert!(known_feature("x86_64", "avx2") && !known_feature("x86_64", "avx9"));
    }
}
