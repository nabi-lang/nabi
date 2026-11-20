//! Canonical ABI code: lowering, lifting, loads and stores.

#[allow(clippy::wildcard_imports)]
use crate::bind::*;

/// One function body being generated.
pub(crate) struct Fx<'g, 'a> {
    pub(crate) g: &'g mut Gen<'a>,
    pub(crate) out: String,
    pub(crate) n: usize,
}

impl Fx<'_, '_> {
    pub(crate) fn tmp(&mut self) -> String {
        self.n += 1;
        format!("w_{}", self.n)
    }

    pub(crate) fn line(&mut self, s: &str) {
        self.out.push_str("        ");
        self.out.push_str(s);
        self.out.push('\n');
    }

    /// `let w_N: ty = expr;`, its name.
    pub(crate) fn let_(&mut self, ty: &str, expr: &str) -> String {
        let v = self.tmp();
        self.line(&format!("let {v}: {ty} = {expr};"));
        v
    }

    pub(crate) fn lower(&mut self, t: &Type, v: &str) -> Result<Vec<String>, String> {
        Ok(match self.g.view(t) {
            View::Prim(p) => match p {
                Prim::Bool => vec![self.let_("i32", &format!("if {v} {{ 1i32 }} else {{ 0i32 }}"))],
                Prim::U8 | Prim::U16 | Prim::U32 | Prim::S8 | Prim::S16 => {
                    vec![self.let_("i32", &format!("{v} as i32"))]
                }
                Prim::S32 => vec![self.let_("i32", v)],
                Prim::U64 => vec![self.let_("i64", &format!("{v} as i64"))],
                Prim::S64 => vec![self.let_("i64", v)],
                Prim::F32 => vec![self.let_("f32", v)],
                Prim::F64 => vec![self.let_("f64", v)],
                Prim::Char => vec![self.let_("i32", &format!("({v} as u32) as i32"))],
                Prim::String => {
                    let (p, n) = (self.tmp(), self.tmp());
                    self.line(&format!("let ({p}, {n}) = w_b.str({v});"));
                    vec![p, n]
                }
            },
            View::List(e) => {
                let (p, n) = self.lower_list(&e, v)?;
                vec![p, n]
            }
            View::Tuple(ts) => {
                let mut out = Vec::new();
                for (i, ft) in ts.iter().enumerate() {
                    let f = self.let_(&self.g.rty(ft)?, &format!("{v}.{i}"));
                    out.extend(self.lower(ft, &f)?);
                }
                out
            }
            View::Record(_, fs) => {
                let mut out = Vec::new();
                for (name, ft) in &fs {
                    let f = self.let_(&self.g.rty(ft)?, &format!("{v}.{}", snake(name)));
                    out.extend(self.lower(ft, &f)?);
                }
                out
            }
            View::FixedList(e, n) => {
                let mut out = Vec::new();
                for i in 0..n {
                    let x = self.let_(&self.g.rty(&e)?, &format!("{v}[{i}]"));
                    out.extend(self.lower(&e, &x)?);
                }
                out
            }
            View::Future | View::Stream => {
                let ops = self.g.ops_fn(t)?;
                let f = if matches!(t, Type::Future(_)) {
                    "future_out"
                } else {
                    "stream_out"
                };
                vec![self.let_("i32", &format!("{A}::{f}({ops}(), {v})"))]
            }
            View::ErrorContext => vec![self.let_("i32", &format!("{v}.raw()"))],
            View::Flags(_, names) => vec![self.flag_bits(&names, v)],
            View::Enum(..) => vec![self.let_("i32", &format!("{v} as i32"))],
            View::Option(_) | View::Result(..) | View::Variant(..) => self.lower_variant(t, v)?,
            View::Own(res) => {
                if self.g.ours(res) {
                    let (new, _, _, table) = self.g.resource_fns(res);
                    vec![self.let_("i32", &format!("{new}({table}.insert({v}))"))]
                } else {
                    vec![self.let_("i32", &format!("{v}.handle.take()"))]
                }
            }
            View::Borrow(res) => {
                // An import's types are imports: its `borrow<r>` is never
                // the component's own exported `r`.
                if self.g.ours(res) {
                    return Err(format!(
                        "a `borrow<{}>` of a resource this component exports cannot be passed to an import",
                        self.g.r.types[res].name
                    ));
                }
                vec![self.let_("i32", &format!("{v}.handle.get()"))]
            }
        })
    }

