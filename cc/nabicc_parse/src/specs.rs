//! Declaration specifiers and attributes.

#[allow(clippy::wildcard_imports)]
use crate::*;

impl Parser<'_, '_> {
    pub(crate) fn is_typedef_name(&self, s: Sym) -> bool {
        !self.hidden.iter().any(|h| h.0 == s) && matches!(self.ords.get(&s), Some(Ord::Typedef(_)))
    }

    /// Could `t` start a type name?
    pub(crate) fn starts_type(&self, t: Tok) -> bool {
        if t.kind != Kind::Ident {
            return false;
        }
        let s = t.sym;
        matches!(
            s,
            kw::VOID
                | kw::CHAR
                | kw::SHORT
                | kw::INT
                | kw::LONG
                | kw::FLOAT
                | kw::DOUBLE
                | kw::SIGNED
                | kw::SIGNED2
                | kw::SIGNED3
                | kw::UNSIGNED
                | kw::BOOL2
                | kw::COMPLEX
                | kw::STRUCT
                | kw::UNION
                | kw::ENUM
                | kw::CONST
                | kw::CONST2
                | kw::CONST3
                | kw::VOLATILE
                | kw::VOLATILE2
                | kw::VOLATILE3
                | kw::RESTRICT
                | kw::RESTRICT2
                | kw::RESTRICT3
                | kw::ATOMIC
                | kw::TYPEOF
                | kw::TYPEOF2
                | kw::TYPEOF3
                | kw::TYPEOF_UNQUAL
                | kw::TYPEOF_UNQUAL2
                | kw::INT128
                | kw::INT64
                | kw::INT32
                | kw::INT16
                | kw::INT8
                | kw::FLOAT16
                | kw::FP16
                | kw::BF16
                | kw::FLOAT128
                | kw::IBM128
                | kw::ATTRIBUTE
                | kw::ATTRIBUTE2
                | kw::NONNULL
                | kw::NULLABLE
                | kw::NULL_UNSPECIFIED
                | kw::NULLABLE_RESULT
                | kw::ALIGNAS2
        ) || self.is_typedef_name(s)
    }

    pub(crate) fn specs(&mut self, member: bool) -> R<Specs> {
        let mut sp = Specs {
            loc: self.peek()?.loc,
            ..Specs::default()
        };
        loop {
            let t = self.peek()?;
            if t.kind != Kind::Ident {
                if self.is_p(t, "[") && self.second_is("[")? {
                    self.std_attributes()?;
                    continue;
                }
                break;
            }
            let s = t.sym;
            match s {
                kw::TYPEDEF => sp.storage = Storage::Typedef,
                kw::EXTERN => sp.storage = Storage::Extern,
                kw::STATIC => sp.storage = Storage::Static,
                kw::AUTO | kw::REGISTER => sp.storage = Storage::Auto,
                kw::THREAD_LOCAL2 | kw::THREAD => sp.tls = true,
                kw::INLINE | kw::INLINE2 | kw::INLINE3 | kw::FORCEINLINE => sp.inline = true,
                kw::CONST | kw::CONST2 | kw::CONST3 => sp.konst = true,
                kw::NORETURN
                | kw::EXTENSION
                | kw::VOLATILE
                | kw::VOLATILE2
                | kw::VOLATILE3
                | kw::RESTRICT
                | kw::RESTRICT2
                | kw::RESTRICT3
                | kw::PTR32
                | kw::PTR64
                | kw::UNALIGNED
                | kw::W64
                | kw::SPTR
                | kw::UPTR => {}
                kw::NONNULL | kw::NULLABLE | kw::NULL_UNSPECIFIED | kw::NULLABLE_RESULT => {
                    sp.null = objc::Null::of(s);
                }
                _ if self.objc.on && s == self.objc.kindof => {}
                kw::CDECL
                | kw::STDCALL
                | kw::FASTCALL
                | kw::VECTORCALL
                | kw::THISCALL
                | kw::REGCALL
                | kw::CDECL2
                | kw::STDCALL2
                | kw::FASTCALL2 => sp.attrs.cc = Some(cc_keyword(s)),
                kw::ATTRIBUTE | kw::ATTRIBUTE2 | kw::DECLSPEC => {
                    let mut a = Attrs::default();
                    self.attributes(&mut a)?;
                    sp.attrs.merge(&a);
                    continue;
                }
                kw::ALIGNAS2 => {
                    self.bump()?;
                    let a = self.alignas()?;
                    sp.attrs.aligned = sp.attrs.aligned.max(a);
                    continue;
                }
                kw::VOID => sp.void = true,
                kw::BOOL2 => sp.bool_ = true,
                kw::CHAR => sp.char_ = true,
                kw::SHORT => sp.short = true,
                kw::INT => sp.int = true,
                kw::LONG => sp.long += 1,
                kw::SIGNED | kw::SIGNED2 | kw::SIGNED3 => sp.signed = true,
                kw::UNSIGNED => sp.unsigned = true,
                kw::FLOAT => sp.float = true,
                kw::DOUBLE => sp.double = true,
                kw::COMPLEX => sp.complex = true,
                kw::INT128 => sp.int128 = true,
                kw::INT64 => sp.ms_int = Some(Prim::LongLong),
                kw::INT32 => sp.ms_int = Some(Prim::Int),
                kw::INT16 => sp.ms_int = Some(Prim::Short),
                kw::INT8 => sp.ms_int = Some(Prim::Char),
                kw::FLOAT16 => sp.float16 = true,
                kw::FP16 => sp.fp16 = true,
                kw::BF16 => sp.bf16 = true,
                kw::FLOAT128 => sp.float128 = true,
                kw::IBM128 => sp.ibm128 = true,
                kw::ATOMIC => {
                    self.bump()?;
                    if self.at("(")? {
                        self.bump()?;
                        let q = self.type_name()?;
                        self.expect(")")?;
                        sp.base = Some(self.types.add(Type::Atomic(q)));
                    }
                    continue;
                }
                kw::TYPEOF | kw::TYPEOF2 | kw::TYPEOF3 | kw::TYPEOF_UNQUAL | kw::TYPEOF_UNQUAL2 => {
                    self.bump()?;
                    self.expect("(")?;
                    let q = if self.next_starts_type()? {
                        self.type_name()?
                    } else {
                        self.expr()?.ty
                    };
                    self.expect(")")?;
                    sp.base = Some(self.types.add(Type::TypeOf(q)));
                    continue;
                }
                kw::STRUCT | kw::UNION => {
                    let (tag, owned) = self.record_spec(member)?;
                    sp.base = Some(tag.0);
                    if let Some(o) = owned {
                        sp.owned = Some(o);
                    }
                    continue;
                }
                kw::ENUM => {
                    let (tag, owned) = self.enum_spec()?;
                    sp.base = Some(tag);
                    if let Some(o) = owned {
                        sp.owned = Some(o);
                    }
                    continue;
                }
                _ if !sp.has_type() && self.is_typedef_name(s) => {
                    let Some(Ord::Typedef(d)) = self.ords.get(&s).copied() else {
                        unreachable!("a typedef name")
                    };
                    sp.base = Some(self.types.add(Type::Typedef(d)));
                    if self.objc.on && self.second_is("<")? {
                        self.bump()?;
                        sp.base = Some(self.objc_angles(d, sp.base.unwrap_or_default())?);
                        continue;
                    }
                }
                _ => break,
            }
            self.bump()?;
        }
        Ok(sp)
    }

