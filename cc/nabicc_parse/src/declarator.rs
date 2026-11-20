//! Base types, declarators and parameter lists.

#[allow(clippy::wildcard_imports)]
use crate::*;

impl Parser<'_, '_> {
    #[allow(clippy::cognitive_complexity)]
    pub(crate) fn base_type(&mut self, sp: &Specs) -> R<QualType> {
        let p = |p: Prim| Some(p);
        let prim = if let Some(b) = sp.base {
            if sp.long > 0 || sp.short || sp.unsigned || sp.signed || sp.int || sp.char_ {
                return Err(self.err(sp.loc, "cannot combine these type specifiers"));
            }
            return Ok(QualType {
                ty: self.mode(b, &sp.attrs),
                konst: sp.konst,
            });
        } else if sp.void {
            p(Prim::Void)
        } else if sp.bool_ {
            p(Prim::Bool)
        } else if sp.float16 {
            p(Prim::Float16)
        } else if sp.fp16 {
            p(Prim::Fp16)
        } else if sp.bf16 {
            p(Prim::BF16)
        } else if sp.float128 {
            p(Prim::Float128)
        } else if sp.ibm128 {
            p(Prim::Ibm128)
        } else if sp.float {
            p(Prim::Float)
        } else if sp.double {
            p(if sp.long > 0 {
                Prim::LongDouble
            } else {
                Prim::Double
            })
        } else if sp.char_ {
            p(if sp.unsigned {
                Prim::UChar
            } else if sp.signed {
                Prim::SChar
            } else {
                Prim::Char
            })
        } else if sp.int128 {
            p(if sp.unsigned {
                Prim::UInt128
            } else {
                Prim::Int128
            })
        } else if let Some(m) = sp.ms_int {
            p(match (m, sp.unsigned) {
                (Prim::Char, true) => Prim::UChar,
                (Prim::Char, false) if sp.signed => Prim::SChar,
                (m, true) => m.to_unsigned(),
                (m, false) => m,
            })
        } else if sp.short {
            p(if sp.unsigned {
                Prim::UShort
            } else {
                Prim::Short
            })
        } else if sp.long >= 2 {
            p(if sp.unsigned {
                Prim::ULongLong
            } else {
                Prim::LongLong
            })
        } else if sp.long == 1 {
            p(if sp.unsigned { Prim::ULong } else { Prim::Long })
        } else if sp.int || sp.signed || sp.unsigned {
            p(if sp.unsigned { Prim::UInt } else { Prim::Int })
        } else if sp.complex {
            p(Prim::Double)
        } else {
            None
        };
        let Some(prim) = prim else {
            return Err(self.err(
                sp.loc,
                "type specifier missing (C99 has no implicit `int`: write the type)",
            ));
        };
        let mut ty = self.types.prim(prim);
        if sp.complex {
            ty = self.types.add(Type::Complex(ty));
        }
        Ok(QualType {
            ty: self.mode(ty, &sp.attrs),
            konst: sp.konst,
        })
    }