    /// The values of `f`'s parameters an export receives: core
    /// parameters `names`, or a block at `a0` (freed).
    pub(crate) fn lift_params(
        &mut self,
        f: &Function,
        pmem: bool,
        names: Vec<String>,
    ) -> Result<Vec<String>, String> {
        let r = self.g.r;
        let mut vals = Vec::new();
        if pmem {
            let types: Vec<Type> = f.params.iter().map(|(_, t)| t.clone()).collect();
            for (t, off) in types.iter().zip(abi::field_offsets(r, &types)) {
                vals.push(self.load(t, &format!("a0 + {off}i32"))?);
            }
            self.line(&format!("{W}::free(a0);"));
        } else {
            let mut it = names.into_iter();
            for (_, t) in &f.params {
                vals.push(self.lift(t, &mut it)?);
            }
        }
        Ok(vals)
    }

    pub(crate) fn flag_bits(&mut self, names: &[String], v: &str) -> String {
        let x = self.let_("i32", "0i32");
        for (i, n) in names.iter().enumerate() {
            self.line(&format!(
                "if {v}.{} {{ {x} = {x} | (1u32 << {i}u32) as i32; }}",
                snake(n)
            ));
        }
        x
    }

    /// The case patterns of a variant-like `t`: `(pattern, binding)`.
    pub(crate) fn patterns(&mut self, t: &Type) -> Result<Vec<(String, Option<String>)>, String> {
        Ok(match self.g.view(t) {
            View::Variant(id, cs) => {
                let path = self.g.type_path(id);
                cs.iter()
                    .map(|(c, ct)| match ct {
                        Some(_) => {
                            let b = self.tmp();
                            (format!("{path}::{}({b})", pascal(c)), Some(b))
                        }
                        None => (format!("{path}::{}", pascal(c)), None),
                    })
                    .collect()
            }
            View::Option(e) => {
                let b = self.tmp();
                vec![
                    ("none".into(), None),
                    (format!("{b}: {}", self.g.rty(&e)?), Some(b)),
                ]
            }
            View::Result(a, b) => {
                let mut out = Vec::new();
                for (ctor, t) in [("Ok", a), ("Err", b)] {
                    if t.is_some() {
                        let v = self.tmp();
                        out.push((format!("{RESULT}::{ctor}({v})"), Some(v)));
                    } else {
                        out.push((format!("{RESULT}::{ctor}(_)"), None));
                    }
                }
                out
            }
            _ => unreachable!("variant-like"),
        })
    }

    /// The value of case `i` of variant-like `t` holding `payload`.
    pub(crate) fn construct(&self, t: &Type, i: usize, payload: Option<&str>) -> String {
        match self.g.view(t) {
            View::Variant(id, cs) => {
                let c = format!("{}::{}", self.g.type_path(id), pascal(&cs[i].0));
                payload.map_or(c.clone(), |p| format!("{c}({p})"))
            }
            View::Option(_) => payload.map_or("none".into(), str::to_owned),
            View::Result(..) => format!(
                "{RESULT}::{}({})",
                if i == 0 { "Ok" } else { "Err" },
                payload.unwrap_or("()")
            ),
            _ => unreachable!("variant-like"),
        }
    }

