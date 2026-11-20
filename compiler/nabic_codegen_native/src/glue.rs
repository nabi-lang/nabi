//! The object a plugin or host link adds (`nabild::plugin`).
//!
//! A slot per import, which the module loader fills, a stub per called import
//! (a jump through its slot), the import table, a host's export table,
//! and a plugin DLL's entry. The stubs touch no register a call keeps
//! and no stack, so they sit between any caller and callee.

use crate::obj::{Obj, RelKind, Reloc, Sect};
use nabic_mir::TargetSpec;
use nabild::plugin::{DLL_ENTRY, EXPORTS, IMPORTS, Scan};
use nabir_abi::module::{EXPORT, IMPORT, sym};
use nabir_abi::typedesc::{offset_of, size_of};

/// The glue object of `s` for `t`.
pub fn object(t: &TargetSpec, s: &Scan) -> Result<Vec<u8>, String> {
    let mut o = Obj::of_target(t);
    o.i386 = t.arch == "x86";
    o.riscv = t.arch.starts_with("riscv");
    o.rv32 = t.arch == "riscv32";
    o.be = t.endian == "big";
    let ptr: u32 = if o.c32() { 4 } else { 8 };
    let mut slots = Vec::with_capacity(s.imports.len());
    for i in &s.imports {
        let slot = o.sym(&s.slot(&i.name));
        o.define_data(slot, Sect::Data, &vec![0; ptr as usize], ptr, true);
        o.syms[slot as usize].hidden = true;
        slots.push(slot);
        if i.stub {
            stub(&mut o, &t.arch, &i.name, slot)?;
        }
    }
    let isz = size_of(IMPORT, ptr);
    let mut rows = Vec::with_capacity(s.imports.len());
    for (i, &slot) in s.imports.iter().zip(&slots) {
        let kind = if i.hot {
            sym::HOT_FN
        } else if i.stub {
            sym::FN
        } else {
            sym::DATA
        };
        let name = o.bytes(i.name.as_bytes());
        let mut row = vec![0; isz as usize];
        put(
            &o,
            &mut row,
            offset_of(IMPORT, 1, ptr),
            i.name.len() as u64,
            ptr,
        );
        put(&o, &mut row, offset_of(IMPORT, 4, ptr), kind as u64, ptr);
        rows.push((
            row,
            [
                (offset_of(IMPORT, 0, ptr), name),
                (offset_of(IMPORT, 3, ptr), slot),
            ],
        ));
    }
    table(&mut o, IMPORTS, rows, ptr);
    if s.host {
        let esz = size_of(EXPORT, ptr);
        let mut rows = Vec::with_capacity(s.exports.len());
        for e in &s.exports {
            let name = o.bytes(e.name.as_bytes());
            let target = o.sym(&e.name);
            let kind = if e.code { sym::FN } else { sym::DATA };
            let mut row = vec![0; esz as usize];
            put(
                &o,
                &mut row,
                offset_of(EXPORT, 1, ptr),
                e.name.len() as u64,
                ptr,
            );
            put(&o, &mut row, offset_of(EXPORT, 6, ptr), kind as u64, ptr);
            rows.push((
                row,
                [
                    (offset_of(EXPORT, 0, ptr), name),
                    (offset_of(EXPORT, 5, ptr), target),
                ],
            ));
        }
        table(&mut o, EXPORTS, rows, ptr);
    }
    if s.coff && !s.host {
        // DllMain: TRUE, nothing to set up.
        let code: &[u8] = match t.arch.as_str() {
            "aarch64" => &[0x20, 0x00, 0x80, 0x52, 0xc0, 0x03, 0x5f, 0xd6],
            _ => &[0xb8, 0x01, 0x00, 0x00, 0x00, 0xc3],
        };
        let e = o.sym(DLL_ENTRY);
        define_code(&mut o, e, code, Vec::new(), false);
    }
    o.drop_unreferenced();
    Ok(if o.coff {
        crate::obj::coff::write(&o, None)
    } else if o.elf {
        crate::obj::elf::write(&o)
    } else {
        crate::obj::macho::write(&o)
    })
}