    /// `__attribute__((mode(..)))` on an integer or floating type.
    pub(crate) fn mode(&mut self, ty: TypeId, a: &Attrs) -> TypeId {
        let Some(m) = &a.mode else {
            return ty;
        };
        // x86's 16-byte float mode is `__float128`, the others' `long double`.
        let tf = if matches!(self.types.t.arch, Arch::X86_64 | Arch::X86) {
            Prim::Float128
        } else {
            Prim::LongDouble
        };
        if let Type::Complex(_) = self.types.get(self.types.canon(ty)) {
            let e = match m.as_str() {
                "SC" => Prim::Float,
                "DC" => Prim::Double,
                "XC" => Prim::LongDouble,
                "TC" => tf,
                _ => return ty,
            };
            let e = self.types.prim(e);
            return self.types.add(Type::Complex(e));
        }
        let Some(p) = self.types.prim_of(ty) else {
            return ty;
        };
        let signed = self.types.t.signed(p);
        let int = |s: Prim| if signed { s } else { s.to_unsigned() };
        let long64 = if self.types.t.long == 8 {
            Prim::Long
        } else {
            Prim::LongLong
        };
        let r = match m.as_str() {
            "QI" | "byte" => int(Prim::SChar),
            "HI" => int(Prim::Short),
            "SI" => int(Prim::Int),
            "DI" => int(long64),
            "TI" => int(Prim::Int128),
            "word" | "pointer" | "unwind_word" => {
                if self.types.t.ptr == 8 {
                    int(long64)
                } else {
                    int(Prim::Int)
                }
            }
            "SF" => Prim::Float,
            "DF" => Prim::Double,
            "XF" => Prim::LongDouble,
            "TF" => tf,
            _ => return ty,
        };
        self.types.prim(r)
    }

    pub(crate) fn is_nested_declarator(&mut self) -> R<bool> {
        let t = self.peek_at(1)?;
        if t.kind == Kind::Punct {
            return Ok(matches!(self.name(t.sym), "*" | "^" | "(" | "["));
        }
        if t.kind != Kind::Ident {
            return Ok(false);
        }
        Ok(matches!(
            t.sym,
            kw::ATTRIBUTE
                | kw::ATTRIBUTE2
                | kw::DECLSPEC
                | kw::CDECL
                | kw::STDCALL
                | kw::FASTCALL
                | kw::VECTORCALL
                | kw::THISCALL
                | kw::REGCALL
                | kw::CDECL2
                | kw::STDCALL2
                | kw::FASTCALL2
                | kw::NONNULL
                | kw::NULLABLE
                | kw::NULL_UNSPECIFIED
                | kw::NULLABLE_RESULT
        ) || !self.starts_type(t))
    }