    pub(crate) fn lower_variant(&mut self, t: &Type, v: &str) -> Result<Vec<String>, String> {
        let cs = abi::cases(self.g.r, t).unwrap_or_default();
        let flat = abi::flatten_cases(self.g.r, &cs);
        let d = self.let_("i32", "0i32");
        let slots: Vec<String> = flat[1..]
            .iter()
            .map(|f| self.let_(f.nabi(), zero(*f)))
            .collect();
        let pats = self.patterns(t)?;
        self.line(&format!("match {v} {{"));
        for (i, ((pat, bind), ct)) in pats.into_iter().zip(&cs).enumerate() {
            self.line(&format!("{pat} => {{"));
            self.line(&format!("{d} = {i}i32;"));
            if let (Some(b), Some(ct)) = (bind, ct) {
                let vals = self.lower(ct, &b)?;
                let pf = abi::flatten(self.g.r, ct);
                for (j, val) in vals.iter().enumerate() {
                    let c = coerce(pf[j], flat[j + 1], val);
                    self.line(&format!("{} = {c};", slots[j]));
                }
            }
            self.line("}");
        }
        self.line("}");
        let mut out = vec![d];
        out.extend(slots);
        Ok(out)
    }

    pub(crate) fn lift(
        &mut self,
        t: &Type,
        vals: &mut std::vec::IntoIter<String>,
    ) -> Result<String, String> {
        let mut next = || vals.next().unwrap_or_else(|| "0i32".into());
        Ok(match self.g.view(t) {
            View::Prim(p) => {
                let x = next();
                match p {
                    Prim::Bool => self.let_("bool", &format!("{x} != 0i32")),
                    Prim::U8 => self.let_("u8", &format!("{x} as u8")),
                    Prim::U16 => self.let_("u16", &format!("{x} as u16")),
                    Prim::U32 => self.let_("u32", &format!("{x} as u32")),
                    Prim::S8 => self.let_("i8", &format!("{x} as i8")),
                    Prim::S16 => self.let_("i16", &format!("{x} as i16")),
                    Prim::U64 => self.let_("u64", &format!("{x} as u64")),
                    Prim::S32 | Prim::S64 | Prim::F32 | Prim::F64 => x,
                    Prim::Char => self.let_("char", &format!("{W}::char_from({x})")),
                    Prim::String => {
                        let n = next();
                        self.let_("str", &format!("{W}::str_take({x}, {n})"))
                    }
                }
            }
            View::List(e) => {
                let (p, n) = (next(), next());
                self.lift_list(&e, &p, &n)?
            }
            View::Tuple(ts) => {
                let mut parts = Vec::new();
                for ft in &ts {
                    parts.push(self.lift(ft, vals)?);
                }
                self.let_(&self.g.rty(t)?, &format!("({},)", parts.join(", ")))
            }
            View::Record(id, fs) => {
                let mut parts = Vec::new();
                for (name, ft) in &fs {
                    let v = self.lift(ft, vals)?;
                    parts.push(format!("{}: {v}", snake(name)));
                }
                self.let_(
                    &self.g.rty(t)?,
                    &format!("{} {{ {} }}", self.g.type_path(id), parts.join(", ")),
                )
            }
            View::FixedList(e, n) => {
                let mut parts = Vec::new();
                for _ in 0..n {
                    parts.push(self.lift(&e, vals)?);
                }
                self.let_(&self.g.rty(t)?, &format!("[{}]", parts.join(", ")))
            }
            View::Future | View::Stream => {
                let h = next();
                let ops = self.g.ops_fn(t)?;
                let f = if matches!(t, Type::Future(_)) {
                    "future_in"
                } else {
                    "stream_in"
                };
                self.let_(&self.g.rty(t)?, &format!("{A}::{f}({ops}(), {h})"))
            }
            View::ErrorContext => {
                let h = next();
                self.let_(
                    &self.g.rty(t)?,
                    &format!("{W}::ErrorContext::from_raw({h})"),
                )
            }
            View::Flags(id, names) => {
                let x = next();
                self.flags_value(id, &names, &x)
            }
            View::Enum(id, n) => {
                let x = next();
                self.enum_value(id, n, &x)
            }
            View::Option(_) | View::Result(..) | View::Variant(..) => {
                let cs = abi::cases(self.g.r, t).unwrap_or_default();
                let flat = abi::flatten_cases(self.g.r, &cs);
                let d = next();
                let slots: Vec<String> = flat[1..]
                    .iter()
                    .map(|_| vals.next().unwrap_or_default())
                    .collect();
                let res = self.tmp();
                self.line(&format!("let {res}: {} = match {d} {{", self.g.rty(t)?));
                for (i, ct) in cs.iter().enumerate() {
                    self.line(&format!("{i}i32 => {{"));
                    let payload = match ct {
                        Some(ct) => {
                            let pf = abi::flatten(self.g.r, ct);
                            let mut cvals = Vec::new();
                            for (j, f) in pf.iter().enumerate() {
                                cvals.push(
                                    self.let_(f.nabi(), &uncoerce(flat[j + 1], *f, &slots[j])),
                                );
                            }
                            Some(self.lift(ct, &mut cvals.into_iter())?)
                        }
                        None => None,
                    };
                    let c = self.construct(t, i, payload.as_deref());
                    self.line(&c);
                    self.line("}");
                }
                self.line(&format!("_ => {W}::bad_case(),"));
                self.line("};");
                res
            }
            View::Own(res) => {
                let h = next();
                self.lift_handle(res, &h, true)
            }
            View::Borrow(res) => {
                let h = next();
                self.lift_handle(res, &h, false)
            }
        })
    }

