//! Struct, union and enum specifiers.

#[allow(clippy::wildcard_imports)]
use crate::*;

impl Parser<'_, '_> {
    /// `struct` / `union` from the keyword: its type, and the tag if this
    /// declares or defines it.
    pub(crate) fn record_spec(&mut self, member: bool) -> R<((TypeId, RecordId), Owned)> {
        let kw_tok = self.bump()?;
        let union = kw_tok.sym == kw::UNION;
        let mut attrs = Attrs::default();
        self.attributes(&mut attrs)?;
        let name_tok = self.peek()?;
        let name = if name_tok.kind == Kind::Ident && !self.is_p(name_tok, "{") {
            self.bump()?;
            Some(name_tok)
        } else {
            None
        };
        self.attributes(&mut attrs)?;
        let def = self.at("{")?;
        let loc = name.map_or(kw_tok.loc, |n| n.loc);
        let alone = !def && self.at(";")? && !member;
        let (rid, owned) = if let Some(n) = name {
            match self.tags.get(&n.sym).copied() {
                Some(Tag::Record(r)) if !(def && self.types.records[r as usize].def.is_some()) => {
                    (r, def || alone)
                }
                Some(Tag::Enum(_)) => {
                    return Err(self.err(
                        n.loc,
                        &format!(
                            "use of '{}' with tag type that does not match previous declaration",
                            self.name(n.sym)
                        ),
                    ));
                }
                _ => {
                    let r = self.new_record(Some(self.name(n.sym).to_owned()), union);
                    self.bind_tag(n.sym, Tag::Record(r));
                    (r, true)
                }
            }
        } else {
            if !def {
                let t = self.peek()?;
                return Err(self.err(t.loc, "expected a name or '{' after 'struct' or 'union'"));
            }
            (self.new_record(None, union), true)
        };
        if def {
            self.record_body(rid, &attrs)?;
        }
        let ty = self.types.add(Type::Record(rid));
        Ok(((ty, rid), owned.then_some((Tag::Record(rid), loc))))
    }

    pub(crate) fn new_record(&mut self, name: Option<String>, union: bool) -> RecordId {
        self.types.records.push(Record {
            name,
            union,
            def: None,
            typedef_name: None,
            builtin: false,
        });
        (self.types.records.len() - 1) as RecordId
    }

    pub(crate) fn record_body(&mut self, rid: RecordId, pre: &Attrs) -> R<()> {
        self.bump()?;
        let pack = self.pack;
        let mut fields = Vec::new();
        loop {
            let t = self.peek()?;
            if self.is_p(t, "}") {
                self.bump()?;
                break;
            }
            if t.kind == Kind::Eof {
                return Err(self.err(t.loc, "expected '}' to end the struct"));
            }
            if self.eat(";")? {
                continue;
            }
            if matches!(t.sym, kw::STATIC_ASSERT2 | kw::STATIC_ASSERT) && t.kind == Kind::Ident {
                self.static_assert()?;
                continue;
            }
            self.member(&mut fields)?;
        }
        let mut attrs = pre.clone();
        self.attributes(&mut attrs)?;
        let rec = &mut self.types.records[rid as usize];
        rec.def = Some(RecordDef {
            fields,
            packed: attrs.packed,
            align: attrs.aligned,
            pack,
        });
        self.types.reset_layout(rid);
        Ok(())
    }

