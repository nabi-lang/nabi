//! Imported functions and interfaces.

#[allow(clippy::wildcard_imports)]
use crate::bind::*;

impl Gen<'_> {
    /// The futures and streams in `t`, in `wit-parser`'s order (each one
    /// after those of its value type), through aliases.
    pub(crate) fn payloads(&self, t: &Type, out: &mut Vec<Type>, depth: usize) {
        if depth > 64 {
            return;
        }
        match t {
            Type::Prim(_) | Type::Own(_) | Type::Borrow(_) | Type::ErrorContext => {}
            Type::List(e) | Type::Option(e) | Type::FixedList(e, _) => {
                self.payloads(e, out, depth + 1);
            }
            Type::Result(a, b) => {
                for x in [a, b].into_iter().flatten() {
                    self.payloads(x, out, depth + 1);
                }
            }
            Type::Tuple(ts) => {
                for x in ts {
                    self.payloads(x, out, depth + 1);
                }
            }
            Type::Future(e) | Type::Stream(e) => {
                if let Some(e) = e {
                    self.payloads(e, out, depth + 1);
                }
                out.push(t.clone());
            }
            Type::Id(id) => match &self.r.types[*id].kind {
                TypeKind::Alias(a) => self.payloads(a, out, depth + 1),
                TypeKind::Record(fs) => {
                    for (_, x) in fs {
                        self.payloads(x, out, depth + 1);
                    }
                }
                TypeKind::Variant(cs) => {
                    for x in cs.iter().filter_map(|(_, t)| t.as_ref()) {
                        self.payloads(x, out, depth + 1);
                    }
                }
                _ => {}
            },
        }
    }

    /// `t` through its aliases.
    pub(crate) fn unalias(&self, t: &Type) -> Type {
        let mut t = t.clone();
        loop {
            let Type::Id(id) = &t else { return t };
            match &self.r.types[*id].kind {
                TypeKind::Alias(a) => t = a.clone(),
                _ => return t,
            }
        }
    }

    /// Bind `f`, whose built-ins live in core module `module`, next.
    pub(crate) fn set_ctx(&mut self, module: &str, f: &Function) {
        let mut payloads = Vec::new();
        for (_, t) in &f.params {
            self.payloads(t, &mut payloads, 0);
        }
        if let Some(t) = &f.result {
            self.payloads(t, &mut payloads, 0);
        }
        let payloads = payloads.iter().map(|t| self.unalias(t)).collect();
        self.cur = Some(FnCtx {
            module: module.to_owned(),
            name: f.wit_name(self.r),
            payloads,
        });
    }

    /// Path of the function giving the ops (`FutureOps` / `StreamOps`) of
    /// future or stream type `t`, named after the bound function and `t`'s
    /// place in it.
    pub(crate) fn ops_fn(&mut self, t: &Type) -> Result<String, String> {
        let t = self.unalias(t);
        let ctx = self
            .cur
            .clone()
            .ok_or("a future or stream outside a function")?;
        let idx = ctx
            .payloads
            .iter()
            .position(|p| *p == t)
            .ok_or("a future or stream not in its function's type")?;
        let (kind, elem) = match &t {
            Type::Future(e) => ("future", e.as_deref().cloned()),
            Type::Stream(e) => ("stream", e.as_deref().cloned()),
            _ => unreachable!("future or stream"),
        };
        let fname = &ctx.name;
        let id = ident("wit_ops", &ctx.module, &format!("{kind}{idx}{fname}"));
        let path = format!("{}::{id}", self.base);
        if !self.ops.insert(id.clone()) {
            return Ok(path);
        }
        let module = &ctx.module;
        let new = self.extern_fn(
            &[],
            module,
            &format!("[{kind}-new-{idx}]{fname}"),
            &[],
            Some(Flat::I64),
        );
        let future = kind == "future";
        let rw: &[Flat] = if future {
            &[Flat::I32; 2]
        } else {
            &[Flat::I32; 3]
        };
        let [read, write] = ["read", "write"].map(|op| {
            self.extern_fn(
                &[],
                module,
                &format!("[async-lower][{kind}-{op}-{idx}]{fname}"),
                rw,
                Some(Flat::I32),
            )
        });
        let [dr, dw] = ["readable", "writable"].map(|end| {
            self.extern_fn(
                &[],
                module,
                &format!("[{kind}-drop-{end}-{idx}]{fname}"),
                &[Flat::I32],
                None,
            )
        });
        let (ty, size, align, lift, lower) = match &elem {
            None => (
                "()".to_owned(),
                0,
                1,
                "        ()\n".to_owned(),
                String::new(),
            ),
            Some(e) => {
                let ty = self.rty(e)?;
                let mut fx = Fx {
                    g: self,
                    out: String::new(),
                    n: 0,
                };
                let v = fx.load(e, "a")?;
                let lift = format!("{}        {v}\n", fx.out);
                let mut fx = Fx {
                    g: self,
                    out: String::new(),
                    n: 0,
                };
                fx.store(e, "v", "a")?;
                let lower = fx.out;
                (ty, abi::size(self.r, e), abi::align(self.r, e), lift, lower)
            }
        };
        let ops = if future { "FutureOps" } else { "StreamOps" };
        let (rp, ra) = if future {
            ("h: i32, a: i32", "h, a")
        } else {
            ("h: i32, a: i32, n: i32", "h, a, n")
        };
        let text = format!(
            "pub(pkg) fn {id}() -> {A}::{ops}<{ty}> {{
    {A}::{ops} {{
        new: || -> i64 {{ unsafe {{ {new}() }} }},
        read: |{rp}| -> i32 {{ unsafe {{ {read}({ra}) }} }},
        write: |{rp}| -> i32 {{ unsafe {{ {write}({ra}) }} }},
        drop_readable: |h: i32| {{ unsafe {{ {dr}(h); }} }},
        drop_writable: |h: i32| {{ unsafe {{ {dw}(h); }} }},
        size: {size}i32,
        align: {align}i32,
        lift: |a: i32| -> {ty} {{
    unsafe {{
{lift}    }}
        }},
        lower: |v: {ty}, a: i32, w_b: {W}::Blocks| {{
    unsafe {{
{lower}    }}
        }},
    }}
}}
"
        );
        self.push(&[], &text);
        Ok(path)
    }

    /// The nabi function calling import `f`. An `async func` starts a subtask
    /// its future awaits; arguments and result stay in memory until it
    /// returns.
    pub(crate) fn import_fn(
        &mut self,
        m: &[String],
        module: &str,
        f: &Function,
    ) -> Result<String, String> {
        let r = self.r;
        self.set_ctx(module, f);
        let (pflat, rflat, ..) = core_sig(r, f);
        let max = if f.is_async {
            MAX_FLAT_ASYNC_PARAMS
        } else {
            MAX_FLAT_PARAMS
        };
        let pmem = pflat.len() > max;
        let rmem = (f.is_async && f.result.is_some()) || rflat.len() > MAX_FLAT_RESULTS;
        let mut cparams = if pmem { vec![Flat::I32] } else { pflat };
        if rmem {
            cparams.push(Flat::I32);
        }
        let cret = if f.is_async {
            Some(Flat::I32)
        } else if rmem {
            None
        } else {
            rflat.first().copied()
        };
        let core = if f.is_async {
            format!("[async-lower]{}", f.wit_name(r))
        } else {
            f.wit_name(r)
        };
        let ext = self.extern_fn(m, module, &core, &cparams, cret);
        let throws = throwing(f.result.as_ref());
        let ret = match (&throws, &f.result) {
            (Some((ok, err)), _) => {
                let ok = ok.as_ref().map(|t| self.rty(t)).transpose()?;
                format!(
                    " throws ({}){}",
                    self.rty(err)?,
                    ok.map(|t| format!(" -> {t}")).unwrap_or_default()
                )
            }
            (None, Some(t)) => format!(" -> {}", self.rty(t)?),
            (None, None) => String::new(),
        };
        let mut params = Vec::new();
        let mut vars = Vec::new();
        for (i, (n, t)) in f.params.iter().enumerate() {
            if i == 0 && matches!(f.kind, FnKind::Method(_)) {
                params.push("self".to_owned());
                vars.push("self".to_owned());
            } else {
                params.push(format!("{}: {}", snake(n), self.rty(t)?));
                vars.push(snake(n));
            }
        }
        let name = match f.kind {
            FnKind::Constructor(_) => "new".to_owned(),
            _ => snake(&f.name),
        };
        let rt = f.result.clone().unwrap_or(Type::Tuple(Vec::new()));
        let mut pre = format!("        let w_b = {W}::Blocks::new();\n");
        if rmem {
            let _ = writeln!(
                pre,
                "        let w_ra: i32 = w_b.alloc({}i32, {}i32);",
                abi::size(r, &rt),
                abi::align(r, &rt)
            );
        }
        let mut fx = Fx {
            g: self,
            out: String::new(),
            n: 0,
        };
        let mut args = Vec::new();
        if pmem {
            let types: Vec<Type> = f.params.iter().map(|(_, t)| t.clone()).collect();
            let rec = Type::Tuple(types.clone());
            let area = fx.let_(
                "i32",
                &format!(
                    "w_b.alloc({}i32, {}i32)",
                    abi::size(r, &rec),
                    abi::align(r, &rec)
                ),
            );
            for ((t, v), off) in types.iter().zip(&vars).zip(abi::field_offsets(r, &types)) {
                fx.store(t, v, &format!("{area} + {off}i32"))?;
            }
            args.push(area);
        } else {
            for ((_, t), v) in f.params.iter().zip(&vars) {
                args.extend(fx.lower(t, v)?);
            }
        }
        if rmem {
            args.push("w_ra".to_owned());
        }
        let call = format!("{ext}({})", args.join(", "));
        // The call's lines; then the result's.
        let mut value = None;
        let lowering = if f.is_async {
            fx.line(&call);
            std::mem::take(&mut fx.out)
        } else {
            if rmem {
                fx.line(&format!("{call};"));
            } else if let (Some(rt), Some(cf)) = (&f.result, cret) {
                let x = fx.let_(cf.nabi(), &call);
                value = Some(fx.lift(rt, &mut vec![x].into_iter())?);
            } else {
                fx.line(&format!("{call};"));
            }
            String::new()
        };
        if rmem && f.result.is_some() {
            value = Some(fx.load(&rt, "w_ra")?);
        }
        fx.line("w_b.free();");
        match (&throws, value) {
            (Some((ok, _)), Some(v)) => {
                let okv = if ok.is_some() { "w_ok" } else { "_" };
                fx.line(&format!(
                    "match {v} {{ {RESULT}::Ok({okv}) => {}, {RESULT}::Err(w_err) => throw w_err, }}",
                    if ok.is_some() { "w_ok" } else { "()" }
                ));
            }
            (_, Some(v)) => fx.line(&v),
            _ => {}
        }
        let lifting = fx.out;
        let ps = params.join(", ");
        Ok(if f.is_async {
            format!(
                "    pub async fn {name}({ps}){ret} {{\n{pre}        let w_st: i32 = unsafe {{\n{lowering}        }};\n        await {A}::subtask(w_st);\n        unsafe {{\n{lifting}        }}\n    }}\n"
            )
        } else {
            format!(
                "    pub fn {name}({ps}){ret} {{\n    unsafe {{\n{pre}{lifting}    }}\n    }}\n"
            )
        })
    }
}

impl Gen<'_> {
    pub(crate) fn import_iface(&mut self, i: IfaceId) -> Result<(), String> {
        let m = self.owner_mod(Owner::Iface(i));
        let key = self.r.iface_key(i);
        let mut free = String::new();
        let mut methods: BTreeMap<TypeId, String> = BTreeMap::new();
        for f in self.r.interfaces[i].funcs.clone() {
            let text = self.import_fn(&m, &key, &f)?;
            match f.kind {
                FnKind::Free => free.push_str(&text),
                FnKind::Method(t) | FnKind::Static(t) | FnKind::Constructor(t) => {
                    methods.entry(t).or_default().push_str(&text);
                }
            }
        }
        let mut text = free;
        for (t, body) in methods {
            let _ = write!(
                text,
                "impl {} {{\n{body}}}\n",
                pascal(&self.r.types[t].name)
            );
        }
        self.push(&m, &text);
        Ok(())
    }
}