    /// A C23 attribute, `[[..]]`.
    pub(crate) fn std_attributes(&mut self) -> R<()> {
        self.bump()?;
        self.skip_balanced()?;
        self.expect("]")?;
        Ok(())
    }

    /// `_Alignas(N)` / `_Alignas(type)` after the keyword.
    pub(crate) fn alignas(&mut self) -> R<u64> {
        self.expect("(")?;
        let a = if self.next_starts_type()? {
            let q = self.type_name()?;
            self.types.size_align(q.ty).map_or(1, |x| x.1)
        } else {
            let t = self.peek()?;
            let e = self.cond_expr()?;
            self.int_value(&e, t.loc, "_Alignas")? as u64
        };
        self.expect(")")?;
        Ok(a)
    }

    /// GNU `__attribute__((..))`, `__declspec(..)`, `[[..]]`, calling
    /// conventions and `__asm__("label")`, in any order.
    pub(crate) fn attributes(&mut self, a: &mut Attrs) -> R<()> {
        loop {
            let t = self.peek()?;
            match t.sym {
                kw::ATTRIBUTE | kw::ATTRIBUTE2 if t.kind == Kind::Ident => {
                    self.bump()?;
                    self.expect("(")?;
                    self.expect("(")?;
                    self.attribute_list(a, false)?;
                    self.expect(")")?;
                    self.expect(")")?;
                }
                kw::DECLSPEC if t.kind == Kind::Ident => {
                    self.bump()?;
                    self.expect("(")?;
                    self.attribute_list(a, true)?;
                    self.expect(")")?;
                }
                kw::ASM | kw::ASM2 | kw::ASM3 if t.kind == Kind::Ident => {
                    self.bump()?;
                    self.expect("(")?;
                    let mut label = String::new();
                    while self.peek()?.kind == Kind::Str {
                        let s = self.bump()?;
                        label.push_str(&String::from_utf8_lossy(&str_bytes(self.name(s.sym))));
                    }
                    self.expect(")")?;
                    a.asm = Some(label);
                }
                kw::CDECL
                | kw::STDCALL
                | kw::FASTCALL
                | kw::VECTORCALL
                | kw::THISCALL
                | kw::REGCALL
                | kw::CDECL2
                | kw::STDCALL2
                | kw::FASTCALL2
                    if t.kind == Kind::Ident =>
                {
                    self.bump()?;
                    a.cc = Some(cc_keyword(t.sym));
                }
                _ if self.is_p(t, "[") && self.second_is("[")? => {
                    self.std_attributes()?;
                }
                _ => return Ok(()),
            }
        }
    }

