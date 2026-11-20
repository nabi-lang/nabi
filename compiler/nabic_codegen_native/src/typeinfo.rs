//! Type descriptors in read-only data (`nabir_abi::typedesc`), and the
//! function that registers them with the runtime at startup. Only the
//! glue emits them, so a program without reflection has none.

use crate::lir::{Func, Inst, SymId};
use crate::lower::{Ctx, FnLower};
use crate::obj::Sect;
use crate::wrap;
use nabic_ir_common::typedesc::{AttrDesc, FieldDesc, TypeDesc};
use nabic_ir_common::{BlockId, ConstVal, FunctionId, LocalId, PrimOp, PrimTy};
use nabic_mir::plugin::Mode;
use nabic_mir::typedesc_abi::{self as abi, Word};
use nabic_mir::{
    BasicBlock, Link, LocalDecl, MirModule, MirTy, Operand, RValue, Statement, Terminator,
};

/// The generated function `nabi.register_types` calls.
pub const REGISTER: &str = "nabi.register_typeinfo";
/// The descriptor pointers, in `MirModule::type_descs` order.
pub const LIST: &str = "nabi.typeinfo.list";

/// Bytes and relocations of one data object, built word by word.
struct Out {
    bytes: Vec<u8>,
    relocs: Vec<(u32, SymId)>,
    pw: u32,
    be: bool,
}

impl Out {
    const fn new(pw: u32, be: bool) -> Self {
        Self {
            bytes: Vec::new(),
            relocs: Vec::new(),
            pw,
            be,
        }
    }

    fn int(&mut self, v: u64, n: usize) {
        if self.be {
            self.bytes.extend_from_slice(&v.to_be_bytes()[8 - n..]);
        } else {
            self.bytes.extend_from_slice(&v.to_le_bytes()[..n]);
        }
    }

    fn word(&mut self, v: u64) {
        self.int(v, self.pw as usize);
    }

    fn ptr(&mut self, sym: Option<SymId>) {
        if let Some(s) = sym {
            self.relocs.push((self.bytes.len() as u32, s));
        }
        self.word(0);
    }

    fn slice(&mut self, sym: Option<SymId>, len: usize) {
        self.ptr(sym.filter(|_| len > 0));
        self.word(len as u64);
    }

    fn define(self, ctx: &mut Ctx<'_>, sym: SymId, words: &[Word]) {
        debug_assert!(
            words.is_empty() || self.bytes.len() % abi::size_of(words, self.pw) as usize == 0,
            "descriptor layout"
        );
        let sect = if self.relocs.is_empty() {
            Sect::Const
        } else {
            Sect::DataConst
        };
        let off = ctx
            .obj
            .define_data(sym, sect, &self.bytes, self.pw.max(8), true);
        for (o, s) in self.relocs {
            ctx.obj.abs64(sect, off + o, s);
        }
        let s = &mut ctx.obj.syms[sym as usize];
        s.weak = true;
        s.hidden = true;
    }
}

/// `text` as a `Str`.
fn str_of(ctx: &mut Ctx<'_>, out: &mut Out, text: &str) {
    let sym = (!text.is_empty()).then(|| ctx.obj.bytes(text.as_bytes()));
    out.slice(sym, text.len());
}

/// `{path}` in `key` as `Y<pkg8>:path`.
pub fn real_key(link: &Link, key: &str) -> String {
    let mut out = String::with_capacity(key.len() + 16);
    let mut rest = key;
    while let Some(open) = rest.find('{') {
        out.push_str(&rest[..open]);
        let Some(close) = rest[open..].find('}') else {
            break;
        };
        let path = &rest[open + 1..open + close];
        out.push('Y');
        out.push_str(link.hash_of(path));
        out.push(':');
        out.push_str(path);
        rest = &rest[open + close + 1..];
    }
    out.push_str(rest);
    out
}

/// A type key's or signature's `u64`: the first 8 bytes of its SHA-256.
pub fn key_hash(key: &str) -> u64 {
    let h = nabic_core::hash::sha256(key.as_bytes());
    u64::from_le_bytes(h[..8].try_into().expect("8 bytes"))
}

/// The code word of function `id`'s value (its `.v` thunk when it takes
/// `nabi_tls`).
fn fn_code(ctx: &mut Ctx<'_>, id: Option<FunctionId>) -> Option<SymId> {
    let sig = ctx.fns.get(&id?)?;
    if sig.tls {
        let name = format!("{}.v", ctx.obj.syms[sig.sym as usize].name);
        ctx.thunks.insert(id?);
        Some(ctx.sym(&name))
    } else {
        Some(sig.sym)
    }
}