    pub(crate) fn lift_handle(&mut self, res: TypeId, h: &str, own: bool) -> String {
        let ty = self.g.type_path(res);
        if self.g.ours(res) {
            let (_, rep, drop, table) = self.g.resource_fns(res);
            if own {
                let r = self.let_("i32", &format!("{rep}({h})"));
                // Out of the table first: the handle's drop runs the
                // `[dtor]`, which then finds no value to drop.
                let v = self.let_(&ty, &format!("{table}.get({r})"));
                self.line(&format!("{table}.take({r});"));
                self.line(&format!("{drop}({h});"));
                v
            } else {
                // A borrow of one's own resource arrives as its rep.
                self.let_(&ty, &format!("{table}.get({h})"))
            }
        } else {
            self.g.resource_fns(res);
            let how = if own { "own" } else { "borrowed" };
            self.let_(&ty, &format!("{ty} {{ handle: {W}::Handle::{how}({h}) }}"))
        }
    }

    pub(crate) fn flags_value(&mut self, id: TypeId, names: &[String], x: &str) -> String {
        let fields: Vec<String> = names
            .iter()
            .enumerate()
            .map(|(i, n)| format!("{}: ({x} & (1u32 << {i}u32) as i32) != 0i32", snake(n)))
            .collect();
        let path = self.g.type_path(id);
        self.let_(&path, &format!("{path} {{ {} }}", fields.join(", ")))
    }

    pub(crate) fn enum_value(&mut self, id: TypeId, n: usize, x: &str) -> String {
        let path = self.g.type_path(id);
        let TypeKind::Enum(cs) = &self.g.r.types[id].kind else {
            unreachable!("enum")
        };
        let mut arms: Vec<String> = cs
            .iter()
            .take(n)
            .enumerate()
            .map(|(i, c)| format!("{i}i32 => {path}::{}", pascal(c)))
            .collect();
        arms.push(format!("_ => {W}::bad_case()"));
        self.let_(&path, &format!("match {x} {{ {} }}", arms.join(", ")))
    }

    pub(crate) fn lower_list(&mut self, e: &Type, v: &str) -> Result<(String, String), String> {
        if matches!(self.g.view(e), View::Prim(Prim::U8)) {
            let (p, n) = (self.tmp(), self.tmp());
            self.line(&format!("let ({p}, {n}) = w_b.bytes({v});"));
            return Ok((p, n));
        }
        let size = abi::size(self.g.r, e);
        let align = abi::align(self.g.r, e);
        let n = self.let_("i32", &format!("{v}.len() as i32"));
        let p = self.let_("i32", &format!("w_b.alloc({n} * {size}i32, {align}i32)"));
        let i = self.tmp();
        self.line(&format!("for {i} in 0..{v}.len() {{"));
        let x = self.let_(&self.g.rty(e)?, &format!("{v}[{i}]"));
        self.store(e, &x, &format!("{p} + ({i} as i32) * {size}i32"))?;
        self.line("}");
        Ok((p, n))
    }

