//! Constant expressions: conversions, operators, literals.

#[allow(clippy::wildcard_imports)]
use crate::*;

impl Parser<'_, '_> {
    pub(crate) fn int_value(&self, e: &Typed, loc: Loc, what: &str) -> R<i128> {
        match self.rvalue(e).v {
            Val::Int(v) => Ok(v),
            _ => Err(self.err(
                loc,
                &format!(
                    "{what} is not an integer constant expression bindgen can evaluate; write it \
                     with integer literals, enum constants, casts, sizeof, _Alignof and offsetof"
                ),
            )),
        }
    }

    pub(crate) fn prim_q(&self, p: Prim) -> QualType {
        QualType::new(self.types.prim(p))
    }

    pub(crate) fn typed_int(&self, p: Prim, v: i128) -> Typed {
        Typed {
            ty: self.prim_q(p),
            v: Val::Int(self.wrap(p, v)),
            lvalue: false,
        }
    }

    /// `v` in the range of `p`.
    pub(crate) fn wrap(&self, p: Prim, v: i128) -> i128 {
        if p == Prim::Bool {
            return i128::from(v != 0);
        }
        let bits = self.types.t.bits(p);
        if bits >= 128 {
            return v;
        }
        let m = v & ((1i128 << bits) - 1);
        if self.types.t.signed(p) && (m >> (bits - 1)) & 1 == 1 {
            m - (1i128 << bits)
        } else {
            m
        }
    }

    /// An expression used as a value: lvalues read (not constant),
    /// arrays and functions decayed.
    pub fn rvalue(&self, e: &Typed) -> Typed {
        let canon = self.types.canon(e.ty.ty);
        match self.types.get(canon) {
            Type::Array { .. } | Type::Func(_) => {
                // Keep string literals and addresses.
                let v = match &e.v {
                    Val::Str(_) => e.v.clone(),
                    Val::Ptr(a) if e.lvalue => Val::Ptr(*a),
                    _ => Val::None,
                };
                Typed {
                    ty: e.ty,
                    v,
                    lvalue: false,
                }
            }
            _ if e.lvalue => Typed {
                ty: e.ty,
                v: Val::None,
                lvalue: false,
            },
            _ => e.clone(),
        }
    }

    pub fn expr(&mut self) -> R<Typed> {
        let mut e = self.assign_expr()?;
        while self.eat(",")? {
            let r = self.assign_expr()?;
            let ok = !matches!(self.rvalue(&e).v, Val::None);
            e = if ok { r } else { Self::unknown(r.ty) };
        }
        Ok(e)
    }

    pub(crate) fn assign_expr(&mut self) -> R<Typed> {
        let e = self.cond_expr()?;
        let t = self.peek()?;
        if t.kind == Kind::Punct
            && matches!(
                self.name(t.sym),
                "=" | "+=" | "-=" | "*=" | "/=" | "%=" | "<<=" | ">>=" | "&=" | "^=" | "|="
            )
        {
            self.bump()?;
            self.assign_expr()?;
            return Ok(Self::unknown(e.ty));
        }
        Ok(e)
    }

    pub(crate) const fn unknown(ty: QualType) -> Typed {
        Typed {
            ty,
            v: Val::None,
            lvalue: false,
        }
    }

    pub fn cond_expr(&mut self) -> R<Typed> {
        let c = self.binary(0)?;
        if !self.at("?")? {
            return Ok(c);
        }
        self.bump()?;
        let truth = self.truth(&c);
        let a = if self.at(":")? { c } else { self.expr()? };
        self.expect(":")?;
        let b = self.cond_expr()?;
        let (a, b) = (self.rvalue(&a), self.rvalue(&b));
        let ty = match (self.arith_prim(a.ty), self.arith_prim(b.ty)) {
            (Some(x), Some(y)) => self.prim_q(self.common(x, y)),
            _ => a.ty,
        };
        let pick = match truth {
            Some(true) => a,
            Some(false) => b,
            None => return Ok(Self::unknown(ty)),
        };
        Ok(self.convert(&pick, ty))
    }

    pub(crate) fn truth(&self, e: &Typed) -> Option<bool> {
        match self.rvalue(e).v {
            Val::Int(v) => Some(v != 0),
            Val::Float(f) => Some(f != 0.0),
            Val::Ptr(p) => Some(p != 0),
            Val::Str(_) => Some(true),
            Val::None => None,
        }
    }