fn fn_value(ctx: &mut Ctx<'_>, out: &mut Out, id: Option<FunctionId>) {
    let code = fn_code(ctx, id);
    out.ptr(code);
    out.word(0);
}

struct Emit<'m> {
    descs: &'m [TypeDesc<MirTy>],
    syms: Vec<SymId>,
    keys: Vec<String>,
    /// Tag index of each descriptor.
    tag_index: Vec<u32>,
    /// Hot builds' `init_default` thunks to make: `(symbol, default
    /// function, field offset, field type)`.
    inits: std::cell::RefCell<Vec<(SymId, FunctionId, u64, MirTy)>>,
}

impl Emit<'_> {
    fn type_ref(&self, ctx: &mut Ctx<'_>, out: &mut Out, i: u32) {
        let d = &self.descs[i as usize];
        str_of(ctx, out, &d.name);
        out.int(key_hash(&self.keys[i as usize]), 8);
        out.ptr(Some(self.syms[i as usize]));
        out.word(u64::from(d.kind));
    }

    fn attrs(&self, ctx: &mut Ctx<'_>, owner: &str, attrs: &[AttrDesc]) -> Option<SymId> {
        if attrs.is_empty() {
            return None;
        }
        let pw = ctx.t.pointer_width / 8;
        let be = ctx.obj.be;
        let mut out = Out::new(pw, be);
        for (k, a) in attrs.iter().enumerate() {
            let value = self.attr_value(ctx, &format!("{owner}.a{k}"), a);
            self.type_ref(ctx, &mut out, a.ty);
            out.ptr(Some(value));
            out.word(0);
        }
        let sym = ctx.sym(&format!("{owner}.a"));
        out.define(ctx, sym, abi::ATTR_DESC);
        Some(sym)
    }

    /// An attribute struct's value in its own layout.
    fn attr_value(&self, ctx: &mut Ctx<'_>, name: &str, a: &AttrDesc) -> SymId {
        let t = ctx.t;
        let ty = &self.descs[a.ty as usize].ty;
        let (size, _) = t.size_align(ty);
        let offs = t.field_offsets(ty);
        let tys: Vec<MirTy> = match ty {
            MirTy::Struct(_, ps) => ps.clone(),
            _ => Vec::new(),
        };
        let pw = t.pointer_width / 8;
        let mut out = Out::new(pw, ctx.obj.be);
        out.bytes = vec![0; size.max(1) as usize];
        for ((v, off), fty) in a.values.iter().zip(&offs).zip(&tys) {
            let mut part = Out::new(pw, ctx.obj.be);
            let MirTy::Prim(p) = fty else { continue };
            let n = t.size_align(fty).0 as usize;
            match (v, p) {
                (ConstVal::Int { value, .. }, _) if p.is_int() => part.int(*value as u64, n),
                (ConstVal::Int { value, .. }, PrimTy::F32) => {
                    part.int(u64::from((*value as f32).to_bits()), 4);
                }
                (ConstVal::Int { value, .. }, PrimTy::F64) => {
                    part.int((*value as f64).to_bits(), 8);
                }
                (ConstVal::Float { bits, .. }, PrimTy::F32) => {
                    part.int(u64::from((f64::from_bits(*bits) as f32).to_bits()), 4);
                }
                (ConstVal::Float { bits, .. }, PrimTy::F64) => part.int(*bits, 8),
                (ConstVal::Bool(b), _) => part.int(u64::from(*b), 1),
                (ConstVal::Char(c), _) => part.int(u64::from(*c), 4),
                (ConstVal::Str(s), PrimTy::Str) => str_of(ctx, &mut part, s),
                _ => continue,
            }
            let at = *off as usize;
            out.bytes[at..at + part.bytes.len()].copy_from_slice(&part.bytes);
            out.relocs
                .extend(part.relocs.into_iter().map(|(o, s)| (o + *off, s)));
        }
        let sym = ctx.sym(name);
        out.define(ctx, sym, &[]);
        sym
    }

    fn fields(
        &self,
        ctx: &mut Ctx<'_>,
        owner: &str,
        fields: &[FieldDesc],
        offsets: &[u64],
    ) -> Option<SymId> {
        if fields.is_empty() {
            return None;
        }
        let pw = ctx.t.pointer_width / 8;
        let mut out = Out::new(pw, ctx.obj.be);
        for (k, f) in fields.iter().enumerate() {
            let attrs = self.attrs(ctx, &format!("{owner}.{k}"), &f.attrs);
            str_of(ctx, &mut out, &f.name);
            str_of(ctx, &mut out, &f.renamed_from);
            self.type_ref(ctx, &mut out, f.ty);
            out.slice(attrs, f.attrs.len());
            let at = if f.offset == abi::NO_OFFSET {
                offsets
                    .get(f.index as usize)
                    .copied()
                    .unwrap_or(abi::NO_OFFSET)
            } else {
                f.offset
            };
            out.int(at, 8);
            out.int(u64::from(f.flags), 4);
            out.int(u64::from(f.index), 4);
            let init = f
                .init_default
                .filter(|id| at != abi::NO_OFFSET && ctx.fns.contains_key(id));
            match init {
                Some(id) => {
                    let sym = ctx.sym(&format!("{owner}.{k}.init"));
                    let ty = self.descs[f.ty as usize].ty.clone();
                    self.inits.borrow_mut().push((sym, id, at, ty));
                    out.ptr(Some(sym));
                    out.word(0);
                }
                None => fn_value(ctx, &mut out, None),
            }
        }
        let sym = ctx.sym(&format!("{owner}.f"));
        out.define(ctx, sym, abi::FIELD_DESC);
        Some(sym)
    }

    fn ops(&self, ctx: &mut Ctx<'_>, i: usize) -> Option<SymId> {
        let d = &self.descs[i];
        let owner = &d.symbol;
        let pw = ctx.t.pointer_width / 8;
        let container = |ctx: &mut Ctx<'_>, name: &str, fns: &[Option<FunctionId>]| {
            let mut out = Out::new(pw, ctx.obj.be);
            for f in fns {
                fn_value(ctx, &mut out, *f);
            }
            for _ in fns.len()..4 {
                fn_value(ctx, &mut out, None);
            }
            let sym = ctx.sym(&format!("{owner}.{name}"));
            out.define(ctx, sym, abi::CONTAINER_OPS);
            sym
        };
        let list = d.list.map(|l| container(ctx, "list", &l));
        let dict = d.dict.map(|l| container(ctx, "dict", &l));
        if d.ops.iter().all(Option::is_none) && list.is_none() && dict.is_none() {
            return None;
        }
        let mut out = Out::new(pw, ctx.obj.be);
        for f in d.ops {
            fn_value(ctx, &mut out, f);
        }
        out.ptr(list);
        out.ptr(dict);
        let sym = ctx.sym(&format!("{owner}.ops"));
        out.define(ctx, sym, abi::TYPE_OPS);
        Some(sym)
    }

    /// Hash of what a layout check compares.
    fn layout_hash(&self, ctx: &Ctx<'_>, i: usize, offsets: &[u64]) -> u64 {
        let d = &self.descs[i];
        let t = ctx.t;
        let (size, align) = t.size_align(&d.ty);
        let mut text = format!("{size} {align}");
        for (f, o) in d.fields.iter().zip(offsets) {
            text.push_str(&format!(" {}@{o}:{}", f.name, self.keys[f.ty as usize]));
        }
        for v in &d.variants {
            text.push_str(&format!(" {}={}", v.name, v.tag));
            for f in &v.fields {
                text.push_str(&format!(" {}:{}", f.name, self.keys[f.ty as usize]));
            }
        }
        for w in t.root_offsets(&d.ty) {
            text.push_str(&format!(" r{w}"));
        }
        key_hash(&text)
    }

    fn desc(&self, ctx: &mut Ctx<'_>, i: usize) {
        let d = &self.descs[i];
        let t = ctx.t;
        let pw = t.pointer_width / 8;
        let owner = d.symbol.clone();
        let offsets: Vec<u64> = t.field_offsets(&d.ty).into_iter().map(u64::from).collect();
        let fields = self.fields(ctx, &owner, &d.fields, &offsets);
        let variants = if d.variants.is_empty() {
            None
        } else {
            let mut out = Out::new(pw, ctx.obj.be);
            for (k, v) in d.variants.iter().enumerate() {
                let fs = self.fields(ctx, &format!("{owner}.v{k}"), &v.fields, &[]);
                let attrs = self.attrs(ctx, &format!("{owner}.v{k}"), &v.attrs);
                // A sum's variants are named by their members' keys.
                str_of(ctx, &mut out, &real_key(&ctx.pkgs.link, &v.name));
                out.slice(fs, v.fields.len());
                out.int(u64::from(v.tag), 4);
                out.int(k as u64, 4);
                out.slice(attrs, v.attrs.len());
            }
            let sym = ctx.sym(&format!("{owner}.v"));
            out.define(ctx, sym, abi::VARIANT_DESC);
            Some(sym)
        };
        let generics = if d.generics.is_empty() {
            None
        } else {
            let mut out = Out::new(pw, ctx.obj.be);
            for g in &d.generics {
                self.type_ref(ctx, &mut out, *g);
            }
            let sym = ctx.sym(&format!("{owner}.g"));
            out.define(ctx, sym, abi::TYPE_REF);
            Some(sym)
        };
        let attrs = self.attrs(ctx, &owner, &d.attrs);
        let ops = self.ops(ctx, i);
        let layout = self.layout_hash(ctx, i, &offsets);
        let (size, align) = t.size_align(&d.ty);
        let mut out = Out::new(pw, ctx.obj.be);
        out.int(u64::from(abi::MAGIC), 4);
        out.int(u64::from(abi::VERSION), 2);
        out.int(u64::from(d.kind), 2);
        out.int(u64::from(d.flags), 4);
        out.int(u64::from(self.tag_index[i]), 4);
        out.int(key_hash(&self.keys[i]), 8);
        out.int(layout, 8);
        out.int(u64::from(size), 8);
        out.int(u64::from(align), 4);
        out.int(0, 4);
        str_of(ctx, &mut out, &self.keys[i]);
        str_of(ctx, &mut out, &d.name);
        str_of(ctx, &mut out, &d.path);
        out.slice(fields, d.fields.len());
        out.slice(variants, d.variants.len());
        out.slice(generics, d.generics.len());
        out.slice(attrs, d.attrs.len());
        out.ptr(ops);
        out.word(0);
        debug_assert_eq!(
            out.bytes.len(),
            abi::size_of(abi::TYPE_DESC, pw) as usize,
            "TypeDesc layout"
        );
        let sym = self.syms[i];
        out.define(ctx, sym, abi::TYPE_DESC);
    }
}

