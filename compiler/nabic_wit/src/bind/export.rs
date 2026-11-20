//! Exported functions and interfaces.

#[allow(clippy::wildcard_imports)]
use crate::bind::*;

impl Gen<'_> {
    /// Core export `export` running `f` via `call` (a method: its name);
    /// built-ins in core module `module`.
    pub(crate) fn export_fn(
        &mut self,
        export: &str,
        module: &str,
        f: &Function,
        call: &str,
    ) -> Result<(), String> {
        self.set_ctx(module, f);
        if f.is_async {
            return self.export_async(export, module, f, call);
        }
        let r = self.r;
        let (pflat, rflat, pmem, rmem) = core_sig(r, f);
        let cparams = if pmem { vec![Flat::I32] } else { pflat };
        let cret = if rmem {
            Some(Flat::I32)
        } else {
            rflat.first().copied()
        };
        let id = ident("export", export, "");
        let mut fx = Fx {
            g: self,
            out: String::new(),
            n: 0,
        };
        fx.line(&format!("let w_b = {W}::Blocks::new();"));
        let names: Vec<String> = (0..cparams.len()).map(|i| format!("a{i}")).collect();
        let vals = fx.lift_params(f, pmem, names.clone())?;
        let call = user_call(f, call, &vals);
        let result = f.result.clone();
        let v = match (&result, throwing(result.as_ref())) {
            (Some(rt), Some(_)) => Some(fx.let_(
                &fx.g.rty(rt)?,
                &format!("{RESULT}::catching(|| try {call})"),
            )),
            (Some(rt), None) => Some(fx.let_(&fx.g.rty(rt)?, &call)),
            (None, _) => {
                fx.line(&format!("{call};"));
                None
            }
        };
        let tail = match (&result, v) {
            (Some(rt), Some(v)) if rmem => {
                let ra = fx.let_(
                    "i32",
                    &format!(
                        "w_b.alloc({}i32, {}i32)",
                        abi::size(r, rt),
                        abi::align(r, rt)
                    ),
                );
                fx.store(rt, &v, &ra)?;
                ra
            }
            (Some(rt), Some(v)) => fx.lower(rt, &v)?.remove(0),
            _ => String::new(),
        };
        let body = fx.out;
        let ps: Vec<String> = names
            .iter()
            .zip(&cparams)
            .map(|(n, f)| format!("{n}: {}", f.nabi()))
            .collect();
        let ret = cret
            .map(|f| format!(" -> {}", f.nabi()))
            .unwrap_or_default();
        let text = format!(
            "#[export_name(\"{export}\")]\nextern \"C\" fn {id}({}){ret} {{\n    unsafe {{\n{body}        {tail}\n    }}\n}}\n",
            ps.join(", ")
        );
        self.push(&[], &text);
        self.exports.push(export.to_owned());
        if rmem {
            let rt = result.unwrap_or(Type::Tuple(Vec::new()));
            let mut fx = Fx {
                g: self,
                out: String::new(),
                n: 0,
            };
            fx.free_mem(&rt, "a0");
            fx.line(&format!("{W}::free(a0);"));
            let body = fx.out;
            let post = format!("cabi_post_{export}");
            let pid = ident("post", export, "");
            self.push(
                &[],
                &format!("#[export_name(\"{post}\")]\nextern \"C\" fn {pid}(a0: i32) {{\n    unsafe {{\n{body}    }}\n}}\n"),
            );
            self.exports.push(post);
        }
        Ok(())
    }

    /// An `async func` export lifted with a callback: starts the user's
    /// future on the host's event loop, result through `task.return`.
    pub(crate) fn export_async(
        &mut self,
        export: &str,
        module: &str,
        f: &Function,
        call: &str,
    ) -> Result<(), String> {
        let r = self.r;
        let (pflat, rflat, pmem, _) = core_sig(r, f);
        let cparams = if pmem { vec![Flat::I32] } else { pflat };
        let rmem = rflat.len() > MAX_FLAT_PARAMS;
        let task_return = self.extern_fn(
            &[],
            module,
            &format!("[task-return]{}", f.wit_name(r)),
            &if rmem { vec![Flat::I32] } else { rflat },
            None,
        );
        let id = ident("export", export, "");
        let mut fx = Fx {
            g: self,
            out: String::new(),
            n: 0,
        };
        fx.line(&format!("{A}::async_enter();"));
        let names: Vec<String> = (0..cparams.len()).map(|i| format!("a{i}")).collect();
        let vals = fx.lift_params(f, pmem, names.clone())?;
        let mut fut = user_call(f, call, &vals);
        if throwing(f.result.as_ref()).is_some() {
            fut = format!("{A}::catching_async({fut})");
        }
        let body = fx.out;
        let mut gx = Fx {
            g: self,
            out: String::new(),
            n: 0,
        };
        gx.line(&format!("let w_b = {W}::Blocks::new();"));
        let mut args = Vec::new();
        let rty = match &f.result {
            Some(rt) if rmem => {
                let ra = gx.let_(
                    "i32",
                    &format!(
                        "w_b.alloc({}i32, {}i32)",
                        abi::size(r, rt),
                        abi::align(r, rt)
                    ),
                );
                gx.store(rt, "w_r", &ra)?;
                args.push(ra);
                gx.g.rty(rt)?
            }
            Some(rt) => {
                args = gx.lower(rt, "w_r")?;
                gx.g.rty(rt)?
            }
            None => "()".to_owned(),
        };
        gx.line(&format!("{task_return}({});", args.join(", ")));
        gx.line("w_b.free();");
        let ret_body = gx.out;
        let ps: Vec<String> = names
            .iter()
            .zip(&cparams)
            .map(|(n, f)| format!("{n}: {}", f.nabi()))
            .collect();
        let lift = format!("[async-lift]{export}");
        let callback = format!("[callback][async-lift]{export}");
        let cid = ident("callback", export, "");
        let text = format!(
            "#[export_name(\"{lift}\")]
extern \"C\" fn {id}({}) -> i32 {{
    unsafe {{
{body}        {A}::async_start({fut}, |w_r: {rty}| {{
    unsafe {{
{ret_body}    }}
        }})
    }}
}}
#[export_name(\"{callback}\")]
extern \"C\" fn {cid}(a0: i32, a1: i32, a2: i32) -> i32 {{
    {A}::async_event(a0, a1, a2)
}}
",
            ps.join(", ")
        );
        self.push(&[], &text);
        self.exports.push(lift);
        self.exports.push(callback);
        Ok(())
    }

    /// Generated code calling the user's exported function `f`: its
    /// path (or method name).
    pub(crate) fn user_fn(&self, f: &Function) -> String {
        match f.kind {
            FnKind::Free => format!("{}::{}", self.base, snake(&f.name)),
            FnKind::Method(_) => snake(&f.name),
            FnKind::Static(t) => format!(
                "{}::{}::{}",
                self.base,
                pascal(&self.r.types[t].name),
                snake(&f.name)
            ),
            FnKind::Constructor(t) => {
                format!("{}::{}::new", self.base, pascal(&self.r.types[t].name))
            }
        }
    }
}