    pub(crate) fn binary(&mut self, min: u8) -> R<Typed> {
        let mut l = self.cast_expr()?;
        loop {
            let t = self.peek()?;
            if t.kind != Kind::Punct {
                return Ok(l);
            }
            let op = self.name(t.sym).to_owned();
            let prec = match op.as_str() {
                "*" | "/" | "%" => 10,
                "+" | "-" => 9,
                "<<" | ">>" => 8,
                "<" | ">" | "<=" | ">=" => 7,
                "==" | "!=" => 6,
                "&" => 5,
                "^" => 4,
                "|" => 3,
                "&&" => 2,
                "||" => 1,
                _ => return Ok(l),
            };
            if prec <= min {
                return Ok(l);
            }
            self.bump()?;
            let r = self.binary(prec)?;
            l = self.binop(&op, &l, &r);
        }
    }

    /// The arithmetic builtin of a type (enums as their integer).
    pub(crate) fn arith_prim(&self, q: QualType) -> Option<Prim> {
        let p = self.types.prim_of(q.ty)?;
        (p.is_integer() || p.is_float()).then_some(p)
    }

    pub(crate) const fn promote(p: Prim) -> Prim {
        if p.is_integer() && p.rank() < Prim::Int.rank() {
            Prim::Int
        } else {
            p
        }
    }

    /// The usual arithmetic conversions.
    pub(crate) fn common(&self, a: Prim, b: Prim) -> Prim {
        let fl = |p: Prim| match p {
            Prim::LongDouble | Prim::Float128 | Prim::Ibm128 => 4,
            Prim::Double => 3,
            Prim::Float => 2,
            Prim::Float16 | Prim::Fp16 | Prim::BF16 => 1,
            _ => 0,
        };
        if a.is_float() || b.is_float() {
            return if fl(a) >= fl(b) { a } else { b };
        }
        let (a, b) = (Self::promote(a), Self::promote(b));
        if a == b {
            return a;
        }
        let t = &self.types.t;
        let (sa, sb) = (t.signed(a), t.signed(b));
        if sa == sb {
            return if a.rank() >= b.rank() { a } else { b };
        }
        let (s, u) = if sa { (a, b) } else { (b, a) };
        if u.rank() >= s.rank() {
            u
        } else if t.bits(s) > t.bits(u) {
            s
        } else {
            s.to_unsigned()
        }
    }

    /// `e` converted to `to`.
    pub(crate) fn convert(&self, e: &Typed, to: QualType) -> Typed {
        let e = self.rvalue(e);
        let canon = self.types.canon(to.ty);
        let v = match (self.types.get(canon), self.types.prim_of(canon), &e.v) {
            (_, Some(p), Val::Int(v)) if p.is_integer() => Val::Int(self.wrap(p, *v)),
            (_, Some(p), Val::Float(f)) if p.is_integer() => {
                if p == Prim::Bool {
                    Val::Int(i128::from(*f != 0.0))
                } else if f.is_finite() {
                    Val::Int(self.wrap(p, *f as i128))
                } else {
                    Val::None
                }
            }
            (_, Some(p), Val::Ptr(a)) if p.is_integer() => Val::Int(self.wrap(p, *a)),
            (_, Some(p), Val::Int(v)) if p.is_float() => Val::Float(round_float(p, *v as f64)),
            (_, Some(p), Val::Float(f)) if p.is_float() => Val::Float(round_float(p, *f)),
            (Type::Pointer(_), _, Val::Int(v)) => Val::Ptr(*v),
            (Type::Pointer(_), _, Val::Ptr(a)) => Val::Ptr(*a),
            _ => Val::None,
        };
        Typed {
            ty: to,
            v,
            lvalue: false,
        }
    }