    pub(crate) fn member(&mut self, fields: &mut Vec<Field>) -> R<()> {
        let specs = self.specs(true)?;
        if !specs.has_type() {
            let t = self.peek()?;
            return Err(self.unexpected(t));
        }
        let base = self.base_type(&specs)?;
        if self.at(";")? {
            // An anonymous member: an unnamed struct or union (C11), or
            // with Microsoft's extensions any struct or union.
            let canon = self.types.canon(base.ty);
            let is_record = matches!(self.types.get(canon), Type::Record(_));
            let unnamed = matches!(self.types.get(base.ty), Type::Record(r)
                if self.types.records[*r as usize].name.is_none());
            if is_record && (unnamed || self.types.t.msvc) {
                fields.push(Field {
                    name: None,
                    ty: base,
                    bits: None,
                    packed: specs.attrs.packed,
                    align: specs.attrs.aligned,
                    loc: specs.loc,
                    anon: true,
                });
            }
            self.bump()?;
            return Ok(());
        }
        loop {
            let mut pre = Attrs::default();
            self.attributes(&mut pre)?;
            let d = if self.at(":")? {
                Declarator {
                    name: None,
                    chunks: Vec::new(),
                    attrs: Attrs::default(),
                }
            } else {
                self.declarator()?
            };
            let mut attrs = specs.attrs.clone();
            attrs.merge(&pre);
            attrs.merge(&d.attrs);
            let bits = if self.eat(":")? {
                let t = self.peek()?;
                let e = self.cond_expr()?;
                Some(self.int_value(&e, t.loc, "the bit-field width")? as u64)
            } else {
                None
            };
            let mut after = Attrs::default();
            self.attributes(&mut after)?;
            attrs.merge(&after);
            let ty = self.apply(base, &specs, &d.chunks, &attrs);
            let loc = d.name.as_ref().map_or(specs.loc, |n| n.2);
            fields.push(Field {
                name: d.name.map(|n| n.0),
                ty,
                bits,
                packed: attrs.packed,
                align: attrs.aligned,
                loc,
                anon: false,
            });
            if self.eat(",")? {
                continue;
            }
            let t = self.peek()?;
            if self.is_p(t, ";") {
                self.bump()?;
                return Ok(());
            }
            if self.is_p(t, "}") {
                return Ok(());
            }
            return Err(self.err(
                t.loc,
                &format!(
                    "expected ';' at the end of the struct member, found {}",
                    self.describe(t)
                ),
            ));
        }
    }

    pub(crate) fn enum_spec(&mut self) -> R<(TypeId, Owned)> {
        let kw_tok = self.bump()?;
        let mut attrs = Attrs::default();
        self.attributes(&mut attrs)?;
        let name_tok = self.peek()?;
        let name = if name_tok.kind == Kind::Ident {
            self.bump()?;
            Some(name_tok)
        } else {
            None
        };
        self.attributes(&mut attrs)?;
        // The underlying type is specifiers only: `enum E : CFIndex E;`
        // declares `E`.
        let fixed = if self.eat(":")? {
            let t = self.peek()?;
            let sp = self.specs(false)?;
            if !sp.has_type() {
                return Err(self.unexpected(t));
            }
            Some(self.base_type(&sp)?.ty)
        } else {
            None
        };
        let def = self.at("{")?;
        let loc = name.map_or(kw_tok.loc, |n| n.loc);
        let alone = !def && self.at(";")?;
        let (eid, owned) = if let Some(n) = name {
            match self.tags.get(&n.sym).copied() {
                Some(Tag::Enum(e)) if !(def && self.types.enums[e as usize].complete) => {
                    (e, def || alone)
                }
                Some(Tag::Record(_)) => {
                    return Err(self.err(
                        n.loc,
                        &format!(
                            "use of '{}' with tag type that does not match previous declaration",
                            self.name(n.sym)
                        ),
                    ));
                }
                _ => {
                    let e = self.new_enum(Some(self.name(n.sym).to_owned()));
                    self.bind_tag(n.sym, Tag::Enum(e));
                    (e, true)
                }
            }
        } else {
            if !def {
                let t = self.peek()?;
                return Err(self.err(t.loc, "expected a name or '{' after 'enum'"));
            }
            (self.new_enum(None), true)
        };
        if let Some(f) = fixed {
            let en = &mut self.types.enums[eid as usize];
            en.fixed = true;
            en.int = Some(f);
        }
        if def {
            self.enum_body(eid, &mut attrs)?;
        }
        if self.objc.on && (attrs.flag_enum || attrs.closed_enum || attrs.open_enum) {
            let k = if attrs.flag_enum {
                objc::EnumKind::Options
            } else if attrs.closed_enum {
                objc::EnumKind::Closed
            } else {
                objc::EnumKind::Open
            };
            self.objc.enums.entry(eid).or_insert(k);
        }
        let ty = self.types.add(Type::Enum(eid));
        Ok((ty, owned.then_some((Tag::Enum(eid), loc))))
    }