    pub(crate) fn attribute_list(&mut self, a: &mut Attrs, declspec: bool) -> R<()> {
        loop {
            let t = self.peek()?;
            if self.is_p(t, ")") {
                return Ok(());
            }
            if self.is_p(t, ",") {
                self.bump()?;
                continue;
            }
            if t.kind != Kind::Ident {
                return Err(self.err(
                    t.loc,
                    &format!("expected an attribute name, found {}", self.describe(t)),
                ));
            }
            self.bump()?;
            let raw = self.name(t.sym).to_owned();
            let name = raw
                .trim_start_matches("__")
                .trim_end_matches("__")
                .to_owned();
            let has_args = self.at("(")?;
            match name.as_str() {
                "packed" => a.packed = true,
                "aligned" | "align" if has_args => {
                    self.bump()?;
                    let e = self.cond_expr()?;
                    let v = self.int_value(&e, t.loc, "the alignment")? as u64;
                    a.aligned = a.aligned.max(v);
                    self.expect(")")?;
                    continue;
                }
                "aligned" => a.aligned = a.aligned.max(self.types.t.attr_align),
                "mode" if has_args => {
                    self.bump()?;
                    let m = self.bump()?;
                    a.mode = Some(
                        self.name(m.sym)
                            .trim_start_matches("__")
                            .trim_end_matches("__")
                            .to_owned(),
                    );
                    self.expect(")")?;
                    continue;
                }
                "vector_size" | "ext_vector_type" | "neon_vector_type" | "neon_polyvector_type"
                    if has_args =>
                {
                    self.bump()?;
                    let e = self.cond_expr()?;
                    let v = self.int_value(&e, t.loc, "the vector size")? as u64;
                    a.vector = Some((
                        v,
                        match name.as_str() {
                            "vector_size" => "vector_size",
                            "ext_vector_type" => "ext_vector_type",
                            "neon_vector_type" => "neon_vector_type",
                            _ => "neon_polyvector_type",
                        },
                    ));
                    self.expect(")")?;
                    continue;
                }
                "thread" if declspec => a.tls = true,
                "availability" if has_args => {
                    self.availability(&mut a.avail)?;
                    continue;
                }
                "unavailable" => a.avail.unavailable = true,
                "ns_returns_retained" | "cf_returns_retained" => a.retained = Some(true),
                "ns_returns_not_retained"
                | "cf_returns_not_retained"
                | "ns_returns_autoreleased" => {
                    a.retained = Some(false);
                }
                "ns_consumed" | "cf_consumed" => a.consumed = true,
                "ns_consumes_self" => a.consumes_self = true,
                "flag_enum" => a.flag_enum = true,
                "enum_extensibility" if has_args => {
                    self.bump()?;
                    let w = self.bump()?;
                    a.closed_enum = self.name(w.sym) == "closed";
                    a.open_enum = !a.closed_enum;
                    self.expect(")")?;
                    continue;
                }
                "stdcall" => a.cc = Some(CallConv::Stdcall),
                "cdecl" => a.cc = Some(CallConv::C),
                "fastcall" => a.cc = Some(CallConv::Fastcall),
                "vectorcall" => a.cc = Some(CallConv::Vectorcall),
                "thiscall" => a.cc = Some(CallConv::Thiscall),
                "regcall" => a.cc = Some(CallConv::Regcall),
                "ms_abi" => a.cc = Some(CallConv::Win64),
                "sysv_abi" => a.cc = Some(CallConv::SysV),
                n @ ("pcs" | "preserve_most" | "preserve_all" | "swiftcall"
                | "aarch64_vector_pcs" | "aarch64_sve_pcs" | "intel_ocl_bicc" | "pascal") => {
                    a.cc = Some(CallConv::Other(match n {
                        "pcs" => "pcs",
                        "preserve_most" => "preserve_most",
                        "preserve_all" => "preserve_all",
                        "swiftcall" => "swiftcall",
                        "aarch64_vector_pcs" => "aarch64_vector_pcs",
                        "aarch64_sve_pcs" => "aarch64_sve_pcs",
                        "intel_ocl_bicc" => "intel_ocl_bicc",
                        _ => "pascal",
                    }));
                }
                _ => {}
            }
            if self.at("(")? {
                self.skip_balanced()?;
            }
        }
    }
}

pub(crate) const fn cc_keyword(s: Sym) -> CallConv {
    match s {
        kw::STDCALL | kw::STDCALL2 => CallConv::Stdcall,
        kw::FASTCALL | kw::FASTCALL2 => CallConv::Fastcall,
        kw::VECTORCALL => CallConv::Vectorcall,
        kw::THISCALL => CallConv::Thiscall,
        kw::REGCALL => CallConv::Regcall,
        _ => CallConv::C,
    }
}