    /// A declarator, named or abstract.
    pub(crate) fn declarator(&mut self) -> R<Declarator> {
        let mut attrs = Attrs::default();
        self.attributes(&mut attrs)?;
        let mut ptrs: Vec<Chunk> = Vec::new();
        loop {
            let t = self.peek()?;
            if self.is_p(t, "*") || self.is_p(t, "^") {
                self.bump()?;
                let block = self.is_p(t, "^");
                let mut konst = false;
                let mut cc = None;
                let mut bytes = None;
                let mut null = None;
                loop {
                    let q = self.peek()?;
                    if q.kind != Kind::Ident {
                        break;
                    }
                    match q.sym {
                        kw::CONST | kw::CONST2 | kw::CONST3 => konst = true,
                        kw::PTR32 if self.types.t.ptr == 8 => bytes = Some(4),
                        kw::PTR64 if self.types.t.ptr == 4 => bytes = Some(8),
                        kw::NONNULL | kw::NULLABLE | kw::NULL_UNSPECIFIED | kw::NULLABLE_RESULT => {
                            null = objc::Null::of(q.sym);
                        }
                        kw::VOLATILE
                        | kw::VOLATILE2
                        | kw::VOLATILE3
                        | kw::RESTRICT
                        | kw::RESTRICT2
                        | kw::RESTRICT3
                        | kw::PTR32
                        | kw::PTR64
                        | kw::UNALIGNED
                        | kw::SPTR
                        | kw::UPTR
                        | kw::ATOMIC
                        | kw::EXTENSION => {}
                        kw::ATTRIBUTE
                        | kw::ATTRIBUTE2
                        | kw::DECLSPEC
                        | kw::CDECL
                        | kw::STDCALL
                        | kw::FASTCALL
                        | kw::VECTORCALL
                        | kw::THISCALL
                        | kw::REGCALL
                        | kw::CDECL2
                        | kw::STDCALL2
                        | kw::FASTCALL2 => {
                            let mut a = Attrs::default();
                            self.attributes(&mut a)?;
                            if a.cc.is_some() {
                                cc = a.cc;
                            }
                            a.cc = None;
                            attrs.merge(&a);
                            continue;
                        }
                        _ => break,
                    }
                    self.bump()?;
                }
                ptrs.push(if block {
                    Chunk::Block { konst, null }
                } else {
                    Chunk::Pointer {
                        konst,
                        cc,
                        bytes,
                        null,
                    }
                });
                continue;
            }
            // A calling convention before a nested declarator's name.
            if t.kind == Kind::Ident
                && matches!(
                    t.sym,
                    kw::CDECL
                        | kw::STDCALL
                        | kw::FASTCALL
                        | kw::VECTORCALL
                        | kw::THISCALL
                        | kw::REGCALL
                        | kw::CDECL2
                        | kw::STDCALL2
                        | kw::FASTCALL2
                        | kw::ATTRIBUTE
                        | kw::ATTRIBUTE2
                        | kw::DECLSPEC
                        | kw::NONNULL
                        | kw::NULLABLE
                        | kw::NULL_UNSPECIFIED
                        | kw::NULLABLE_RESULT
                        | kw::PTR32
                        | kw::PTR64
                        | kw::UNALIGNED
                )
            {
                let mut a = Attrs::default();
                if matches!(
                    t.sym,
                    kw::NONNULL
                        | kw::NULLABLE
                        | kw::NULL_UNSPECIFIED
                        | kw::NULLABLE_RESULT
                        | kw::PTR32
                        | kw::PTR64
                        | kw::UNALIGNED
                ) {
                    self.bump()?;
                } else {
                    self.attributes(&mut a)?;
                }
                attrs.merge(&a);
                continue;
            }
            break;
        }
        let t = self.peek()?;
        let (name, inner, inner_attrs) = if self.is_p(t, "(") && self.is_nested_declarator()? {
            self.bump()?;
            let d = self.declarator()?;
            self.expect(")")?;
            (d.name, d.chunks, d.attrs)
        } else if t.kind == Kind::Ident && !is_reserved(t.sym) {
            self.bump()?;
            (
                Some((self.name(t.sym).to_owned(), t.sym, t.loc)),
                Vec::new(),
                Attrs::default(),
            )
        } else {
            (None, Vec::new(), Attrs::default())
        };
        let mut suffix = Vec::new();
        loop {
            let t = self.peek()?;
            if self.is_p(t, "[") {
                self.bump()?;
                // `static`, qualifiers in parameter arrays.
                while matches!(
                    self.peek()?.sym,
                    kw::STATIC
                        | kw::CONST
                        | kw::CONST2
                        | kw::CONST3
                        | kw::RESTRICT
                        | kw::RESTRICT2
                        | kw::RESTRICT3
                        | kw::VOLATILE
                        | kw::NONNULL
                        | kw::NULLABLE
                        | kw::NULL_UNSPECIFIED
                        | kw::NULLABLE_RESULT
                ) && self.peek()?.kind == Kind::Ident
                {
                    self.bump()?;
                }
                if self.eat("]")? {
                    suffix.push(Chunk::Array(None));
                    continue;
                }
                if self.at("*")? && self.second_is("]")? {
                    self.bump()?;
                    self.bump()?;
                    suffix.push(Chunk::Array(None));
                    continue;
                }
                let lt = self.peek()?;
                let e = self.expr()?;
                self.expect("]")?;
                let e = self.rvalue(&e);
                let n = match e.v {
                    Val::Int(n) if n >= 0 => Some(n as u64),
                    Val::Int(_) => {
                        return Err(self.err(lt.loc, "array has a negative size"));
                    }
                    // A variable length array: only in parameters, where
                    // it decays.
                    _ if self.params_depth > 0 => None,
                    _ if self.bodies_on && !self.scopes.is_empty() => {
                        return Err(self.err(
                            lt.loc,
                            "variable length arrays are not supported by `nabi cc`; give the \
                             array a constant size, or allocate it: \
                             `int *a = malloc(n * sizeof *a);` .. `free(a);`",
                        ));
                    }
                    _ => {
                        return Err(self.err(
                            lt.loc,
                            "the array size is not an integer constant expression bindgen can \
                             evaluate; write it with integer literals, enum constants, casts, \
                             sizeof, _Alignof and offsetof",
                        ));
                    }
                };
                suffix.push(Chunk::Array(n));
                continue;
            }
            if self.is_p(t, "(") {
                self.bump()?;
                let f = self.params()?;
                suffix.push(f);
                continue;
            }
            break;
        }
        // Attributes after the declarator's name or parameters.
        self.attributes(&mut attrs)?;
        attrs.merge(&inner_attrs);
        let mut chunks = inner;
        chunks.extend(suffix);
        chunks.extend(ptrs.into_iter().rev());
        Ok(Declarator {
            name,
            chunks,
            attrs,
        })
    }

