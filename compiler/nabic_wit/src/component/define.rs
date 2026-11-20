//! Component types: value types, definitions, function types.

#[allow(clippy::wildcard_imports)]
use crate::component::*;

impl Enc<'_> {
    pub(crate) fn val(&mut self, sp: &mut Space, v: &Val) -> Res<Vt> {
        let m = self.m;
        match v {
            Val::Prim(p) => Ok(Vt::P(prim_byte(*p))),
            Val::ErrorContext => Ok(Vt::P(0x64)),
            Val::Id(t) => {
                if m.tys[*t].name.is_some() {
                    return self.named(sp, *t).map(Vt::T);
                }
                if let Def::Alias(a) = &m.tys[*t].def {
                    return self.val(sp, a);
                }
                let b = self.def_bytes(sp, *t)?;
                Ok(Vt::T(self.define(sp, b)))
            }
        }
    }

    pub(crate) fn opt_val(
        &mut self,
        sp: &mut Space,
        v: Option<&Val>,
        out: &mut Vec<u8>,
    ) -> Res<()> {
        match v {
            Some(v) => {
                out.push(1);
                self.val(sp, v)?.put(out);
            }
            None => out.push(0),
        }
        Ok(())
    }

    /// The `defvaltype` of `t` (not an alias or resource).
    pub(crate) fn def_bytes(&mut self, sp: &mut Space, t: Tid) -> Res<Vec<u8>> {
        let m = self.m;
        let mut b = Vec::new();
        let labels = |b: &mut Vec<u8>, ls: &[String]| {
            bin::usize(b, ls.len());
            for l in ls {
                bin::name(b, l);
            }
        };
        match &m.tys[t].def {
            Def::Record(fs) => {
                b.push(0x72);
                bin::usize(&mut b, fs.len());
                for (n, v) in fs {
                    bin::name(&mut b, n);
                    self.val(sp, v)?.put(&mut b);
                }
            }
            Def::Variant(cs) => {
                b.push(0x71);
                bin::usize(&mut b, cs.len());
                for (n, v) in cs {
                    bin::name(&mut b, n);
                    self.opt_val(sp, v.as_ref(), &mut b)?;
                    b.push(0);
                }
            }
            Def::Enum(cs) => {
                b.push(0x6d);
                labels(&mut b, cs);
            }
            Def::Flags(fs) => {
                b.push(0x6e);
                labels(&mut b, fs);
            }
            Def::List(v) => {
                b.push(0x70);
                self.val(sp, v)?.put(&mut b);
            }
            Def::FixedList(v, n) => {
                b.push(0x67);
                self.val(sp, v)?.put(&mut b);
                bin::u32(&mut b, *n);
            }
            Def::Tuple(ts) => {
                b.push(0x6f);
                bin::usize(&mut b, ts.len());
                for v in ts {
                    self.val(sp, v)?.put(&mut b);
                }
            }
            Def::Option(v) => {
                b.push(0x6b);
                self.val(sp, v)?.put(&mut b);
            }
            Def::Result(x, y) => {
                b.push(0x6a);
                self.opt_val(sp, x.as_ref(), &mut b)?;
                self.opt_val(sp, y.as_ref(), &mut b)?;
            }
            Def::Own(r) | Def::Borrow(r) => {
                b.push(if matches!(m.tys[t].def, Def::Own(_)) {
                    0x69
                } else {
                    0x68
                });
                let i = self.named(sp, *r)?;
                bin::u32(&mut b, i);
            }
            Def::Future(v) => {
                b.push(0x65);
                self.opt_val(sp, v.as_ref(), &mut b)?;
            }
            Def::Stream(v) => {
                b.push(0x66);
                self.opt_val(sp, v.as_ref(), &mut b)?;
            }
            Def::Alias(_) | Def::Resource => {
                return Err("internal: a named type defined anonymously".to_owned());
            }
        }
        Ok(b)
    }

    /// A type definition in `sp`, shared with an identical one.
    pub(crate) fn define(&mut self, sp: &mut Space, b: Vec<u8>) -> u32 {
        match sp {
            Space::Outer => {
                if let Some(&i) = self.outer_anon.get(&b) {
                    return i;
                }
                let i = self.b.ty(&b);
                self.outer_anon.insert(b, i);
                i
            }
            Space::Inst(s) => {
                if let Some(&i) = s.anon.get(&b) {
                    return i;
                }
                s.decls.push(0x01);
                s.decls.extend_from_slice(&b);
                s.n += 1;
                s.types += 1;
                s.anon.insert(b, s.types - 1);
                s.types - 1
            }
            Space::Nest(s) => {
                if let Some(&i) = s.anon.get(&b) {
                    return i;
                }
                let i = s.b.ty(&b);
                s.anon.insert(b, i);
                i
            }
        }
    }

    /// The structure named type `t` is equal to, as a type index.
    pub(crate) fn eq_idx(&mut self, sp: &mut Space, t: Tid) -> Res<u32> {
        let m = self.m;
        match &m.tys[t].def {
            Def::Alias(v) => match self.val(sp, v)? {
                Vt::T(i) => Ok(i),
                Vt::P(p) => Ok(self.define(sp, vec![p])),
            },
            Def::Resource => Err("internal: a resource as a type bound".to_owned()),
            _ => {
                let b = self.def_bytes(sp, t)?;
                Ok(self.define(sp, b))
            }
        }
    }

    /// Index of named type `t` in `sp`, made on first use.
    pub(crate) fn named(&mut self, sp: &mut Space, t: Tid) -> Res<u32> {
        let m = self.m;
        let t = self.g.canon(m, t);
        let key = self
            .g
            .named(m, t)
            .ok_or("internal: an anonymous type named")?;
        let resource = matches!(m.tys[t].def, Def::Resource);
        match sp {
            Space::Outer => self.outer(t, key),
            Space::Inst(s) => {
                if let Some(&i) = s.named.get(&key) {
                    return Ok(i);
                }
                let group = s.group;
                if key.0 == group {
                    let bound = if resource {
                        None
                    } else {
                        Some(self.eq_idx(sp, t)?)
                    };
                    let Space::Inst(s) = sp else { unreachable!() };
                    s.decls.extend_from_slice(&[0x04, 0x00]);
                    bin::name(&mut s.decls, &key.1);
                    s.decls.extend_from_slice(&type_bound(bound));
                    s.n += 1;
                } else {
                    let o = self.named(&mut Space::Outer, t)?;
                    let Space::Inst(s) = sp else { unreachable!() };
                    s.decls.extend_from_slice(&[0x02, 0x03, 0x02, 0x01]);
                    bin::u32(&mut s.decls, o);
                    s.n += 1;
                }
                let Space::Inst(s) = sp else { unreachable!() };
                s.types += 1;
                s.named.insert(key, s.types - 1);
                Ok(s.types - 1)
            }
            Space::Nest(s) => {
                if s.exporting && key.0 == s.group {
                    if let Some(&i) = s.exports.get(&key) {
                        return Ok(i);
                    }
                    // The interface's own type, exported.
                    let idx = match &m.tys[t].def {
                        Def::Resource => {
                            s.exporting = false;
                            let r = self.named(sp, t);
                            let Space::Nest(s) = sp else { unreachable!() };
                            s.exporting = true;
                            r?
                        }
                        _ => self.eq_idx(sp, t)?,
                    };
                    let Space::Nest(s) = sp else { unreachable!() };
                    let i = s.b.export(&key.1, TYPE, idx, None);
                    s.exports.insert(key, i);
                    return Ok(i);
                }
                if let Some(&i) = s.imports.get(&key) {
                    return Ok(i);
                }
                let was = s.exporting;
                s.exporting = false;
                let bound = if resource {
                    Ok(None)
                } else {
                    self.eq_idx(sp, t).map(Some)
                };
                let Space::Nest(s) = sp else { unreachable!() };
                s.exporting = was;
                let bound = bound?;
                let outer = self.named(&mut Space::Outer, t)?;
                let Space::Nest(s) = sp else { unreachable!() };
                let name = s.uniq(&format!("import-type-{}", key.1));
                let i = s.b.import(&name, &type_bound(bound));
                s.args.push((name, TYPE, outer));
                s.imports.insert(key, i);
                Ok(i)
            }
        }
    }

    /// Named type `t` in the component itself.
    pub(crate) fn outer(&mut self, t: Tid, key: Key) -> Res<u32> {
        let m = self.m;
        if let Some(&i) = self.outer_named.get(&key) {
            return Ok(i);
        }
        let (g, name) = key.clone();
        let i = if self.exported.contains(&g) {
            if matches!(m.tys[t].def, Def::Resource) {
                let mut b = vec![0x3f, 0x7f];
                match self.dtors.get(&key) {
                    Some(&slot) => {
                        let shim = self.shim.ok_or("internal: no shim for a destructor")?;
                        let f = self.b.alias_core(shim, &slot.to_string(), CORE_FUNC);
                        b.push(1);
                        bin::u32(&mut b, f);
                    }
                    None => b.push(0),
                }
                self.b.ty(&b)
            } else {
                self.eq_idx(&mut Space::Outer, t)?
            }
        } else if Some(g) == self.root {
            if matches!(m.tys[t].def, Def::Resource) {
                return Err(format!("world-level resource `{name}` is not supported"));
            }
            let e = self.eq_idx(&mut Space::Outer, t)?;
            let mut d = vec![0x03, 0x00];
            bin::u32(&mut d, e);
            self.b.import(&name, &d)
        } else {
            if !self.live_types.get(&g).is_some_and(|s| s.contains(&name)) {
                return Err(format!("internal: type `{name}` used but not imported"));
            }
            let inst = self.import_group(g)?;
            self.b.alias_export(inst, &name, TYPE)
        };
        self.outer_named.insert(key, i);
        Ok(i)
    }

    pub(crate) fn functype(&mut self, sp: &mut Space, f: &Func) -> Res<u32> {
        let mut b = vec![if f.is_async { 0x43 } else { 0x40 }];
        bin::usize(&mut b, f.params.len());
        for (n, v) in &f.params {
            bin::name(&mut b, n);
            self.val(sp, v)?.put(&mut b);
        }
        match &f.result {
            Some(v) => {
                b.push(0);
                self.val(sp, v)?.put(&mut b);
            }
            None => b.extend_from_slice(&[1, 0]),
        }
        Ok(self.define(sp, b))
    }
}