    pub(crate) fn binop(&self, op: &str, l: &Typed, r: &Typed) -> Typed {
        let (l, r) = (self.rvalue(l), self.rvalue(r));
        let int = self.prim_q(Prim::Int);
        if matches!(op, "&&" | "||") {
            let v = match (self.truth(&l), self.truth(&r), op) {
                (Some(false), _, "&&") => Val::Int(0),
                (Some(true), _, "||") => Val::Int(1),
                (Some(_), Some(b), _) => Val::Int(i128::from(b)),
                _ => Val::None,
            };
            return Typed {
                ty: int,
                v,
                lvalue: false,
            };
        }
        // Pointer arithmetic, for address constants.
        let lp = self.pointee(l.ty);
        let rp = self.pointee(r.ty);
        if lp.is_some() || rp.is_some() {
            let size = |p: Option<QualType>| {
                p.and_then(|p| self.types.size_align(p.ty))
                    .map_or(1, |x| x.0) as i128
            };
            let v = match (op, &l.v, &r.v) {
                ("+", Val::Ptr(a), Val::Int(n)) => Val::Ptr(a + n * size(lp)),
                ("+", Val::Int(n), Val::Ptr(a)) => Val::Ptr(a + n * size(rp)),
                ("-", Val::Ptr(a), Val::Int(n)) => Val::Ptr(a - n * size(lp)),
                ("-", Val::Ptr(a), Val::Ptr(b)) if rp.is_some() => {
                    return Typed {
                        ty: QualType::new(self.types.prim(self.types.t.ptrdiff_t)),
                        v: Val::Int((a - b) / size(lp).max(1)),
                        lvalue: false,
                    };
                }
                ("==" | "!=" | "<" | ">" | "<=" | ">=", Val::Ptr(a), Val::Ptr(b)) => {
                    let c = match op {
                        "==" => a == b,
                        "!=" => a != b,
                        "<" => a < b,
                        ">" => a > b,
                        "<=" => a <= b,
                        _ => a >= b,
                    };
                    return self.typed_int(Prim::Int, i128::from(c));
                }
                _ => Val::None,
            };
            let ty = if lp.is_some() { l.ty } else { r.ty };
            let ty = if matches!(op, "==" | "!=" | "<" | ">" | "<=" | ">=") {
                int
            } else {
                ty
            };
            return Typed {
                ty,
                v,
                lvalue: false,
            };
        }
        let (Some(a), Some(b)) = (self.arith_prim(l.ty), self.arith_prim(r.ty)) else {
            return Self::unknown(int);
        };
        let shift = matches!(op, "<<" | ">>");
        let ty = if shift {
            Self::promote(a)
        } else {
            self.common(a, b)
        };
        let cmp = matches!(op, "<" | ">" | "<=" | ">=" | "==" | "!=");
        let res = if cmp { Prim::Int } else { ty };
        if ty.is_float() {
            if matches!(op, "%" | "<<" | ">>" | "&" | "|" | "^") {
                return Self::unknown(self.prim_q(res));
            }
            let f = |v: &Val| match v {
                Val::Int(i) => Some(*i as f64),
                Val::Float(f) => Some(*f),
                _ => None,
            };
            let (Some(x), Some(y)) = (f(&l.v), f(&r.v)) else {
                return Self::unknown(self.prim_q(res));
            };
            let v = match op {
                "*" => Val::Float(round_float(ty, x * y)),
                "/" => Val::Float(round_float(ty, x / y)),
                "+" => Val::Float(round_float(ty, x + y)),
                "-" => Val::Float(round_float(ty, x - y)),
                "<" => Val::Int(i128::from(x < y)),
                ">" => Val::Int(i128::from(x > y)),
                "<=" => Val::Int(i128::from(x <= y)),
                ">=" => Val::Int(i128::from(x >= y)),
                "==" => Val::Int(i128::from((x - y).abs() == 0.0)),
                _ => Val::Int(i128::from((x - y).abs() != 0.0)),
            };
            return Typed {
                ty: self.prim_q(res),
                v,
                lvalue: false,
            };
        }
        let (Val::Int(x), Val::Int(y)) = (&l.v, &r.v) else {
            return Self::unknown(self.prim_q(res));
        };
        let x = self.wrap(ty, *x);
        let y = if shift { *y } else { self.wrap(ty, *y) };
        let bits = i128::from(self.types.t.bits(ty));
        let v = match op {
            "*" => x.checked_mul(y),
            "/" => (y != 0).then(|| x / y),
            "%" => (y != 0).then(|| x % y),
            "+" => Some(x + y),
            "-" => Some(x - y),
            "<<" => (0..bits).contains(&y).then(|| x << y),
            ">>" => (0..bits).contains(&y).then(|| x >> y),
            "<" => Some(i128::from(x < y)),
            ">" => Some(i128::from(x > y)),
            "<=" => Some(i128::from(x <= y)),
            ">=" => Some(i128::from(x >= y)),
            "==" => Some(i128::from(x == y)),
            "!=" => Some(i128::from(x != y)),
            "&" => Some(x & y),
            "^" => Some(x ^ y),
            _ => Some(x | y),
        };
        match v {
            Some(v) => self.typed_int(res, v),
            None => Self::unknown(self.prim_q(res)),
        }
    }

    pub(crate) fn pointee(&self, q: QualType) -> Option<QualType> {
        match self.types.get(self.types.canon(q.ty)) {
            Type::Pointer(p) => Some(*p),
            Type::Array { elem, .. } => Some(*elem),
            _ => None,
        }
    }

