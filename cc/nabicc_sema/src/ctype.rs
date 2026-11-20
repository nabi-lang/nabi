//! Questions about C types: classes, sizes, promotions and the usual
//! arithmetic conversions.

use crate::check::Cx;
use nabicc_parse::ty::{Func, QualType, RecordId, Type, TypeId};
use nabicc_target::Prim;

impl Cx<'_> {
    /// Through typedefs, `typeof` and `_Atomic`; an enum as its integer
    /// type.
    pub(crate) fn unqual(&self, t: TypeId) -> TypeId {
        let mut t = self.types.canon(t);
        loop {
            match self.types.get(t).clone() {
                Type::Atomic(q) => t = self.types.canon(q.ty),
                Type::Enum(e) => {
                    return match self.types.enums[e as usize].int {
                        Some(i) => self.types.canon(i),
                        None => self.types.prim(Prim::Int),
                    };
                }
                _ => return t,
            }
        }
    }

    pub(crate) fn prim_of(&self, t: TypeId) -> Option<Prim> {
        let mut t = self.types.canon(t);
        loop {
            match self.types.get(t) {
                Type::Prim(p) => return Some(*p),
                Type::Enum(e) => {
                    return Some(
                        self.types.enums[*e as usize]
                            .int
                            .and_then(|i| self.types.prim_of(i))
                            .unwrap_or(Prim::Int),
                    );
                }
                Type::Atomic(q) => t = self.types.canon(q.ty),
                _ => return None,
            }
        }
    }

    pub(crate) fn is_int(&self, t: TypeId) -> bool {
        self.prim_of(t).is_some_and(Prim::is_integer)
    }

    pub(crate) fn is_float(&self, t: TypeId) -> bool {
        self.prim_of(t).is_some_and(Prim::is_float)
    }

    pub(crate) fn is_arith(&self, t: TypeId) -> bool {
        self.is_int(t) || self.is_float(t)
    }

    pub(crate) fn is_void(&self, t: TypeId) -> bool {
        self.prim_of(t) == Some(Prim::Void)
    }

    pub(crate) fn pointee(&self, t: TypeId) -> Option<QualType> {
        match self.types.get(self.types.canon(t)) {
            Type::Pointer(q) | Type::SizedPointer(q, _) | Type::Block(q) => Some(*q),
            _ => None,
        }
    }

    pub(crate) fn is_ptr(&self, t: TypeId) -> bool {
        self.pointee(t).is_some()
    }

    pub(crate) fn is_scalar(&self, t: TypeId) -> bool {
        self.is_arith(t) || self.is_ptr(t)
    }

    pub(crate) fn record_of(&self, t: TypeId) -> Option<RecordId> {
        match self.types.get(self.types.canon(t)) {
            Type::Record(r) => Some(*r),
            _ => None,
        }
    }

    pub(crate) fn array_of(&self, t: TypeId) -> Option<(QualType, Option<u64>)> {
        match self.types.get(self.types.canon(t)) {
            Type::Array { elem, len } => Some((*elem, *len)),
            _ => None,
        }
    }

    pub(crate) fn func_of(&self, t: TypeId) -> Option<Func> {
        match self.types.get(self.types.canon(t)) {
            Type::Func(f) => Some(f.clone()),
            _ => None,
        }
    }

    pub(crate) fn size_of(&self, t: TypeId) -> Option<u64> {
        self.types.size_align(self.types.canon(t)).map(|x| x.0)
    }

    pub(crate) fn prim_ty(&self, p: Prim) -> TypeId {
        self.types.prim(p)
    }

    pub(crate) fn int_ty(&self) -> TypeId {
        self.types.prim(Prim::Int)
    }

    pub(crate) fn ptr_to(&mut self, t: TypeId) -> TypeId {
        self.types.pointer(QualType::new(t))
    }

    /// The size a pointer to `t` steps by: GNU's 1 for `void` and
    /// functions.
    pub(crate) fn step(&self, t: TypeId) -> u64 {
        if self.is_void(t) || self.func_of(t).is_some() {
            return 1;
        }
        self.size_of(t).unwrap_or(1)
    }

    /// The integer promotion.
    pub(crate) const fn promote(&self, p: Prim) -> Prim {
        let t = &self.types.t;
        match p {
            Prim::Bool | Prim::Char | Prim::SChar | Prim::UChar | Prim::Short | Prim::UShort => {
                if t.bits(p) < t.bits(Prim::Int) || t.signed(p) {
                    Prim::Int
                } else {
                    Prim::UInt
                }
            }
            Prim::Float16 | Prim::Fp16 | Prim::BF16 => Prim::Float,
            p => p,
        }
    }

    /// The usual arithmetic conversions' common type.
    pub(crate) fn common(&self, a: Prim, b: Prim) -> Prim {
        let t = &self.types.t;
        let order = [Prim::LongDouble, Prim::Float128, Prim::Double, Prim::Float];
        for f in order {
            if a == f || b == f {
                return f;
            }
        }
        let (a, b) = (self.promote(a), self.promote(b));
        if a == b {
            return a;
        }
        let (sa, sb) = (t.signed(a), t.signed(b));
        if sa == sb {
            return if a.rank() >= b.rank() { a } else { b };
        }
        let (s, u) = if sa { (a, b) } else { (b, a) };
        if u.rank() >= s.rank() {
            return u;
        }
        if t.bits(s) > t.bits(u) {
            return s;
        }
        s.to_unsigned()
    }

    /// `T` as clang spells it, for messages.
    pub(crate) fn spell(&self, t: TypeId) -> String {
        self.types.spell(QualType::new(t))
    }

    /// The size of `long double` is that of `double` here.
    pub(crate) const fn ld_is_double(&self) -> bool {
        self.types.t.prim(Prim::LongDouble).0 == 8
    }
}