    pub(crate) fn new_enum(&mut self, name: Option<String>) -> EnumId {
        self.types.enums.push(Enum {
            name,
            typedef_name: None,
            int: None,
            fixed: false,
            values: Vec::new(),
            complete: false,
        });
        (self.types.enums.len() - 1) as EnumId
    }

    pub(crate) fn enum_body(&mut self, eid: EnumId, attrs: &mut Attrs) -> R<()> {
        self.bump()?;
        let mut next: i128 = 0;
        loop {
            let t = self.peek()?;
            if self.is_p(t, "}") {
                self.bump()?;
                break;
            }
            if t.kind != Kind::Ident {
                return Err(self.err(
                    t.loc,
                    &format!("expected an enumerator name, found {}", self.describe(t)),
                ));
            }
            self.bump()?;
            let mut a = Attrs::default();
            self.attributes(&mut a)?;
            if self.eat("=")? {
                let vt = self.peek()?;
                let e = self.cond_expr()?;
                let e = self.rvalue(&e);
                let v = self.int_value(&e, vt.loc, "the enumerator value")?;
                next = v;
            }
            // A fixed type (MSVC's enums are `int`) holds the value as it
            // converts.
            let fixed = {
                let en = &self.types.enums[eid as usize];
                if en.fixed {
                    en.int.and_then(|i| self.types.prim_of(i))
                } else if self.types.t.msvc {
                    Some(Prim::Int)
                } else {
                    None
                }
            };
            if let Some(p) = fixed {
                next = self.wrap(p, next);
            }
            let idx = self.types.enums[eid as usize].values.len() as u32;
            let ename = self.name(t.sym).to_owned();
            self.types.enums[eid as usize].values.push(Enumerator {
                name: ename,
                value: next,
                loc: t.loc,
            });
            self.bind(t.sym, Ord::Enumerator(eid, idx));
            next += 1;
            if !self.eat(",")? {
                self.expect("}")?;
                break;
            }
        }
        self.attributes(attrs)?;
        self.finish_enum(eid, attrs.packed);
        Ok(())
    }

    /// The integer type of a completed enum, as clang picks it.
    pub(crate) fn finish_enum(&mut self, eid: EnumId, packed: bool) {
        let t = &self.types.t;
        let en = &self.types.enums[eid as usize];
        let int = if en.fixed {
            en.int.expect("a fixed type")
        } else if t.msvc {
            self.types.prim(Prim::Int)
        } else {
            let neg = en.values.iter().any(|v| v.value < 0);
            let (mut pos_bits, mut neg_bits) = (0u32, 0u32);
            for v in &en.values {
                if v.value >= 0 {
                    pos_bits = pos_bits.max(128 - v.value.leading_zeros());
                } else {
                    neg_bits = neg_bits.max(128 - (!v.value).leading_zeros() + 1);
                }
            }
            let long = (t.long * 8) as u32;
            let p = if neg {
                if packed && neg_bits <= 8 && pos_bits < 8 {
                    Prim::SChar
                } else if packed && neg_bits <= 16 && pos_bits < 16 {
                    Prim::Short
                } else if neg_bits <= 32 && pos_bits < 32 {
                    Prim::Int
                } else if neg_bits <= long && pos_bits < long {
                    Prim::Long
                } else {
                    Prim::LongLong
                }
            } else if packed && pos_bits <= 8 {
                Prim::UChar
            } else if packed && pos_bits <= 16 {
                Prim::UShort
            } else if pos_bits <= 32 {
                Prim::UInt
            } else if pos_bits <= long {
                Prim::ULong
            } else {
                Prim::ULongLong
            };
            self.types.prim(p)
        };
        let en = &mut self.types.enums[eid as usize];
        en.int = Some(int);
        en.complete = true;
    }
}