/// Every descriptor of `module`, and `REGISTER` into `funcs` unless it
/// is a plugin's.
pub fn emit(module: &MirModule, ctx: &mut Ctx<'_>, funcs: &mut Vec<(Func, bool, bool)>) {
    let descs = &module.type_descs;
    if descs.is_empty() {
        return;
    }
    let keys: Vec<String> = descs
        .iter()
        .map(|d| real_key(&ctx.pkgs.link, &d.key))
        .collect();
    let syms: Vec<SymId> = descs.iter().map(|d| ctx.sym(&d.symbol)).collect();
    // A plugin's descriptors index its module's tag table, which the
    // loader fills; a program's its own list below.
    let plugin = module.plugin.mode == Mode::Plugin;
    let mut tag_index = Vec::with_capacity(descs.len());
    let mut tagged: Vec<(usize, u32)> = Vec::new();
    for (i, d) in descs.iter().enumerate() {
        let index = match d.tag {
            Some(t) if plugin => module.plugin.tag_index(t).map(|i| i as u32),
            Some(t) => {
                tagged.push((i, t));
                Some(u32::try_from(tagged.len() - 1).expect("tag count"))
            }
            None => None,
        };
        tag_index.push(index.unwrap_or(abi::NO_TAG));
    }
    let e = Emit {
        descs,
        syms,
        keys,
        tag_index,
        inits: std::cell::RefCell::default(),
    };
    for i in 0..descs.len() {
        e.desc(ctx, i);
    }
    for (sym, id, at, ty) in e.inits.take() {
        let name = ctx.obj.syms[sym as usize].name.clone();
        let f = init_thunk(&name, id, at, ty);
        funcs.push((FnLower::new(ctx, &f, sym).lower(), false, false));
    }
    // The registration's tables: descriptor pointers, tag keys, tags.
    let pw = ctx.t.pointer_width / 8;
    let mut list = Out::new(pw, ctx.obj.be);
    for s in &e.syms {
        list.ptr(Some(*s));
    }
    let list_sym = ctx.sym(LIST);
    list.define(ctx, list_sym, &[]);
    ctx.obj.syms[list_sym as usize].weak = false;
    ctx.obj.syms[list_sym as usize].global = false;
    // A plugin's list is its descriptor's `types`, registered by the
    // loader.
    if plugin {
        return;
    }
    let mut keys = Out::new(pw, ctx.obj.be);
    let mut tags = Out::new(pw, ctx.obj.be);
    for &(i, t) in &tagged {
        str_of(ctx, &mut keys, &e.keys[i]);
        keys.int(abi::fnv64(e.keys[i].as_bytes()), 8);
        tags.int(u64::from(t), 4);
    }
    let keys_sym = ctx.sym("nabi.typeinfo.keys");
    let tags_sym = ctx.sym("nabi.typeinfo.tags");
    if tagged.is_empty() {
        keys.word(0);
        tags.int(0, 4);
    }
    keys.define(ctx, keys_sym, &[]);
    let off = ctx
        .obj
        .define_data(tags_sym, Sect::Data, &tags.bytes, 4, false);
    let _ = off;
    ctx.obj.syms[keys_sym as usize].weak = false;
    ctx.obj.syms[keys_sym as usize].global = false;
    let reg = ctx.sym(REGISTER);
    let n = (descs.len(), tagged.len());
    let func = wrap::generated(ctx, REGISTER, reg, nabic_diagnostic::Span::DUMMY, |l| {
        l.register_typeinfo([list_sym, keys_sym, tags_sym], n)
    });
    funcs.push((func, false, false));
}