impl Gen<'_> {
    pub(crate) fn export_iface(&mut self, i: IfaceId) -> Result<(), String> {
        let m = self.owner_mod(Owner::Iface(i));
        let key = self.r.iface_key(i);
        let module = format!("[export]{key}");
        for f in self.r.interfaces[i].funcs.clone() {
            let call = self.user_fn(&f);
            self.export_fn(&format!("{key}#{}", f.wit_name(self.r)), &module, &f, &call)?;
        }
        for (_, t) in self.r.interfaces[i].types.clone() {
            if matches!(self.r.types[t].kind, TypeKind::Resource) {
                let (.., table) = self.resource_fns(t);
                let name = &self.r.types[t].name;
                let export = format!("{key}#[dtor]{name}");
                let id = ident("dtor", &export, "");
                // The value leaves the table; the user's `Drop` runs.
                let body = if self.drops.contains(&pascal(name)) {
                    format!(
                        "    if let w_v: {}::{} = {table}.take(a0) {{\n        using w_u = w_v {{}}\n    }}\n",
                        self.base,
                        pascal(name)
                    )
                } else {
                    format!("    {table}.take(a0);\n")
                };
                self.push(
                    &m,
                    &format!(
                        "#[export_name(\"{export}\")]\nextern \"C\" fn {id}(a0: i32) {{\n{body}}}\n"
                    ),
                );
                self.exports.push(export);
            }
        }
        Ok(())
    }

    /// Every type `t` refers to, added to `out`.
    pub(crate) fn reach(&self, t: &Type, out: &mut BTreeSet<TypeId>) {
        match t {
            Type::Prim(_) | Type::ErrorContext => {}
            Type::List(e) | Type::Option(e) | Type::FixedList(e, _) => self.reach(e, out),
            Type::Future(e) | Type::Stream(e) => {
                if let Some(e) = e {
                    self.reach(e, out);
                }
            }
            Type::Result(a, b) => {
                for x in [a, b].into_iter().flatten() {
                    self.reach(x, out);
                }
            }
            Type::Tuple(ts) => ts.iter().for_each(|t| self.reach(t, out)),
            Type::Own(id) | Type::Borrow(id) | Type::Id(id) => {
                if !out.insert(*id) {
                    return;
                }
                match &self.r.types[*id].kind {
                    TypeKind::Alias(a) => self.reach(a, out),
                    TypeKind::Record(fs) => fs.iter().for_each(|(_, t)| self.reach(t, out)),
                    TypeKind::Variant(cs) => cs
                        .iter()
                        .filter_map(|(_, t)| t.as_ref())
                        .for_each(|t| self.reach(t, out)),
                    _ => {}
                }
            }
        }
    }
}