    pub(crate) fn lift_list(&mut self, e: &Type, p: &str, n: &str) -> Result<String, String> {
        let ty = format!("{LIST}<{}>", self.g.rty(e)?);
        if matches!(self.g.view(e), View::Prim(Prim::U8)) {
            return Ok(self.let_(&ty, &format!("{W}::bytes_take({p}, {n})")));
        }
        let size = abi::size(self.g.r, e);
        let out = self.let_(&ty, &format!("{LIST}::new()"));
        let i = self.tmp();
        self.line(&format!("for {i} in 0..({n} as u32 as usize) {{"));
        let x = self.load(e, &format!("{p} + ({i} as i32) * {size}i32"))?;
        self.line(&format!("{out}.push({x});"));
        self.line("}");
        self.line(&format!("if {n} != 0i32 {{ {W}::free({p}); }}"));
        Ok(out)
    }

    pub(crate) fn store_disc(&mut self, a: &str, d: &str, cases: usize) {
        match abi::disc_size(cases) {
            1 => self.line(&format!("{W}::store_u8({a}, ({d}) as u8);")),
            2 => self.line(&format!("{W}::store_u16({a}, ({d}) as u16);")),
            _ => self.line(&format!("{W}::store_i32({a}, {d});")),
        }
    }

    pub(crate) fn load_disc(&mut self, a: &str, cases: usize) -> String {
        match abi::disc_size(cases) {
            1 => self.let_("i32", &format!("{W}::load_u8({a}) as i32")),
            2 => self.let_("i32", &format!("{W}::load_u16({a}) as i32")),
            _ => self.let_("i32", &format!("{W}::load_i32({a})")),
        }
    }