/// `v` in `ptr` bytes at `at` of `row`, in the target's byte order.
fn put(o: &Obj, row: &mut [u8], at: u32, v: u64, ptr: u32) {
    let at = at as usize;
    row[at..at + ptr as usize].copy_from_slice(&o.int_bytes(v, ptr as usize));
}

/// A table row's bytes and its two relocated words `(offset, symbol)`.
type Row = (Vec<u8>, [(u32, u32); 2]);

/// Define table `name`: the rows, each with two pointer words relocated
/// to symbols.
fn table(o: &mut Obj, name: &str, rows: Vec<Row>, ptr: u32) {
    let id = o.sym(name);
    let size = rows.first().map_or(0, |r| r.0.len()) as u32;
    let bytes: Vec<u8> = rows.iter().flat_map(|r| r.0.iter().copied()).collect();
    // An empty table still needs an address.
    let bytes = if bytes.is_empty() {
        vec![0; ptr as usize]
    } else {
        bytes
    };
    let base = o.define_data(id, Sect::Data, &bytes, 8, true);
    o.syms[id as usize].hidden = true;
    for (k, (_, words)) in rows.iter().enumerate() {
        for (at, target) in words {
            o.abs64(Sect::Data, base + k as u32 * size + at, *target);
        }
    }
}

/// Define `sym` as `code` in the text section with `relocs` (offsets in
/// `code`), aligned for the arch.
fn define_code(o: &mut Obj, sym: u32, code: &[u8], relocs: Vec<(u32, RelKind, u32)>, hidden: bool) {
    let align = if o.x86 || o.i386 { 16 } else { 4 };
    let text = o.sect(Sect::Text);
    let off = text.align_to(align);
    text.data.extend_from_slice(code);
    for (at, kind, target) in relocs {
        text.relocs.push(Reloc {
            off: off + at,
            kind,
            sym: target,
        });
    }
    let s = &mut o.syms[sym as usize];
    s.def = Some((Sect::Text, off, code.len() as u32));
    s.global = true;
    s.hidden = hidden;
}

/// The stub of `name`: jump to what `slot` holds.
fn stub(o: &mut Obj, arch: &str, name: &str, slot: u32) -> Result<(), String> {
    let id = o.sym(name);
    let le = |w: &[u32]| -> Vec<u8> { w.iter().flat_map(|x| x.to_le_bytes()).collect() };
    let (code, relocs) = match arch {
        // jmp [rip + slot]
        "x86_64" => (
            vec![0xff, 0x25, 0, 0, 0, 0],
            vec![(2, RelKind::X86Pc32, slot)],
        ),
        // adrp x16, slot; add x16, x16, :lo12:slot; ldr x16, [x16]; br x16
        "aarch64" => (
            le(&[0x9000_0010, 0x9100_0210, 0xf940_0210, 0xd61f_0200]),
            vec![(0, RelKind::Page21, slot), (4, RelKind::PageOff12, slot)],
        ),
        // push ebx; call 1f; 1: pop ebx; add ebx, GOT - 1b;
        // push [ebx + slot@GOTOFF]; mov ebx, [esp + 4]; ret 4
        "x86" => {
            let got = o.sym("_GLOBAL_OFFSET_TABLE_");
            let mut c = vec![0x53, 0xe8, 0, 0, 0, 0, 0x5b, 0x81, 0xc3, 3, 0, 0, 0];
            c.extend_from_slice(&[
                0xff, 0xb3, 0, 0, 0, 0, 0x8b, 0x5c, 0x24, 0x04, 0xc2, 0x04, 0x00,
            ]);
            (
                c,
                vec![
                    (9, RelKind::I386GotPc, got),
                    (15, RelKind::I386GotOff, slot),
                ],
            )
        }
        // auipc t1, %pcrel_hi(slot); ld t1, %pcrel_lo(1b)(t1); jr t1
        "riscv64" => (
            le(&[0x0000_0317, 0x0003_3303, 0x0003_0067]),
            vec![(0, RelKind::RvPcHi, slot), (4, RelKind::RvPcLoI, slot)],
        ),
        a => return Err(format!("plugins are not supported on {a}")),
    };
    define_code(o, id, &code, relocs, true);
    Ok(())
}