/// `fn(env, owner: usize)`: field default `id()` stored `at` bytes into
/// the value at `owner` (`FieldDesc::init_default`).
fn init_thunk(name: &str, id: FunctionId, at: u64, ty: MirTy) -> nabic_mir::MirFunction {
    let w = MirTy::Prim(PrimTy::Usize);
    let mut f = wrap::shell(name);
    f.params = vec![LocalId(0), LocalId(1)];
    f.param_tys = vec![w.clone(), w.clone()];
    f.locals = [w.clone(), w.clone(), ty.clone(), w.clone()]
        .into_iter()
        .zip(0..)
        .map(|(ty, i)| LocalDecl {
            id: LocalId(i),
            name: None,
            ty,
        })
        .collect();
    f.blocks = vec![BasicBlock {
        id: BlockId(0),
        params: Vec::new(),
        stmts: vec![
            Statement::Assign {
                target: LocalId(2),
                rvalue: RValue::Call {
                    callee: id,
                    args: Vec::new(),
                    ret_ty: ty.clone(),
                },
            },
            Statement::Assign {
                target: LocalId(3),
                rvalue: RValue::BinOp {
                    op: PrimOp::Add,
                    op_ty: w,
                    lhs: Operand::Copy(LocalId(1)),
                    rhs: Operand::Const(ConstVal::UInt {
                        value: u128::from(at),
                        ty: PrimTy::Usize,
                    }),
                },
            },
            Statement::RawStore {
                addr: Operand::Copy(LocalId(3)),
                value: Operand::Copy(LocalId(2)),
                ty,
                volatile: false,
            },
        ],
        terminator: Terminator::Return(None),
    }];
    f
}