    pub(crate) fn cast_expr(&mut self) -> R<Typed> {
        if self.at("(")? && self.second_starts_type()? {
            self.bump()?;
            let q = self.type_name()?;
            self.expect(")")?;
            if self.at("{")? {
                // A compound literal.
                self.skip_balanced()?;
                let e = Typed {
                    ty: q,
                    v: Val::None,
                    lvalue: true,
                };
                return self.postfix(e);
            }
            let e = self.cast_expr()?;
            return Ok(self.convert(&e, q));
        }
        self.unary()
    }

    pub(crate) fn unary(&mut self) -> R<Typed> {
        let t = self.peek()?;
        if t.kind == Kind::Punct {
            let op = self.name(t.sym).to_owned();
            match op.as_str() {
                "-" | "+" | "~" | "!" => {
                    self.bump()?;
                    let e = self.cast_expr()?;
                    let e = self.rvalue(&e);
                    if op == "!" {
                        return Ok(match self.truth(&e) {
                            Some(b) => self.typed_int(Prim::Int, i128::from(!b)),
                            None => Self::unknown(self.prim_q(Prim::Int)),
                        });
                    }
                    let Some(p) = self.arith_prim(e.ty) else {
                        return Ok(Self::unknown(e.ty));
                    };
                    let p = Self::promote(p);
                    return Ok(match (&e.v, op.as_str()) {
                        (Val::Int(v), "-") => self.typed_int(p, -v),
                        (Val::Int(v), "+") => self.typed_int(p, *v),
                        (Val::Int(v), _) => self.typed_int(p, !v),
                        (Val::Float(f), "-") => Typed {
                            ty: self.prim_q(p),
                            v: Val::Float(-f),
                            lvalue: false,
                        },
                        (Val::Float(f), "+") => Typed {
                            ty: self.prim_q(p),
                            v: Val::Float(*f),
                            lvalue: false,
                        },
                        _ => Self::unknown(self.prim_q(p)),
                    });
                }
                "&" => {
                    self.bump()?;
                    let e = self.cast_expr()?;
                    let ty = QualType::new(self.types.pointer(e.ty));
                    let v = match e.v {
                        Val::Ptr(a) if e.lvalue => Val::Ptr(a),
                        _ => Val::None,
                    };
                    return Ok(Typed {
                        ty,
                        v,
                        lvalue: false,
                    });
                }
                "*" => {
                    self.bump()?;
                    let e = self.cast_expr()?;
                    let e = self.rvalue(&e);
                    let ty = self.pointee(e.ty).unwrap_or_else(|| self.prim_q(Prim::Int));
                    let v = match e.v {
                        Val::Ptr(a) => Val::Ptr(a),
                        _ => Val::None,
                    };
                    return Ok(Typed {
                        ty,
                        v,
                        lvalue: true,
                    });
                }
                "++" | "--" => {
                    self.bump()?;
                    let e = self.unary()?;
                    return Ok(Self::unknown(e.ty));
                }
                "&&" => {
                    // A label's address.
                    self.bump()?;
                    self.bump()?;
                    let v = QualType::new(self.types.prim(Prim::Void));
                    let p = QualType::new(self.types.pointer(v));
                    return Ok(Self::unknown(p));
                }
                _ => {}
            }
        }
        if t.kind == Kind::Ident {
            match t.sym {
                kw::SIZEOF | kw::ALIGNOF2 | kw::ALIGNOF3 | kw::ALIGNOF4 => {
                    self.bump()?;
                    let q = if self.at("(")? && self.second_starts_type()? {
                        self.bump()?;
                        let q = self.type_name()?;
                        self.expect(")")?;
                        if self.at("{")? {
                            self.skip_balanced()?;
                        }
                        q
                    } else {
                        self.unary()?.ty
                    };
                    let size = self.types.prim(self.types.t.size_t);
                    let v = match self.types.size_align(q.ty) {
                        Some((s, a)) => {
                            Val::Int(i128::from(if t.sym == kw::SIZEOF { s } else { a }))
                        }
                        None if self.types.is_void(q.ty) => Val::Int(1),
                        None => match self.types.get(self.types.canon(q.ty)) {
                            Type::Func(_) => Val::Int(1),
                            _ => Val::None,
                        },
                    };
                    return Ok(Typed {
                        ty: QualType::new(size),
                        v,
                        lvalue: false,
                    });
                }
                kw::EXTENSION => {
                    self.bump()?;
                    return self.cast_expr();
                }
                kw::REAL | kw::IMAG => {
                    self.bump()?;
                    let e = self.cast_expr()?;
                    return Ok(Self::unknown(e.ty));
                }
                _ => {}
            }
        }
        let e = self.primary()?;
        self.postfix(e)
    }