// The tests dlopen what they link: not in a static musl test binary.
#[cfg(all(
    test,
    any(
        target_os = "macos",
        all(target_os = "linux", not(target_env = "musl")),
        windows
    ),
    any(target_arch = "aarch64", target_arch = "x86_64")
))]
#[allow(clippy::cast_ptr_alignment)] // the loader's symbols are aligned
mod tests {
    use super::*;
    use nabild::plugin::{MODULE, Plugin};
    use nabir_abi::module::{Export, Import, MAGIC, MODULE_DESC, ModuleDesc, PLUGIN_SYMBOL};

    #[cfg(not(windows))]
    unsafe extern "C" {
        fn dlopen(path: *const u8, mode: i32) -> *mut u8;
        fn dlsym(h: *mut u8, name: *const u8) -> *mut u8;
        fn dlerror() -> *const u8;
    }
    #[cfg(windows)]
    unsafe extern "system" {
        fn LoadLibraryA(path: *const u8) -> *mut u8;
        fn GetProcAddress(h: *mut u8, name: *const u8) -> *mut u8;
    }

    #[allow(clippy::missing_const_for_fn)]
    extern "C" fn inc(x: i64) -> i64 {
        x + 1
    }

    static COUNTER: i64 = 41;

    /// A plugin object as the compiler emits one: `_Yp_call` tail-calls
    /// the process's `nabi_test_inc`, `_Yp_load` reads the process's
    /// `_Yh_counter` through the GOT (COFF: `__imp_`), and the export
    /// table lists both.
    /// `hot`: `_Yp_call` calls the module's own `_Yp_inc` instead, and the
    /// export table names its body `_Yp_inc.hot` (a hot build).
    fn plugin_object(t: &TargetSpec, hot: bool) -> Vec<u8> {
        let coff = t.os == "windows";
        let x86 = t.arch == "x86_64";
        let mut o = Obj::new(x86, !coff && t.os != "macos", coff);
        let le = |w: &[u32]| -> Vec<u8> { w.iter().flat_map(|x| x.to_le_bytes()).collect() };
        let callee = o.sym(if hot { "_Yp_inc" } else { "nabi_test_inc" });
        let mut listed = Vec::new();
        if hot {
            let body = match (x86, coff) {
                (true, false) => vec![0x48, 0x8d, 0x47, 0x01, 0xc3],
                (true, true) => vec![0x48, 0x8d, 0x41, 0x01, 0xc3],
                _ => le(&[0x9100_0400, 0xd65f_03c0]),
            };
            define_code(&mut o, callee, &body, Vec::new(), false);
            listed.push(o.sym("_Yp_inc.hot"));
        }
        let data = o.sym(if coff {
            "__imp__Yh_counter"
        } else {
            "_Yh_counter"
        });
        let call = o.sym("_Yp_call");
        let load = o.sym("_Yp_load");
        let (c1, r1, c2, r2) = if x86 {
            let kind = if coff {
                RelKind::X86Pc32
            } else {
                RelKind::X86GotLoad
            };
            (
                vec![0xe9, 0, 0, 0, 0],
                vec![(1, RelKind::X86Call, callee)],
                vec![0x48, 0x8b, 0x05, 0, 0, 0, 0, 0x48, 0x8b, 0x00, 0xc3],
                vec![(3, kind, data)],
            )
        } else {
            let (hi, lo) = if coff {
                (RelKind::Page21, RelKind::PageOff12)
            } else {
                (RelKind::GotPage21, RelKind::GotPageOff12)
            };
            (
                le(&[0x1400_0000]),
                vec![(0, RelKind::Branch26, callee)],
                le(&[0x9000_0000, 0xf940_0000, 0xf940_0000, 0xd65f_03c0]),
                vec![(0, hi, data), (4, lo, data)],
            )
        };
        define_code(&mut o, call, &c1, r1, false);
        define_code(&mut o, load, &c2, r2, false);
        // The exports, then the descriptor.
        let mut listed_all = vec![call, load];
        listed_all.extend(listed);
        let n = listed_all.len();
        let esz = size_of(EXPORT, 8);
        let exp = o.sym("nabi.test.exports");
        let base = o.define_data(exp, Sect::DataConst, &vec![0; n * esz as usize], 8, false);
        for (k, f) in listed_all.into_iter().enumerate() {
            o.abs64(
                Sect::DataConst,
                base + k as u32 * esz + offset_of(EXPORT, 5, 8),
                f,
            );
        }
        let mut d = vec![0; size_of(MODULE_DESC, 8) as usize];
        d[..4].copy_from_slice(&MAGIC.to_le_bytes());
        d[offset_of(MODULE_DESC, 19, 8) as usize] = n as u8;
        let m = o.sym(MODULE);
        let base = o.define_data(m, Sect::DataConst, &d, 8, true);
        let imports = o.sym(IMPORTS);
        o.abs64(
            Sect::DataConst,
            base + offset_of(MODULE_DESC, 16, 8),
            imports,
        );
        o.abs64(Sect::DataConst, base + offset_of(MODULE_DESC, 18, 8), exp);
        if coff {
            crate::obj::coff::write(&o, None)
        } else if o.elf {
            crate::obj::elf::write(&o)
        } else {
            crate::obj::macho::write(&o)
        }
    }