    pub(crate) fn store(&mut self, t: &Type, v: &str, a: &str) -> Result<(), String> {
        match self.g.view(t) {
            View::Prim(p) => {
                let s = match p {
                    Prim::Bool => format!("store_u8({a}, if {v} {{ 1u8 }} else {{ 0u8 }})"),
                    Prim::U8 => format!("store_u8({a}, {v})"),
                    Prim::S8 => format!("store_u8({a}, {v} as u8)"),
                    Prim::U16 => format!("store_u16({a}, {v})"),
                    Prim::S16 => format!("store_u16({a}, {v} as u16)"),
                    Prim::U32 => format!("store_i32({a}, {v} as i32)"),
                    Prim::S32 => format!("store_i32({a}, {v})"),
                    Prim::U64 => format!("store_i64({a}, {v} as i64)"),
                    Prim::S64 => format!("store_i64({a}, {v})"),
                    Prim::F32 => format!("store_f32({a}, {v})"),
                    Prim::F64 => format!("store_f64({a}, {v})"),
                    Prim::Char => format!("store_i32({a}, ({v} as u32) as i32)"),
                    Prim::String => {
                        let (p, n) = (self.tmp(), self.tmp());
                        self.line(&format!("let ({p}, {n}) = w_b.str({v});"));
                        format!("store_i32({a}, {p}); {W}::store_i32({a} + 4i32, {n})")
                    }
                };
                self.line(&format!("{W}::{s};"));
            }
            View::List(e) => {
                let (p, n) = self.lower_list(&e, v)?;
                self.line(&format!(
                    "{W}::store_i32({a}, {p}); {W}::store_i32({a} + 4i32, {n});"
                ));
            }
            View::Tuple(ts) => {
                let offs = abi::field_offsets(self.g.r, &ts);
                for (i, (ft, off)) in ts.iter().zip(offs).enumerate() {
                    let f = self.let_(&self.g.rty(ft)?, &format!("{v}.{i}"));
                    self.store(ft, &f, &format!("{a} + {off}i32"))?;
                }
            }
            View::Record(_, fs) => {
                let types: Vec<Type> = fs.iter().map(|(_, t)| t.clone()).collect();
                let offs = abi::field_offsets(self.g.r, &types);
                for ((name, ft), off) in fs.iter().zip(offs) {
                    let f = self.let_(&self.g.rty(ft)?, &format!("{v}.{}", snake(name)));
                    self.store(ft, &f, &format!("{a} + {off}i32"))?;
                }
            }
            View::FixedList(e, n) => {
                let size = abi::size(self.g.r, &e);
                let i = self.tmp();
                self.line(&format!("for {i} in 0..{n}usize {{"));
                let x = self.let_(&self.g.rty(&e)?, &format!("{v}[{i}]"));
                self.store(&e, &x, &format!("{a} + ({i} as i32) * {size}i32"))?;
                self.line("}");
            }
            View::Future | View::Stream | View::ErrorContext => {
                let h = self.lower(t, v)?.remove(0);
                self.line(&format!("{W}::store_i32({a}, {h});"));
            }
            View::Flags(_, names) => {
                let x = self.flag_bits(&names, v);
                match abi::flags_size(names.len()) {
                    0 => {}
                    1 => self.line(&format!("{W}::store_u8({a}, {x} as u8);")),
                    2 => self.line(&format!("{W}::store_u16({a}, {x} as u16);")),
                    _ => self.line(&format!("{W}::store_i32({a}, {x});")),
                }
            }
            View::Enum(_, n) => self.store_disc(a, &format!("{v} as i32"), n),
            View::Option(_) | View::Result(..) | View::Variant(..) => {
                let cs = abi::cases(self.g.r, t).unwrap_or_default();
                let off = abi::payload_offset(self.g.r, &cs);
                let pats = self.patterns(t)?;
                self.line(&format!("match {v} {{"));
                for (i, ((pat, bind), ct)) in pats.into_iter().zip(&cs).enumerate() {
                    self.line(&format!("{pat} => {{"));
                    self.store_disc(a, &format!("{i}i32"), cs.len());
                    if let (Some(b), Some(ct)) = (bind, ct) {
                        self.store(ct, &b, &format!("{a} + {off}i32"))?;
                    }
                    self.line("}");
                }
                self.line("}");
            }
            View::Own(_) | View::Borrow(_) => {
                let h = self.lower(t, v)?.remove(0);
                self.line(&format!("{W}::store_i32({a}, {h});"));
            }
        }
        Ok(())
    }