    pub(crate) fn postfix(&mut self, mut e: Typed) -> R<Typed> {
        loop {
            let t = self.peek()?;
            if t.kind != Kind::Punct {
                return Ok(e);
            }
            match self.name(t.sym) {
                "[" => {
                    self.bump()?;
                    let i = self.expr()?;
                    self.expect("]")?;
                    let b = self.rvalue(&e);
                    let i = self.rvalue(&i);
                    let (base, idx) = if self.pointee(b.ty).is_some() {
                        (b, i)
                    } else {
                        (i, b)
                    };
                    let elem = self
                        .pointee(base.ty)
                        .unwrap_or_else(|| self.prim_q(Prim::Int));
                    let size = self.types.size_align(elem.ty).map_or(1, |x| x.0) as i128;
                    let v = match (&base.v, &idx.v) {
                        (Val::Ptr(a), Val::Int(n)) => Val::Ptr(a + n * size),
                        _ => Val::None,
                    };
                    e = Typed {
                        ty: elem,
                        v,
                        lvalue: true,
                    };
                }
                "(" => {
                    let f = self.rvalue(&e);
                    let ret = match self.types.get(self.types.canon(f.ty.ty)) {
                        Type::Func(fun) => fun.ret,
                        Type::Pointer(p) => match self.types.get(self.types.canon(p.ty)) {
                            Type::Func(fun) => fun.ret,
                            _ => self.prim_q(Prim::Int),
                        },
                        _ => self.prim_q(Prim::Int),
                    };
                    self.skip_balanced()?;
                    e = Self::unknown(ret);
                }
                "." | "->" => {
                    let arrow = self.name(t.sym) == "->";
                    self.bump()?;
                    let m = self.bump()?;
                    if m.kind != Kind::Ident {
                        return Err(self.err(m.loc, "expected a member name"));
                    }
                    let (rec_ty, base) = if arrow {
                        let b = self.rvalue(&e);
                        let p = self.pointee(b.ty).unwrap_or(b.ty);
                        (p, if let Val::Ptr(a) = b.v { Some(a) } else { None })
                    } else {
                        (
                            e.ty,
                            if let (true, Val::Ptr(a)) = (e.lvalue, &e.v) {
                                Some(*a)
                            } else {
                                None
                            },
                        )
                    };
                    let name = self.name(m.sym).to_owned();
                    let Some((fty, off)) = self.member_of(rec_ty.ty, &name) else {
                        return Err(self.err(
                            m.loc,
                            &format!("no member named '{name}' in {}", self.types.spell(rec_ty)),
                        ));
                    };
                    e = Typed {
                        ty: fty,
                        v: match base {
                            Some(a) => Val::Ptr(a + (off / 8) as i128),
                            None => Val::None,
                        },
                        lvalue: true,
                    };
                }
                "++" | "--" => {
                    self.bump()?;
                    e = Self::unknown(e.ty);
                }
                _ => return Ok(e),
            }
        }
    }

    /// A member's type and bit offset, through anonymous members.
    pub(crate) fn member_of(&self, ty: TypeId, name: &str) -> Option<(QualType, u64)> {
        let Type::Record(r) = self.types.get(self.types.canon(ty)) else {
            return None;
        };
        let rec = &self.types.records[*r as usize];
        let def = rec.def.as_ref()?;
        let layout = self.types.layout(*r);
        for (i, f) in def.fields.iter().enumerate() {
            let off = layout.as_ref().map_or(0, |l| l.offsets[i]);
            if f.name.as_deref() == Some(name) {
                return Some((f.ty, off));
            }
            if f.anon {
                if let Some((t, o)) = self.member_of(f.ty.ty, name) {
                    return Some((t, off + o));
                }
            }
        }
        None
    }