    fn link(
        t: &TargetSpec,
        obj: &std::path::Path,
        out: &std::path::Path,
        hot: bool,
    ) -> Result<String, String> {
        let p = Plugin {
            exports: &[],
            import_own: hot,
            host: false,
            hot_deps: &[],
        };
        let glue = |s: &Scan| object(t, s);
        let exports = [MODULE.to_owned()];
        if t.object_format == nabic_mir::ObjectFormat::MachO {
            let sh = nabild::Shared {
                exports: &exports,
                init: "",
                anchors: &[],
                implib: None,
                soname: None,
                versions: None,
            };
            return nabild::link_plugin(t, &[obj], out, None, &sh, &p, &glue);
        }
        let mut a: Vec<std::ffi::OsString> = Vec::new();
        if t.os == "windows" {
            for x in [
                "/nologo",
                if t.arch == "x86_64" {
                    "/machine:x64"
                } else {
                    "/machine:arm64"
                },
                "/nodefaultlib",
                "/subsystem:windows",
                "/dll",
                "/entry:nabi.dll_entry",
                "/export:nabi_module",
            ] {
                a.push(x.into());
            }
            let mut o = std::ffi::OsString::from("/out:");
            o.push(out);
            a.push(o);
            a.push(obj.into());
            let o = nabild::coff::parse_args(&a)?;
            nabild::coff::link_with(&o, Some((&p, &glue)))?;
        } else {
            let m = if t.arch == "x86_64" {
                "elf_x86_64"
            } else {
                "aarch64linux"
            };
            for x in [
                "-m",
                m,
                "-shared",
                "--hash-style=gnu",
                "--nabi-export=nabi_module",
                "-o",
            ] {
                a.push(x.into());
            }
            a.push(out.into());
            a.push(obj.into());
            let o = nabild::elf::parse_args(&a)?;
            nabild::elf::link_with(&o, Some((&p, &glue)))?;
        }
        Ok(String::new())
    }

    #[test]
    fn a_plugin_reaches_the_process_through_its_slots() {
        reach(false);
    }

    /// A hot build calls its own function through a slot too.
    // COFF: the module's own `X` is left undefined there.
    #[test]
    #[cfg_attr(windows, ignore)]
    fn a_hot_plugin_calls_itself_through_a_slot() {
        reach(true);
    }

