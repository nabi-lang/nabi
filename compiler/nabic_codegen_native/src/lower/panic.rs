//! Panic sites, checks and the string parts their messages use.

#[allow(clippy::wildcard_imports)]
use crate::lower::*;

impl FnLower<'_, '_> {
    pub(crate) fn panic_label(&mut self, msg: &'static str) -> Label {
        let at = if self.f.facts.track_caller {
            "#caller".to_owned()
        } else {
            self.c.sources.location(self.at)
        };
        let p = Panic { msg, at };
        if let Some((_, l)) = self.panics.iter().find(|(q, _)| *q == p) {
            return *l;
        }
        let l = self.label();
        self.panics.push((p, l));
        l
    }

    /// Panic with `msg` when the `I8` `fail` is set.
    pub(crate) fn check(&mut self, fail: V, msg: &'static str) {
        // C leaves these undefined; the machine's answer stands.
        if self.cfn {
            return;
        }
        let p = self.panic_label(msg);
        let ok = self.label();
        self.push(Inst::Br {
            c: fail,
            t: p,
            f: ok,
        });
        self.push(Inst::Label(ok));
    }

    pub(crate) fn cmp(&mut self, cc: Cc, k: K, signed: bool, a: V, b: V) -> V {
        let dst = self.tmp_k(K::I8);
        self.push(Inst::Cmp {
            cc,
            k,
            signed,
            dst,
            a,
            b,
        });
        V::M(dst)
    }

    /// `nabi_str_len(s)` (and `nabi_fmt_len`), `nabi_str_byte(s, i)` and
    /// `nabi_str_bytes_buf(s)` read the value in place: the length word,
    /// the byte (0 past the end), the pointer word less the header.
    pub(crate) fn str_part(&mut self, sym: SymId, args: &[Operand], dst: Option<Mem>) -> bool {
        let name = self.c.obj.syms[sym as usize].name.as_str();
        let (byte, buf) = match (name, args.len()) {
            ("nabi_str_len" | "nabi_fmt_len", 1) => (false, false),
            ("nabi_str_byte", 2) => (true, false),
            ("nabi_str_bytes_buf", 1) => (false, true),
            _ => return false,
        };
        let Some(d) = dst else {
            // Nothing reads it, and neither has an effect.
            return true;
        };
        let w = self.word();
        let ws = w.size();
        let str_ty = MirTy::Prim(PrimTy::Str);
        let v = self.val(&args[0], &str_ty);
        let m = self.mem_of(v, &str_ty);
        if buf {
            // The buffer word: `HEADER_SIZE` before the bytes.
            self.push(Inst::Bin {
                op: Bin::Sub,
                k: w,
                signed: false,
                dst: d,
                a: V::M(m),
                b: V::I(i64::from(HEADER_SIZE)),
            });
            return true;
        }
        if !byte {
            self.push(Inst::Mov {
                k: w,
                dst: d,
                src: V::M(m.at(ws)),
            });
            return true;
        }
        let (i, _) = self.scalar(&args[1], &MirTy::Prim(PrimTy::Usize));
        let inside = self.cmp(Cc::Lt, w, false, i, V::M(m.at(ws)));
        let [load, zero, done] = [(); 3].map(|()| self.label());
        self.push(Inst::Br {
            c: inside,
            t: load,
            f: zero,
        });
        self.push(Inst::Label(load));
        let p = self.word_tmp();
        self.push(Inst::Bin {
            op: Bin::Add,
            k: w,
            signed: false,
            dst: p,
            a: V::M(m),
            b: i,
        });
        let Mem::Frame(ps, _) = p else { unreachable!() };
        self.push(Inst::Mov {
            k: K::I8,
            dst: d,
            src: V::M(Mem::Ind(ps, 0)),
        });
        self.push(Inst::Jmp(done));
        self.push(Inst::Label(zero));
        self.push(Inst::Mov {
            k: K::I8,
            dst: d,
            src: V::I(0),
        });
        self.push(Inst::Jmp(done));
        self.push(Inst::Label(done));
        true
    }

    pub(crate) fn str_lit(&mut self, text: &str, dst: Mem) {
        let sym = self.c.str_sym(text);
        let w = self.word();
        self.push(Inst::Sym {
            dst,
            sym,
            got: false,
        });
        self.push(Inst::Mov {
            k: w,
            dst: dst.at(self.pw() / 8),
            src: V::I(text.len() as i64),
        });
    }

    pub(crate) fn emit_panics(&mut self) {
        let str_ty = MirTy::Prim(PrimTy::Str);
        for (p, l) in std::mem::take(&mut self.panics) {
            self.push(Inst::Label(l));
            let msg = self.tmp(&str_ty);
            self.str_lit(p.msg, msg);
            let at = if p.at == "#caller" {
                let last = *self.f.params.last().expect("track_caller location");
                self.local_mem(last).expect("location slot")
            } else {
                let m = self.tmp(&str_ty);
                self.str_lit(&p.at, m);
                m
            };
            let sym = self.c.sym("nabi_panic_at");
            self.call_runtime(
                sym,
                &[str_ty.clone(), str_ty.clone()],
                &MirTy::Unit,
                &[Val::M(msg), Val::M(at)],
                None,
            );
            self.push(Inst::Trap);
        }
    }
}