    pub(crate) fn primary(&mut self) -> R<Typed> {
        let t = self.bump()?;
        match t.kind {
            Kind::Num => self.number(t),
            Kind::Char => {
                let s = self.name(t.sym).to_owned();
                let Some(v) = char_value(&s, self.types.t.char_signed).map(i128::from) else {
                    return Err(self.err(t.loc, "invalid character literal"));
                };
                let p = match s.chars().next() {
                    Some('L') => self.types.t.wchar,
                    Some('u') if s.starts_with("u8") => Prim::UChar,
                    Some('u') => Prim::UShort,
                    Some('U') => Prim::UInt,
                    _ => Prim::Int,
                };
                Ok(self.typed_int(p, v))
            }
            Kind::Str => {
                let mut bytes = Vec::new();
                let mut wide = false;
                let mut s = self.name(t.sym).to_owned();
                loop {
                    if !s.starts_with('"') && !s.starts_with("u8") {
                        wide = true;
                    }
                    bytes.extend(str_bytes(&s));
                    let n = self.peek()?;
                    if n.kind != Kind::Str {
                        break;
                    }
                    self.bump()?;
                    self.name(n.sym).clone_into(&mut s);
                }
                let elem = if wide { self.types.t.wchar } else { Prim::Char };
                let n = bytes.len() as u64 + 1;
                let ty = self.types.add(Type::Array {
                    elem: self.prim_q(elem),
                    len: Some(n),
                });
                Ok(Typed {
                    ty: QualType::new(ty),
                    v: if wide { Val::None } else { Val::Str(bytes) },
                    lvalue: false,
                })
            }
            Kind::Punct if self.name(t.sym) == "(" => {
                if self.at("{")? {
                    // A statement expression.
                    self.skip_balanced()?;
                    self.expect(")")?;
                    return Ok(Self::unknown(self.prim_q(Prim::Int)));
                }
                let e = self.expr()?;
                self.expect(")")?;
                Ok(e)
            }
            Kind::Ident => self.ident_expr(t),
            _ => Err(self.err(
                t.loc,
                &format!("expected an expression, found {}", self.describe(t)),
            )),
        }
    }

    pub(crate) fn ident_expr(&mut self, t: Tok) -> R<Typed> {
        match t.sym {
            kw::OFFSETOF => {
                self.expect("(")?;
                let q = self.type_name()?;
                self.expect(",")?;
                let mut ty = q;
                let mut off: i128 = 0;
                loop {
                    let m = self.bump()?;
                    if m.kind != Kind::Ident {
                        return Err(self.err(m.loc, "expected a member name"));
                    }
                    let name = self.name(m.sym).to_owned();
                    let Some((fty, o)) = self.member_of(ty.ty, &name) else {
                        return Err(self.err(
                            m.loc,
                            &format!("no member named '{name}' in {}", self.types.spell(ty)),
                        ));
                    };
                    off += (o / 8) as i128;
                    ty = fty;
                    loop {
                        if self.eat("[")? {
                            let i = self.expr()?;
                            self.expect("]")?;
                            let elem = self.pointee(ty).unwrap_or(ty);
                            let s = self.types.size_align(elem.ty).map_or(0, |x| x.0) as i128;
                            if let Val::Int(i) = self.rvalue(&i).v {
                                off += i * s;
                            }
                            ty = elem;
                        } else {
                            break;
                        }
                    }
                    if !self.eat(".")? {
                        break;
                    }
                }
                self.expect(")")?;
                let size_t = self.types.t.size_t;
                Ok(self.typed_int(size_t, off))
            }
            kw::VA_ARG => {
                self.expect("(")?;
                self.assign_expr()?;
                self.expect(",")?;
                let q = self.type_name()?;
                self.expect(")")?;
                Ok(Self::unknown(q))
            }
            kw::TYPES_COMPATIBLE => {
                self.expect("(")?;
                let a = self.type_name()?;
                self.expect(",")?;
                let b = self.type_name()?;
                self.expect(")")?;
                let same = self.types.canon(a.ty) == self.types.canon(b.ty)
                    || (self.types.prim_of(a.ty).is_some()
                        && self.types.prim_of(a.ty) == self.types.prim_of(b.ty));
                Ok(self.typed_int(Prim::Int, i128::from(same)))
            }
            kw::CHOOSE_EXPR => {
                self.expect("(")?;
                let c = self.assign_expr()?;
                self.expect(",")?;
                let a = self.assign_expr()?;
                self.expect(",")?;
                let b = self.assign_expr()?;
                self.expect(")")?;
                Ok(if self.truth(&c) == Some(false) { b } else { a })
            }
            kw::GENERIC => {
                self.skip_balanced()?;
                Ok(Self::unknown(self.prim_q(Prim::Int)))
            }
            kw::TRUE | kw::FALSE if !self.ords.contains_key(&t.sym) => {
                Ok(self.typed_int(Prim::Int, i128::from(t.sym == kw::TRUE)))
            }
            _ => {
                let name = self.name(t.sym).to_owned();
                // Microsoft's `__noop` is 0, its arguments unevaluated.
                if name == "__noop" && self.types.t.msvc {
                    if self.at("(")? {
                        self.skip_balanced()?;
                    }
                    return Ok(self.typed_int(Prim::Int, 0));
                }
                if name == "__builtin_constant_p" && self.at("(")? {
                    self.bump()?;
                    let e = self.assign_expr()?;
                    self.expect(")")?;
                    let c = !matches!(self.rvalue(&e).v, Val::None);
                    return Ok(self.typed_int(Prim::Int, i128::from(c)));
                }
                if name == "__builtin_expect" && self.at("(")? {
                    self.bump()?;
                    let e = self.assign_expr()?;
                    self.expect(",")?;
                    self.assign_expr()?;
                    self.expect(")")?;
                    return Ok(self.rvalue(&e));
                }
                if let Some(&(_, ty)) = self.hidden.iter().rev().find(|h| h.0 == t.sym) {
                    return Ok(Typed {
                        ty,
                        v: Val::None,
                        lvalue: true,
                    });
                }
                match self.ords.get(&t.sym).copied() {
                    Some(Ord::Enumerator(e, i)) => {
                        let en = &self.types.enums[e as usize];
                        let v = en.values[i as usize].value;
                        // An enumerator is an `int`, or of its enum's type
                        // when it does not fit one.
                        let p = if i32::try_from(v).is_ok() && !en.fixed {
                            Prim::Int
                        } else {
                            en.int
                                .and_then(|i| self.types.prim_of(i))
                                .unwrap_or(Prim::LongLong)
                        };
                        Ok(self.typed_int(p, v))
                    }
                    Some(Ord::Local(l)) => Ok(Typed {
                        ty: self.locals[l as usize].ty,
                        v: Val::None,
                        lvalue: true,
                    }),
                    Some(Ord::Var(v)) => match self.objc.consts.get(&v) {
                        Some(&n) => Ok(Typed {
                            ty: self.vars[v].ty,
                            v: Val::Int(n),
                            lvalue: false,
                        }),
                        None => Ok(Typed {
                            ty: self.vars[v].ty,
                            v: Val::None,
                            lvalue: true,
                        }),
                    },
                    Some(Ord::Func(f)) => Ok(Typed {
                        ty: QualType::new(self.fns[f].ty),
                        v: Val::None,
                        lvalue: false,
                    }),
                    Some(Ord::Typedef(_)) => Err(self.err(
                        t.loc,
                        &format!("unexpected type name '{name}': expected an expression"),
                    )),
                    None if name.starts_with("__builtin_") && self.at("(")? => {
                        // A builtin function: a value, not a constant.
                        Ok(Self::unknown(self.prim_q(Prim::Int)))
                    }
                    None => Err(self.err(t.loc, &format!("use of undeclared identifier '{name}'"))),
                }
            }
        }
    }