    /// A parameter list after its `(`, through the `)`.
    pub(crate) fn params(&mut self) -> R<Chunk> {
        self.params_depth += 1;
        let r = self.param_list();
        self.params_depth -= 1;
        r
    }

    pub(crate) fn param_list(&mut self) -> R<Chunk> {
        let mut params = Vec::new();
        let mut variadic = false;
        if self.eat(")")? {
            return Ok(Chunk::Func {
                params,
                variadic,
                proto: false,
            });
        }
        let hidden = self.hidden.len();
        loop {
            if self.eat("...")? {
                variadic = true;
                self.expect(")")?;
                break;
            }
            let t = self.peek()?;
            if t.kind == Kind::Ident
                && !self.starts_type(t)
                && (self.second_is(",")? || self.second_is(")")?)
            {
                return Err(self.err(
                    t.loc,
                    "K&R-style function definitions (`f(a, b) int a; ..`) are not supported; \
                     write a prototype: `int f(int a, int b)`",
                ));
            }
            let specs = self.specs(false)?;
            if !specs.has_type() {
                self.hidden.truncate(hidden);
                return Err(self.unexpected(t));
            }
            let base = self.base_type(&specs)?;
            let d = self.declarator()?;
            let mut attrs = specs.attrs.clone();
            attrs.merge(&d.attrs);
            let ty = self.apply(base, &specs, &d.chunks, &attrs);
            // `(void)`: no parameters.
            if params.is_empty()
                && d.name.is_none()
                && d.chunks.is_empty()
                && self.types.is_void(ty.ty)
                && self.at(")")?
            {
                self.bump()?;
                self.hidden.truncate(hidden);
                return Ok(Chunk::Func {
                    params,
                    variadic: false,
                    proto: true,
                });
            }
            let ty = self.decay(ty);
            if let Some((_, s, _)) = &d.name {
                self.hidden.push((*s, ty));
            }
            params.push((d.name.map(|n| n.0), ty));
            if self.eat(",")? {
                continue;
            }
            self.expect(")")?;
            break;
        }
        self.hidden.truncate(hidden);
        Ok(Chunk::Func {
            params,
            variadic,
            proto: true,
        })
    }

    /// A parameter's type: arrays and functions become pointers.
    pub(crate) fn decay(&mut self, q: QualType) -> QualType {
        match self.types.get(self.types.canon(q.ty)).clone() {
            Type::Array { elem, .. } => QualType::new(self.types.pointer(elem)),
            Type::Func(_) => QualType::new(self.types.pointer(q)),
            _ => q,
        }
    }