    fn reach(hot: bool) {
        let t = TargetSpec::host();
        let dir = std::env::temp_dir().join(format!("nabi-glue-{}-{hot}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let obj = dir.join("p.o");
        std::fs::write(&obj, plugin_object(t, hot)).unwrap();
        let ext = match t.os.as_str() {
            "macos" => "dylib",
            "windows" => "dll",
            _ => "so",
        };
        let out = dir.join(format!("libp.{ext}"));
        link(t, &obj, &out, hot).unwrap();
        let mut path = out.to_string_lossy().into_owned().into_bytes();
        path.push(0);
        let sym = format!("{PLUGIN_SYMBOL}\0");
        // SAFETY: a library this test just linked; its descriptor and
        // tables have the layouts of `nabir_abi`.
        unsafe {
            #[cfg(not(windows))]
            let m = {
                let h = dlopen(path.as_ptr(), 2);
                assert!(
                    !h.is_null(),
                    "{:?}",
                    std::ffi::CStr::from_ptr(dlerror().cast())
                );
                dlsym(h, sym.as_ptr()).cast::<ModuleDesc>()
            };
            #[cfg(windows)]
            let m = {
                let h = LoadLibraryA(path.as_ptr());
                assert!(!h.is_null());
                GetProcAddress(h, sym.as_ptr()).cast::<ModuleDesc>()
            };
            assert!(!m.is_null());
            let m = &*m;
            assert_eq!(m.magic, MAGIC);
            let imports: &[Import] = std::slice::from_raw_parts(m.imports.ptr, m.imports.len);
            let names: Vec<(&[u8], usize)> =
                imports.iter().map(|i| (i.sym.bytes(), i.kind)).collect();
            let callee: (&[u8], usize) = if hot {
                (b"_Yp_inc", sym::HOT_FN)
            } else {
                (b"nabi_test_inc", sym::FN)
            };
            assert_eq!(names, [(&b"_Yh_counter"[..], sym::DATA), callee]);
            let exports: &[Export] = std::slice::from_raw_parts(m.exports.ptr, m.exports.len);
            *imports[0].slot = (&raw const COUNTER) as usize;
            *imports[1].slot = if hot { exports[2].addr } else { inc as usize };
            let call: extern "C" fn(i64) -> i64 = std::mem::transmute(exports[0].addr);
            let load: extern "C" fn() -> i64 = std::mem::transmute(exports[1].addr);
            assert_eq!(call(6), 7);
            assert_eq!(load(), 41);
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A host's export table lists its Nabi symbols (the runtime member
    /// nothing referenced included), not the hidden copies.
    #[test]
    fn a_host_lists_its_symbols() {
        let t = TargetSpec::host();
        let coff = t.os == "windows";
        let ret: &[u8] = if t.arch == "x86_64" {
            &[0xb8, 0x01, 0, 0, 0, 0xc3]
        } else {
            &[0x20, 0x00, 0x80, 0x52, 0xc0, 0x03, 0x5f, 0xd6]
        };
        let new = || Obj::new(t.arch == "x86_64", !coff && t.os != "macos", coff);
        let write = |o: &Obj| {
            if coff {
                crate::obj::coff::write(o, None)
            } else if o.elf {
                crate::obj::elf::write(o)
            } else {
                crate::obj::macho::write(o)
            }
        };
        let mut o = new();
        for n in ["nabi_test_entry", "_Yh_f", "_Yh_f.copy_Yab_"] {
            let f = o.sym(n);
            define_code(&mut o, f, ret, Vec::new(), false);
        }
        let d = o.sym("_Yh_data");
        o.define_data(d, Sect::Data, &[7; 8], 8, true);
        let mut desc = vec![0; size_of(MODULE_DESC, 8) as usize];
        desc[..4].copy_from_slice(&MAGIC.to_le_bytes());
        let m = o.sym("nabi_host_module");
        let base = o.define_data(m, Sect::DataConst, &desc, 8, true);
        let e = o.sym(EXPORTS);
        o.abs64(Sect::DataConst, base + offset_of(MODULE_DESC, 18, 8), e);
        let mut lib = new();
        let r = lib.sym("nabi_test_extra");
        define_code(&mut lib, r, ret, Vec::new(), false);
        let dir = std::env::temp_dir().join(format!("nabi-host-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (obj, member, ar) = (dir.join("h.o"), dir.join("r.o"), dir.join("librt.a"));
        std::fs::write(&obj, write(&o)).unwrap();
        std::fs::write(&member, write(&lib)).unwrap();
        nabild::write_archive(t, &ar, &[&member]).unwrap();
        let out = dir.join(if coff { "libh.dll" } else { "libh" });
        let p = Plugin {
            exports: &[],
            import_own: false,
            host: true,
            hot_deps: &[],
        };
        let glue = |s: &Scan| object(t, s);
        let mut a: Vec<std::ffi::OsString> = Vec::new();
        let push =
            |a: &mut Vec<std::ffi::OsString>, xs: &[&str]| a.extend(xs.iter().map(Into::into));
        match t.os.as_str() {
            "macos" => {
                push(
                    &mut a,
                    &[
                        "-arch",
                        "arm64",
                        "-dylib",
                        "-platform_version",
                        "macos",
                        "11.0",
                        "11.0",
                    ],
                );
                push(&mut a, &["-exported_symbol", "_nabi_host_module", "-o"]);
                a.push(out.clone().into());
                a.push(obj.into());
                a.push(ar.into());
                let o = nabild::macho::parse_args(&a).unwrap();
                nabild::macho::link_with(&o, Some((&p, &glue))).unwrap();
            }
            "windows" => {
                push(
                    &mut a,
                    &[
                        "/nologo",
                        if t.arch == "x86_64" {
                            "/machine:x64"
                        } else {
                            "/machine:arm64"
                        },
                        "/nodefaultlib",
                        "/subsystem:windows",
                        "/dll",
                        "/entry:nabi_test_entry",
                    ],
                );
                push(&mut a, &["/export:nabi_host_module"]);
                let mut x = std::ffi::OsString::from("/out:");
                x.push(&out);
                a.push(x);
                a.push(obj.into());
                a.push(ar.into());
                let o = nabild::coff::parse_args(&a).unwrap();
                nabild::coff::link_with(&o, Some((&p, &glue))).unwrap();
            }
            _ => {
                let m = if t.arch == "x86_64" {
                    "elf_x86_64"
                } else {
                    "aarch64linux"
                };
                push(
                    &mut a,
                    &["-m", m, "-shared", "--nabi-export=nabi_host_module", "-o"],
                );
                a.push(out.clone().into());
                a.push(obj.into());
                a.push(ar.into());
                let o = nabild::elf::parse_args(&a).unwrap();
                nabild::elf::link_with(&o, Some((&p, &glue))).unwrap();
            }
        }
        let mut path = out.to_string_lossy().into_owned().into_bytes();
        path.push(0);
        // SAFETY: as above.
        unsafe {
            #[cfg(not(windows))]
            let m = {
                let h = dlopen(path.as_ptr(), 2);
                assert!(
                    !h.is_null(),
                    "{:?}",
                    std::ffi::CStr::from_ptr(dlerror().cast())
                );
                dlsym(h, c"nabi_host_module".as_ptr().cast()).cast::<ModuleDesc>()
            };
            #[cfg(windows)]
            let m = {
                let h = LoadLibraryA(path.as_ptr());
                assert!(!h.is_null());
                GetProcAddress(h, c"nabi_host_module".as_ptr().cast()).cast::<ModuleDesc>()
            };
            let m = &*m;
            assert_eq!(m.imports.len, 0);
            let exports: &[Export] = std::slice::from_raw_parts(m.exports.ptr, m.exports.len);
            let names: Vec<(&[u8], usize)> =
                exports.iter().map(|e| (e.sym.bytes(), e.kind)).collect();
            assert_eq!(
                names,
                [
                    (&b"_Yh_data"[..], sym::DATA),
                    (&b"_Yh_f"[..], sym::FN),
                    (&b"nabi_test_entry"[..], sym::FN),
                    (&b"nabi_test_extra"[..], sym::FN),
                ]
            );
            assert_eq!(*(exports[0].addr as *const u8), 7);
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