    pub(crate) fn number(&self, t: Tok) -> R<Typed> {
        let s = self.name(t.sym);
        let lower = s.to_ascii_lowercase();
        let is_hex = lower.starts_with("0x");
        let float = if is_hex {
            lower.contains('p')
        } else {
            lower.contains(['.', 'e'])
        };
        if !float {
            let Some((v, u)) = parse_int(s) else {
                return Err(self.err(t.loc, &format!("invalid number `{s}`")));
            };
            let decimal = !lower.starts_with('0') || lower == "0";
            return Ok(self.int_literal(&lower, v, u, decimal));
        }
        // A floating literal and its suffix.
        let (body, p) = if let Some(b) = lower.strip_suffix("f16") {
            (b, Prim::Float16)
        } else if let Some(b) = lower.strip_suffix("bf16") {
            (b, Prim::BF16)
        } else if let Some(b) = lower
            .strip_suffix("f128")
            .or_else(|| lower.strip_suffix('q'))
        {
            (b, Prim::Float128)
        } else if let Some(b) = lower.strip_suffix('f') {
            (b, Prim::Float)
        } else if let Some(b) = lower.strip_suffix('l') {
            (b, Prim::LongDouble)
        } else {
            (lower.as_str(), Prim::Double)
        };
        let v = if is_hex {
            hex_float(body)
        } else {
            body.parse::<f64>().ok()
        };
        let Some(v) = v else {
            return Err(self.err(t.loc, &format!("invalid number `{s}`")));
        };
        Ok(Typed {
            ty: self.prim_q(p),
            v: Val::Float(round_float(p, v)),
            lvalue: false,
        })
    }

