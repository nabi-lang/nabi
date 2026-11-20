//! Objective-C selectors and classes named by a string literal: on
//! Mach-O a load of a static reference (`obj::objc`), which the runtime
//! fixes up when the image loads, in place of the lookup call.

#[allow(clippy::wildcard_imports)]
use crate::lower::*;

impl FnLower<'_, '_> {
    /// `sel_registerName("x:")` / `objc_getRequiredClass("X")` as a load
    /// of the reference; false when it stays a call.
    pub(crate) fn objc_ref(&mut self, sym: SymId, args: &[Operand], dst: Option<Mem>) -> bool {
        let o = &self.c.obj;
        if o.elf || o.coff || o.wasm.is_some() {
            return false;
        }
        let class = match o.syms[sym as usize].name.as_str() {
            "sel_registerName" => false,
            "objc_getRequiredClass" => true,
            _ => return false,
        };
        let [Operand::Const(ConstVal::Str(name))] = args else {
            return false;
        };
        if name.is_empty() || name.contains('\0') {
            return false;
        }
        let Some(d) = dst else {
            return true;
        };
        let r = self.c.obj.sym(&crate::obj::objc::ref_name(class, name));
        let p = self.word_tmp();
        self.push(Inst::Sym {
            dst: p,
            sym: r,
            got: false,
        });
        let Mem::Frame(ps, _) = p else { unreachable!() };
        let k = self.word();
        self.push(Inst::Mov {
            k,
            dst: d,
            src: V::M(Mem::Ind(ps, 0)),
        });
        true
    }
}