impl FnLower<'_, '_> {
    /// `nabi_types_register(list, n, keys, tags, nt)`.
    fn register_typeinfo(mut self, syms: [SymId; 3], (n, nt): (usize, usize)) -> Func {
        let w = MirTy::Prim(PrimTy::Usize);
        let k = self.word();
        let addr = |l: &mut Self, sym: SymId| {
            let p = l.word_tmp();
            l.push(Inst::Sym {
                dst: p,
                sym,
                got: false,
            });
            l.val_of(p, &w)
        };
        let list = addr(&mut self, syms[0]);
        let keys = addr(&mut self, syms[1]);
        let tags = addr(&mut self, syms[2]);
        let args = [
            list,
            Self::imm(n as i64, k),
            keys,
            tags,
            Self::imm(nt as i64, k),
        ];
        let reg = self.c.sym("nabi_types_register");
        // Its `i32` result is declared: wasm checks signatures.
        let r = self.tmp_k(crate::lir::K::I32);
        self.call_sym(
            reg,
            &[w.clone(), w.clone(), w.clone(), w.clone(), w],
            &MirTy::Prim(PrimTy::I32),
            &args,
            Some(r),
        );
        self.push(Inst::Ret { vals: Vec::new() });
        self.out
    }
}

/// The registration function, when `emit` made one.
pub fn register_sym(ctx: &mut Ctx<'_>) -> Option<SymId> {
    ctx.obj.named(REGISTER).then(|| ctx.sym(REGISTER))
}