    /// An integer literal's type by C's rules: the first of its suffix's
    /// list the value fits.
    pub(crate) fn int_literal(
        &self,
        lower: &str,
        v: u64,
        unsigned_suffix: bool,
        decimal: bool,
    ) -> Typed {
        let t = &self.types.t;
        let suffix: String = lower
            .chars()
            .rev()
            .take_while(|c| matches!(c, 'u' | 'l'))
            .collect();
        let ll = suffix.contains("ll");
        let l = !ll && suffix.contains('l');
        // Microsoft's sized suffixes set the type.
        for (suf, p) in [
            ("i8", Prim::Char),
            ("i16", Prim::Short),
            ("i32", Prim::Int),
            ("i64", Prim::LongLong),
        ] {
            if lower.ends_with(suf) && t.msvc {
                let p = if unsigned_suffix {
                    if p == Prim::Char {
                        Prim::UChar
                    } else {
                        p.to_unsigned()
                    }
                } else {
                    p
                };
                return self.typed_int(p, i128::from(v));
            }
        }
        let ms64 = lower.ends_with("i64");
        let mut cands: Vec<Prim> = Vec::new();
        if !l && !ll && !ms64 {
            cands.push(Prim::Int);
            if !decimal || unsigned_suffix {
                cands.push(Prim::UInt);
            }
        }
        if !ll && !ms64 {
            cands.push(Prim::Long);
            if !decimal || unsigned_suffix {
                cands.push(Prim::ULong);
            }
        }
        cands.push(Prim::LongLong);
        cands.push(Prim::ULongLong);
        if unsigned_suffix {
            cands.retain(|p| !t.signed(*p));
        }
        let fits = |p: Prim| {
            let bits = t.bits(p);
            if t.signed(p) {
                u128::from(v) < 1u128 << (bits - 1)
            } else {
                u128::from(v) < 1u128 << bits
            }
        };
        let p = cands
            .into_iter()
            .find(|p| fits(*p))
            .unwrap_or(Prim::ULongLong);
        self.typed_int(p, i128::from(v))
    }

    /// Parse `toks` as a constant expression in the unit's final scope
    /// (bindgen's macro constants).
    pub fn eval_tokens(&mut self, toks: Vec<Tok>) -> Option<Typed> {
        let saved = std::mem::take(&mut self.buf);
        let mut end = toks.last().copied()?;
        end.kind = Kind::Eof;
        self.buf = toks.into();
        self.buf.push_back(end);
        let r = self.expr();
        let at_end = self.peek().is_ok_and(|t| t.kind == Kind::Eof);
        self.buf = saved;
        let e = r.ok()?;
        at_end.then(|| self.rvalue(&e))
    }

    pub const fn arch(&self) -> Arch {
        self.types.t.arch
    }
}

pub(crate) fn round_float(p: Prim, v: f64) -> f64 {
    match p {
        Prim::Float => f64::from(v as f32),
        _ => v,
    }
}

/// `0x1.8p3` and kin, without the suffix.
pub(crate) fn hex_float(s: &str) -> Option<f64> {
    let s = s.strip_prefix("0x")?;
    let (mant, exp) = s.split_once('p').unwrap_or((s, "0"));
    let exp: i32 = exp.parse().ok()?;
    let (int, frac) = mant.split_once('.').unwrap_or((mant, ""));
    let mut v = 0f64;
    for c in int.chars() {
        v = v.mul_add(16.0, f64::from(c.to_digit(16)?));
    }
    let mut scale = 1.0 / 16.0;
    for c in frac.chars() {
        v += f64::from(c.to_digit(16)?) * scale;
        scale /= 16.0;
    }
    Some(v * 2f64.powi(exp))
}

/// A narrow string literal's bytes (escapes resolved, quotes and prefix
/// dropped).
pub fn str_bytes(s: &str) -> Vec<u8> {
    let Some(start) = s.find('"') else {
        return Vec::new();
    };
    let body = &s[start + 1..s.len().saturating_sub(1).max(start + 1)];
    let b = body.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        if b[i] != b'\\' {
            out.push(b[i]);
            i += 1;
            continue;
        }
        i += 1;
        let Some(&c) = b.get(i) else { break };
        i += 1;
        match c {
            b'n' => out.push(b'\n'),
            b't' => out.push(b'\t'),
            b'r' => out.push(b'\r'),
            b'a' => out.push(7),
            b'b' => out.push(8),
            b'f' => out.push(12),
            b'v' => out.push(11),
            b'e' => out.push(27),
            b'x' => {
                let mut v: u32 = 0;
                while i < b.len() && b[i].is_ascii_hexdigit() {
                    v = v * 16 + (b[i] as char).to_digit(16).unwrap_or(0);
                    i += 1;
                }
                out.push(v as u8);
            }
            b'0'..=b'7' => {
                let mut v: u32 = u32::from(c - b'0');
                let mut n = 1;
                while n < 3 && i < b.len() && (b'0'..=b'7').contains(&b[i]) {
                    v = v * 8 + u32::from(b[i] - b'0');
                    i += 1;
                    n += 1;
                }
                out.push(v as u8);
            }
            c => out.push(c),
        }
    }
    out
}
