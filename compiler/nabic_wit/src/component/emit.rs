//! Imports, core functions and exports.

#[allow(clippy::wildcard_imports)]
use crate::component::*;

impl Enc<'_> {
    pub(crate) fn import_group(&mut self, g: usize) -> Res<u32> {
        let m = self.m;
        if let Some(&i) = self.imported.get(&g) {
            return Ok(i);
        }
        if !self.importing.insert(g) {
            return Err(format!(
                "`{}`: interfaces depend on each other",
                self.g.name(g)
            ));
        }
        let mut sp = Space::Inst(InstSp {
            group: g,
            decls: Vec::new(),
            n: 0,
            types: 0,
            named: HashMap::default(),
            anon: HashMap::default(),
        });
        let live = self.live_funcs.get(&g).cloned().unwrap_or_default();
        let mut done = BTreeSet::new();
        let groups = self.g;
        for &i in &groups.groups[g].1 {
            for f in &m.ifaces[i].funcs {
                if !live.contains(&f.name) || !done.insert(f.name.clone()) {
                    continue;
                }
                let ft = self.functype(&mut sp, f)?;
                let Space::Inst(s) = &mut sp else {
                    unreachable!()
                };
                s.decls.extend_from_slice(&[0x04, 0x00]);
                bin::name(&mut s.decls, &f.name);
                s.decls.push(0x01);
                bin::u32(&mut s.decls, ft);
                s.n += 1;
            }
        }
        for name in self.live_types.get(&g).cloned().unwrap_or_default() {
            let t = self.g.ty(m, g, &name).ok_or("internal: lost type")?;
            self.named(&mut sp, t)?;
        }
        let Space::Inst(s) = sp else { unreachable!() };
        let mut b = vec![0x42];
        bin::usize(&mut b, s.n);
        b.extend_from_slice(&s.decls);
        let ty = self.b.ty(&b);
        let mut d = vec![0x05];
        bin::u32(&mut d, ty);
        let inst = self.b.import(self.g.name(g), &d);
        self.imported.insert(g, inst);
        Ok(inst)
    }

    /// The component function of world-level import `name`.
    pub(crate) fn root_func(&mut self, name: &str) -> Res<u32> {
        if let Some(&f) = self.root_funcs.get(name) {
            return Ok(f);
        }
        let r = self.root.ok_or("no world")?;
        let f = self.g.func(self.m, r, name).ok_or("no such function")?;
        let ft = self.functype(&mut Space::Outer, f)?;
        let mut d = vec![0x01];
        bin::u32(&mut d, ft);
        let i = self.b.import(name, &d);
        self.root_funcs.insert(name.to_owned(), i);
        Ok(i)
    }

    pub(crate) fn inst(&self, i: Inst) -> u32 {
        match i {
            Inst::Main => self.main,
            Inst::Adapter => self.adapter,
        }
        .unwrap_or(0)
    }

    /// Core instances to instantiate `by` with, one per module it imports.
    pub(crate) fn bags(&mut self, by: Inst) -> Res<Vec<(String, u32)>> {
        let mut order: Vec<String> = Vec::new();
        let mut items: HashMap<String, Vec<(String, u8, u32)>> = HashMap::default();
        let binds = std::mem::take(&mut self.binds);
        let r = self.bag_items(&binds, by, &mut order, &mut items);
        self.binds = binds;
        r?;
        let mut args = Vec::new();
        for module in order {
            let bag = self.b.core_bag(&items[&module]);
            args.push((module, bag));
        }
        Ok(args)
    }

    pub(crate) fn bag_items(
        &mut self,
        binds: &[Bind],
        by: Inst,
        order: &mut Vec<String>,
        items: &mut HashMap<String, Vec<(String, u8, u32)>>,
    ) -> Res<()> {
        for b in binds.iter().filter(|b| b.by == by) {
            let item = match &b.special {
                Some(Special::Memory) => {
                    let mem = self.mem.ok_or("the main module exports no `memory`")?;
                    (b.name.clone(), CORE_MEMORY, mem)
                }
                Some(Special::Main(n)) => {
                    let main = self.inst(Inst::Main);
                    (
                        b.name.clone(),
                        CORE_FUNC,
                        self.b.alias_core(main, n, CORE_FUNC),
                    )
                }
                None => {
                    let f = match b.slot {
                        Some(s) => {
                            let shim = self.shim.ok_or("internal: no shim")?;
                            self.b.alias_core(shim, &s.to_string(), CORE_FUNC)
                        }
                        None => self.emit_op(&b.op, by)?,
                    };
                    (b.name.clone(), CORE_FUNC, f)
                }
            };
            if !items.contains_key(&b.module) {
                order.push(b.module.clone());
            }
            items.entry(b.module.clone()).or_default().push(item);
        }
        Ok(())
    }

    pub(crate) fn realloc(&mut self, by: Inst, name: &'static str) -> Res<u32> {
        if let Some(&f) = self.reallocs.get(&(by, name)) {
            return Ok(f);
        }
        let has = match by {
            Inst::Main => self.main_exports.contains(name),
            Inst::Adapter => self.adapter_exports.contains(name),
        };
        if !has {
            return Err(format!("no `{name}` export for the canonical ABI"));
        }
        let inst = self.inst(by);
        let f = self.b.alias_core(inst, name, CORE_FUNC);
        self.reallocs.insert((by, name), f);
        Ok(f)
    }

    /// Canonical options `o` of an import of `by`, as a vector.
    pub(crate) fn put_opts(
        &mut self,
        o: Opts,
        by: Inst,
        extra: &[(u8, u32)],
        out: &mut Vec<u8>,
    ) -> Res<()> {
        let mut items: Vec<(u8, Option<u32>)> = Vec::new();
        if o.utf8 {
            items.push((0x00, None));
        }
        if o.memory {
            items.push((
                0x03,
                Some(self.mem.ok_or("the main module exports no `memory`")?),
            ));
        }
        if o.realloc {
            let name = match by {
                Inst::Main => "cabi_realloc",
                Inst::Adapter => "cabi_import_realloc",
            };
            items.push((0x04, Some(self.realloc(by, name)?)));
        }
        if o.async_ {
            items.push((0x06, None));
        }
        items.extend(extra.iter().map(|&(k, i)| (k, Some(i))));
        bin::usize(out, items.len());
        for (k, i) in items {
            out.push(k);
            if let Some(i) = i {
                bin::u32(out, i);
            }
        }
        Ok(())
    }

    /// The core function `op` defines for an import of `by`.
    pub(crate) fn emit_op(&mut self, op: &Op, by: Inst) -> Res<u32> {
        let mut b = Vec::new();
        match op {
            Op::Lower { g, name, opts } => {
                let f = if Some(*g) == self.root {
                    self.root_func(name)?
                } else {
                    let inst = self.import_group(*g)?;
                    self.b.alias_export(inst, name, FUNC)
                };
                b.extend_from_slice(&[0x01, 0x00]);
                bin::u32(&mut b, f);
                self.put_opts(*opts, by, &[], &mut b)?;
            }
            Op::ResDrop(t) | Op::ResNew(t) | Op::ResRep(t) => {
                b.push(match op {
                    Op::ResNew(_) => 0x02,
                    Op::ResDrop(_) => 0x03,
                    _ => 0x04,
                });
                let i = self.named(&mut Space::Outer, *t)?;
                bin::u32(&mut b, i);
            }
            Op::TaskReturn(r, opts) => {
                b.push(0x09);
                match r {
                    Some(v) => {
                        b.push(0);
                        self.val(&mut Space::Outer, v)?.put(&mut b);
                    }
                    None => b.extend_from_slice(&[1, 0]),
                }
                self.put_opts(*opts, by, &[], &mut b)?;
            }
            Op::Simple(c) => b.extend_from_slice(c),
            Op::WithMem(c) => {
                b.extend_from_slice(c);
                bin::u32(
                    &mut b,
                    self.mem.ok_or("the main module exports no `memory`")?,
                );
            }
            Op::WithOpts(c, opts) => {
                b.extend_from_slice(c);
                self.put_opts(*opts, by, &[], &mut b)?;
            }
            Op::Payload {
                code,
                ty,
                opts,
                flag,
            } => {
                b.push(*code);
                let Vt::T(i) = self.val(&mut Space::Outer, &Val::Id(*ty))? else {
                    unreachable!("a future or stream type")
                };
                bin::u32(&mut b, i);
                if let Some(o) = opts {
                    self.put_opts(*o, by, &[], &mut b)?;
                }
                if let Some(f) = flag {
                    b.push(u8::from(*f));
                }
            }
            Op::Export(i, name) => {
                let has = match i {
                    Inst::Main => self.main_exports.contains(name),
                    Inst::Adapter => self.adapter_exports.contains(name),
                };
                if !has {
                    return Err(format!("no core export `{name}`"));
                }
                let inst = self.inst(*i);
                return Ok(self.b.alias_core(inst, name, CORE_FUNC));
            }
        }
        Ok(self.b.canon(&b))
    }

    /// `canon lift` of `f`, core export `core` of `from`.
    pub(crate) fn lift(&mut self, f: &Func, core: &str, from: Inst) -> Res<u32> {
        let m = self.m;
        let exports = match from {
            Inst::Main => &self.main_exports,
            Inst::Adapter => &self.adapter_exports,
        };
        let name = if f.is_async {
            format!("[async-lift]{core}")
        } else {
            core.to_owned()
        };
        if !exports.contains(&name) {
            return Err(format!("the core module does not export `{name}`"));
        }
        let post = format!("cabi_post_{core}");
        let has_post = !f.is_async && exports.contains(&post);
        let callback = format!("[callback][async-lift]{core}");
        let has_callback = f.is_async && exports.contains(&callback);
        if f.is_async && !has_callback {
            return Err(format!("the core module does not export `{callback}`"));
        }
        let inst = self.inst(from);
        let cf = self.b.alias_core(inst, &name, CORE_FUNC);
        let mut extra = Vec::new();
        if has_post {
            extra.push((0x05, self.b.alias_core(inst, &post, CORE_FUNC)));
        }
        if has_callback {
            extra.push((0x07, self.b.alias_core(inst, &callback, CORE_FUNC)));
        }
        let pflat = m.flat_all(f.params.iter().map(|(_, t)| t));
        let pptr = pflat > MAX_FLAT_PARAMS || f.params.iter().any(|(_, t)| m.has_ptr(t));
        // An async result goes through `task.return`, yet a pointer in it
        // still asks for the memory.
        let rmem = f
            .result
            .as_ref()
            .is_some_and(|t| m.has_ptr(t) || (!f.is_async && m.flat(t) > 1));
        let opts = Opts {
            memory: pptr || rmem,
            realloc: false,
            utf8: self.has_string(f),
            async_: f.is_async,
        };
        let ft = self.functype(&mut Space::Outer, f)?;
        let mut b = vec![0x00, 0x00];
        bin::u32(&mut b, cf);
        if pptr {
            let r = match from {
                Inst::Main => "cabi_realloc",
                Inst::Adapter => "cabi_export_realloc",
            };
            extra.insert(0, (0x04, self.realloc(from, r)?));
        }
        self.put_opts(opts, from, &extra, &mut b)?;
        bin::u32(&mut b, ft);
        Ok(self.b.lift(&b))
    }

    /// Exported interface `i` (of `from`): its functions lifted, given to
    /// a component that exports them with the interface's types.
    pub(crate) fn export_iface(&mut self, i: usize, from: Inst) -> Res<()> {
        let m = self.m;
        let iface = &m.ifaces[i];
        let g = self.g.of[i];
        let mut lifted = Vec::new();
        for f in &iface.funcs {
            let core = format!("{}#{}", iface.key, f.name);
            lifted.push(self.lift(f, &core, from)?);
        }
        let mut sp = Space::Nest(Box::new(Nest {
            group: g,
            b: Builder::default(),
            exporting: false,
            imports: HashMap::default(),
            exports: HashMap::default(),
            anon: HashMap::default(),
            names: HashSet::default(),
            args: Vec::new(),
        }));
        for (_, t) in &iface.types {
            self.named(&mut sp, *t)?;
        }
        let mut funcs = Vec::new();
        for (f, &outer) in iface.funcs.iter().zip(&lifted) {
            let ft = self.functype(&mut sp, f)?;
            let Space::Nest(s) = &mut sp else {
                unreachable!()
            };
            let name = s.uniq(&import_func_name(&f.name));
            let mut d = vec![0x01];
            bin::u32(&mut d, ft);
            funcs.push(s.b.import(&name, &d));
            s.args.push((name, FUNC, outer));
        }
        if let Space::Nest(s) = &mut sp {
            s.exporting = true;
        }
        for (_, t) in &iface.types {
            self.named(&mut sp, *t)?;
        }
        for (f, idx) in iface.funcs.iter().zip(funcs) {
            let ft = self.functype(&mut sp, f)?;
            let Space::Nest(s) = &mut sp else {
                unreachable!()
            };
            let mut d = vec![0x01];
            bin::u32(&mut d, ft);
            s.b.export(&f.name, FUNC, idx, Some(&d));
        }
        let Space::Nest(s) = sp else { unreachable!() };
        let c = self.b.component(&s.b.finish());
        let inst = self.b.instantiate(c, &s.args);
        let name = self.g.name(g).to_owned();
        self.b.export(&name, INSTANCE, inst, None);
        Ok(())
    }
}
