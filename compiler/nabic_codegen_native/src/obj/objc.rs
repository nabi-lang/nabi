//! Mach-O Objective-C references: the selector and class references a
//! function loads (`lower::objc`), defined at the end of the object as
//! clang does, with the image info that makes the runtime fix them up.

use crate::lir::SymId;
use crate::obj::{Obj, Sect};

const S_CSTRING_LITERALS: u32 = 0x2;
const S_LITERAL_POINTERS: u32 = 0x5;
const S_ATTR_NO_DEAD_STRIP: u32 = 0x1000_0000;

const SELREF: &str = "nabi.objc.selref:";
const CLASSREF: &str = "nabi.objc.classref:";
const METHNAME: &str = "nabi.objc.methname:";

/// One of the sections the Objective-C runtime reads by name.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum Part {
    /// `__TEXT,__objc_methname`: the selector names.
    Methname,
    /// `__DATA,__objc_selrefs`: a pointer to each name, which the
    /// runtime replaces with the registered selector at load.
    Selrefs,
    /// `__DATA,__objc_classrefs`: a pointer to each class, bound by dyld.
    Classrefs,
    /// `__DATA,__objc_imageinfo`: present once per image that uses them.
    Imageinfo,
}

impl Part {
    /// Segment, section and flags.
    #[must_use]
    pub const fn section(self) -> (&'static str, &'static str, u32) {
        match self {
            Self::Methname => ("__TEXT", "__objc_methname", S_CSTRING_LITERALS),
            Self::Selrefs => (
                "__DATA",
                "__objc_selrefs",
                S_LITERAL_POINTERS | S_ATTR_NO_DEAD_STRIP,
            ),
            Self::Classrefs => ("__DATA", "__objc_classrefs", S_ATTR_NO_DEAD_STRIP),
            Self::Imageinfo => ("__DATA", "__objc_imageinfo", S_ATTR_NO_DEAD_STRIP),
        }
    }
}

/// The symbol of the reference to selector `name` (`class`: to class
/// `name`).
#[must_use]
pub fn ref_name(class: bool, name: &str) -> String {
    format!("{}{name}", if class { CLASSREF } else { SELREF })
}

/// Define every reference a function of `obj` loads and the image info.
pub fn define(obj: &mut Obj) {
    let refs: Vec<(SymId, bool, String)> = obj
        .syms
        .iter()
        .enumerate()
        .filter(|(_, s)| s.def.is_none())
        .filter_map(|(i, s)| {
            let (class, name) = s
                .name
                .strip_prefix(SELREF)
                .map(|n| (false, n))
                .or_else(|| s.name.strip_prefix(CLASSREF).map(|n| (true, n)))?;
            Some((i as SymId, class, name.to_owned()))
        })
        .collect();
    if refs.is_empty() {
        return;
    }
    for (sym, class, name) in refs {
        let target = if class {
            obj.sym(&format!("OBJC_CLASS_$_{name}"))
        } else {
            let m = obj.sym(&format!("{METHNAME}{name}"));
            if obj.syms[m as usize].def.is_none() {
                let mut bytes = name.into_bytes();
                bytes.push(0);
                obj.define_data(m, Sect::Objc(Part::Methname), &bytes, 1, false);
            }
            m
        };
        let part = if class {
            Part::Classrefs
        } else {
            Part::Selrefs
        };
        let off = obj.define_data(sym, Sect::Objc(part), &[0; 8], 8, false);
        obj.abs64(Sect::Objc(part), off, target);
    }
    // Version 0, flags "has category class properties", as clang writes.
    let info = obj.sym("nabi.objc.imageinfo");
    obj.define_data(
        info,
        Sect::Objc(Part::Imageinfo),
        &[0, 0, 0, 0, 0x40, 0, 0, 0],
        4,
        false,
    );
}