    pub(crate) fn load(&mut self, t: &Type, a: &str) -> Result<String, String> {
        Ok(match self.g.view(t) {
            View::Prim(p) => {
                let (ty, e) = match p {
                    Prim::Bool => ("bool", format!("{W}::load_u8({a}) != 0u8")),
                    Prim::U8 => ("u8", format!("{W}::load_u8({a})")),
                    Prim::S8 => ("i8", format!("{W}::load_u8({a}) as i8")),
                    Prim::U16 => ("u16", format!("{W}::load_u16({a})")),
                    Prim::S16 => ("i16", format!("{W}::load_u16({a}) as i16")),
                    Prim::U32 => ("u32", format!("{W}::load_i32({a}) as u32")),
                    Prim::S32 => ("i32", format!("{W}::load_i32({a})")),
                    Prim::U64 => ("u64", format!("{W}::load_i64({a}) as u64")),
                    Prim::S64 => ("i64", format!("{W}::load_i64({a})")),
                    Prim::F32 => ("f32", format!("{W}::load_f32({a})")),
                    Prim::F64 => ("f64", format!("{W}::load_f64({a})")),
                    Prim::Char => ("char", format!("{W}::char_from({W}::load_i32({a}))")),
                    Prim::String => {
                        let p = self.let_("i32", &format!("{W}::load_i32({a})"));
                        let n = self.let_("i32", &format!("{W}::load_i32({a} + 4i32)"));
                        ("str", format!("{W}::str_take({p}, {n})"))
                    }
                };
                self.let_(ty, &e)
            }
            View::List(e) => {
                let p = self.let_("i32", &format!("{W}::load_i32({a})"));
                let n = self.let_("i32", &format!("{W}::load_i32({a} + 4i32)"));
                self.lift_list(&e, &p, &n)?
            }
            View::Tuple(ts) => {
                let offs = abi::field_offsets(self.g.r, &ts);
                let mut parts = Vec::new();
                for (ft, off) in ts.iter().zip(offs) {
                    parts.push(self.load(ft, &format!("{a} + {off}i32"))?);
                }
                self.let_(&self.g.rty(t)?, &format!("({},)", parts.join(", ")))
            }
            View::Record(id, fs) => {
                let types: Vec<Type> = fs.iter().map(|(_, t)| t.clone()).collect();
                let offs = abi::field_offsets(self.g.r, &types);
                let mut parts = Vec::new();
                for ((name, ft), off) in fs.iter().zip(offs) {
                    let v = self.load(ft, &format!("{a} + {off}i32"))?;
                    parts.push(format!("{}: {v}", snake(name)));
                }
                self.let_(
                    &self.g.rty(t)?,
                    &format!("{} {{ {} }}", self.g.type_path(id), parts.join(", ")),
                )
            }
            View::FixedList(e, n) => {
                let size = abi::size(self.g.r, &e);
                let first = self.load(&e, a)?;
                let arr = self.let_(&self.g.rty(t)?, &format!("[{first}; {n}]"));
                let i = self.tmp();
                self.line(&format!("for {i} in 1..{n}usize {{"));
                let x = self.load(&e, &format!("{a} + ({i} as i32) * {size}i32"))?;
                self.line(&format!("{arr}[{i}] = {x};"));
                self.line("}");
                arr
            }
            View::Future | View::Stream | View::ErrorContext => {
                let h = self.let_("i32", &format!("{W}::load_i32({a})"));
                self.lift(t, &mut vec![h].into_iter())?
            }
            View::Flags(id, names) => {
                let x = match abi::flags_size(names.len()) {
                    0 => "0i32".to_owned(),
                    1 => format!("{W}::load_u8({a}) as i32"),
                    2 => format!("{W}::load_u16({a}) as i32"),
                    _ => format!("{W}::load_i32({a})"),
                };
                let x = self.let_("i32", &x);
                self.flags_value(id, &names, &x)
            }
            View::Enum(id, n) => {
                let d = self.load_disc(a, n);
                self.enum_value(id, n, &d)
            }
            View::Option(_) | View::Result(..) | View::Variant(..) => {
                let cs = abi::cases(self.g.r, t).unwrap_or_default();
                let off = abi::payload_offset(self.g.r, &cs);
                let d = self.load_disc(a, cs.len());
                let res = self.tmp();
                self.line(&format!("let {res}: {} = match {d} {{", self.g.rty(t)?));
                for (i, ct) in cs.iter().enumerate() {
                    self.line(&format!("{i}i32 => {{"));
                    let payload = match ct {
                        Some(ct) => Some(self.load(ct, &format!("{a} + {off}i32"))?),
                        None => None,
                    };
                    let c = self.construct(t, i, payload.as_deref());
                    self.line(&c);
                    self.line("}");
                }
                self.line(&format!("_ => {W}::bad_case(),"));
                self.line("};");
                res
            }
            View::Own(_) | View::Borrow(_) => {
                let h = self.let_("i32", &format!("{W}::load_i32({a})"));
                self.lift(t, &mut vec![h].into_iter())?
            }
        })
    }