    /// The type of a declarator over `base`.
    pub(crate) fn apply(
        &mut self,
        base: QualType,
        specs: &Specs,
        chunks: &[Chunk],
        attrs: &Attrs,
    ) -> QualType {
        let base = QualType {
            ty: self.mode(base.ty, attrs),
            ..base
        };
        // Calling conventions: one on a pointer goes to the function type
        // outside it; one in the specifiers or after the name to the
        // innermost function.
        let mut ccs: Vec<Option<CallConv>> = vec![None; chunks.len()];
        for (i, c) in chunks.iter().enumerate() {
            if let Chunk::Pointer { cc: Some(cc), .. } = c {
                if let Some(j) =
                    (i + 1..chunks.len()).find(|&j| matches!(chunks[j], Chunk::Func { .. }))
                {
                    ccs[j] = Some(*cc);
                }
            }
        }
        if let Some(cc) = attrs.cc.or(specs.attrs.cc) {
            if let Some(j) = chunks.iter().position(|c| matches!(c, Chunk::Func { .. })) {
                if ccs[j].is_none() {
                    ccs[j] = Some(cc);
                }
            } else if let Type::Func(f) = self.types.get(self.types.canon(base.ty)).clone() {
                // A function declared through a typedef: the convention
                // makes a new function type.
                let mut f = f;
                f.cc = cc;
                let t = self.types.add(Type::Func(f));
                return QualType::new(t);
            }
        }
        let mut q = base;
        // Specifier nullability is the outermost pointer's (`_Nullable id`
        // is the typedef's).
        let outer = chunks
            .iter()
            .position(|c| matches!(c, Chunk::Pointer { .. } | Chunk::Block { .. }));
        if self.objc.on && outer.is_none() {
            if let Some(n) = specs.null {
                self.objc.null.insert(q.ty, n);
            }
        }
        for (i, c) in chunks.iter().enumerate().rev() {
            q = match c {
                Chunk::Pointer {
                    konst,
                    bytes: None,
                    null,
                    ..
                } => {
                    let ty = self.types.add(Type::Pointer(q));
                    self.objc_null(ty, *null, specs, outer == Some(i));
                    QualType { ty, konst: *konst }
                }
                Chunk::Pointer {
                    konst,
                    bytes: Some(n),
                    ..
                } => QualType {
                    ty: self.types.add(Type::SizedPointer(q, *n)),
                    konst: *konst,
                },
                Chunk::Block { konst, null } => {
                    let ty = self.types.add(Type::Block(q));
                    self.objc_null(ty, *null, specs, outer == Some(i));
                    QualType { ty, konst: *konst }
                }
                Chunk::Array(n) => QualType::new(self.types.add(Type::Array { elem: q, len: *n })),
                Chunk::Func {
                    params,
                    variadic,
                    proto,
                    ..
                } => QualType::new(self.types.add(Type::Func(Func {
                    ret: q,
                    params: params.iter().map(|p| p.1).collect(),
                    variadic: *variadic,
                    proto: *proto,
                    cc: ccs[i].unwrap_or(CallConv::C),
                }))),
            };
        }
        if let Some((n, attr)) = attrs.vector {
            let (s, _) = self.types.size_align(q.ty).unwrap_or((1, 1));
            let len = if attr == "vector_size" {
                n / s.max(1)
            } else {
                n
            };
            q = QualType::new(self.types.add(Type::Vector {
                elem: q.ty,
                len,
                attr,
            }));
        }
        q
    }

    /// A type name (in casts, `sizeof`, `_Atomic(..)`).
    pub fn type_name(&mut self) -> R<QualType> {
        let t = self.peek()?;
        let specs = self.specs(false)?;
        if !specs.has_type() {
            return Err(self.unexpected(t));
        }
        let base = self.base_type(&specs)?;
        let d = self.declarator()?;
        let mut attrs = specs.attrs.clone();
        attrs.merge(&d.attrs);
        Ok(self.apply(base, &specs, &d.chunks, &attrs))
    }
}

/// Keywords that cannot be a declarator's name.
pub(crate) const fn is_reserved(s: Sym) -> bool {
    s <= kw::LAST_KEYWORD
}