    /// Free what lowering `t` at `a` allocated (a post-return function).
    pub(crate) fn free_mem(&mut self, t: &Type, a: &str) {
        if !self.g.has_heap(t) {
            return;
        }
        match self.g.view(t) {
            View::Prim(_) => {
                self.line(&format!(
                    "if {W}::load_i32({a} + 4i32) != 0i32 {{ {W}::free({W}::load_i32({a})); }}"
                ));
            }
            View::List(e) => {
                let p = self.let_("i32", &format!("{W}::load_i32({a})"));
                let n = self.let_("i32", &format!("{W}::load_i32({a} + 4i32)"));
                if self.g.has_heap(&e) {
                    let size = abi::size(self.g.r, &e);
                    let i = self.tmp();
                    self.line(&format!("for {i} in 0..{n} {{"));
                    self.free_mem(&e, &format!("{p} + {i} * {size}i32"));
                    self.line("}");
                }
                self.line(&format!("if {n} != 0i32 {{ {W}::free({p}); }}"));
            }
            View::Tuple(ts) => {
                for (ft, off) in ts.iter().zip(abi::field_offsets(self.g.r, &ts)) {
                    self.free_mem(ft, &format!("{a} + {off}i32"));
                }
            }
            View::Record(_, fs) => {
                let types: Vec<Type> = fs.into_iter().map(|(_, t)| t).collect();
                for (ft, off) in types.iter().zip(abi::field_offsets(self.g.r, &types)) {
                    self.free_mem(ft, &format!("{a} + {off}i32"));
                }
            }
            View::Option(_) | View::Result(..) | View::Variant(..) => {
                let cs = abi::cases(self.g.r, t).unwrap_or_default();
                let off = abi::payload_offset(self.g.r, &cs);
                let d = self.load_disc(a, cs.len());
                self.line(&format!("match {d} {{"));
                for (i, ct) in cs.iter().enumerate() {
                    if let Some(ct) = ct.as_ref().filter(|c| self.g.has_heap(c)) {
                        self.line(&format!("{i}i32 => {{"));
                        self.free_mem(ct, &format!("{a} + {off}i32"));
                        self.line("}");
                    }
                }
                self.line("_ => {}");
                self.line("}");
            }
            View::FixedList(e, n) => {
                let size = abi::size(self.g.r, &e);
                let i = self.tmp();
                self.line(&format!("for {i} in 0..{n}i32 {{"));
                self.free_mem(&e, &format!("{a} + {i} * {size}i32"));
                self.line("}");
            }
            View::Enum(..)
            | View::Flags(..)
            | View::Own(_)
            | View::Borrow(_)
            | View::Future
            | View::Stream
            | View::ErrorContext => {}
        }
    }
}

/// The call of the user's function `call` with `vals` (a method's
/// first value its receiver).
pub(crate) fn user_call(f: &Function, call: &str, vals: &[String]) -> String {
    match f.kind {
        FnKind::Method(_) => format!("{}.{call}({})", vals[0], vals[1..].join(", ")),
        _ => format!("{call}({})", vals.join(", ")),
    }
}

/// The core signature of `f`: flat params (one pointer when there are
/// too many) and whether results go through memory.
pub(crate) fn core_sig(r: &Resolve, f: &Function) -> (Vec<Flat>, Vec<Flat>, bool, bool) {
    let pflat: Vec<Flat> = f
        .params
        .iter()
        .flat_map(|(_, t)| abi::flatten(r, t))
        .collect();
    let rflat = f
        .result
        .as_ref()
        .map(|t| abi::flatten(r, t))
        .unwrap_or_default();
    let pmem = pflat.len() > MAX_FLAT_PARAMS;
    let rmem = rflat.len() > MAX_FLAT_RESULTS;
    (pflat, rflat, pmem, rmem)
}

/// `result<T, E>` with an error type: `(T, E)`.
pub(crate) fn throwing(t: Option<&Type>) -> Option<(Option<Type>, Type)> {
    match t {
        Some(Type::Result(ok, Some(err))) => Some((ok.as_deref().cloned(), (**err).clone())),
        _ => None,
    }
}
