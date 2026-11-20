//! Writing a component: items appended in order, consecutive ones of a
//! kind sharing a section, each index space counted as items enter it.

use crate::component::bin;

/// Component sorts.
pub const FUNC: u8 = 0x01;
pub const TYPE: u8 = 0x03;
pub const COMPONENT: u8 = 0x04;
pub const INSTANCE: u8 = 0x05;

/// Core sorts (inside a component, after a 0x00 byte).
pub const CORE_FUNC: u8 = 0x00;
pub const CORE_TABLE: u8 = 0x01;
pub const CORE_MEMORY: u8 = 0x02;

const SEC_CORE_MODULE: u8 = 1;
const SEC_CORE_INSTANCE: u8 = 2;
const SEC_COMPONENT: u8 = 4;
const SEC_INSTANCE: u8 = 5;
const SEC_ALIAS: u8 = 6;
const SEC_TYPE: u8 = 7;
const SEC_CANON: u8 = 8;
const SEC_IMPORT: u8 = 10;
const SEC_EXPORT: u8 = 11;

/// A value type: a primitive's byte or a type index.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Vt {
    P(u8),
    T(u32),
}

impl Vt {
    pub fn put(self, out: &mut Vec<u8>) {
        match self {
            Self::P(b) => out.push(b),
            Self::T(i) => bin::s64(out, i64::from(i)),
        }
    }
}

#[derive(Default)]
pub struct Builder {
    out: Vec<u8>,
    sec: u8,
    n: usize,
    buf: Vec<u8>,
    pub types: u32,
    pub funcs: u32,
    pub instances: u32,
    pub components: u32,
    pub modules: u32,
    pub core_instances: u32,
    pub core_funcs: u32,
    pub core_mems: u32,
    pub core_tables: u32,
}

fn bump(c: &mut u32) -> u32 {
    *c += 1;
    *c - 1
}

impl Builder {
    fn flush(&mut self) {
        if self.n > 0 {
            bin::vec_section(&mut self.out, self.sec, self.n, &self.buf);
        }
        self.n = 0;
        self.buf.clear();
    }

    fn item(&mut self, sec: u8, b: &[u8]) {
        if self.sec != sec {
            self.flush();
            self.sec = sec;
        }
        self.n += 1;
        self.buf.extend_from_slice(b);
    }

    fn raw(&mut self, sec: u8, b: &[u8]) {
        self.flush();
        bin::section(&mut self.out, sec, b);
    }

    pub fn finish(mut self) -> Vec<u8> {
        self.flush();
        let mut out = b"\0asm\x0d\0\x01\0".to_vec();
        out.extend_from_slice(&self.out);
        out
    }

    fn count(&mut self, sort: u8) -> u32 {
        match sort {
            FUNC => bump(&mut self.funcs),
            TYPE => bump(&mut self.types),
            COMPONENT => bump(&mut self.components),
            _ => bump(&mut self.instances),
        }
    }

    fn core_count(&mut self, sort: u8) -> u32 {
        match sort {
            CORE_FUNC => bump(&mut self.core_funcs),
            CORE_TABLE => bump(&mut self.core_tables),
            _ => bump(&mut self.core_mems),
        }
    }

    /// A type definition (`deftype` bytes).
    pub fn ty(&mut self, def: &[u8]) -> u32 {
        self.item(SEC_TYPE, def);
        bump(&mut self.types)
    }

    /// Import `name` of extern type `desc` (its first byte the sort).
    pub fn import(&mut self, name: &str, desc: &[u8]) -> u32 {
        let mut b = vec![0];
        bin::name(&mut b, name);
        b.extend_from_slice(desc);
        self.item(SEC_IMPORT, &b);
        self.count(desc[0])
    }

    /// Export item `idx` of `sort` as `name`, its type ascribed by
    /// `desc` if given.
    pub fn export(&mut self, name: &str, sort: u8, idx: u32, desc: Option<&[u8]>) -> u32 {
        let mut b = vec![0];
        bin::name(&mut b, name);
        b.push(sort);
        bin::u32(&mut b, idx);
        match desc {
            Some(d) => {
                b.push(1);
                b.extend_from_slice(d);
            }
            None => b.push(0),
        }
        self.item(SEC_EXPORT, &b);
        self.count(sort)
    }

    /// Export `name` of instance `inst`.
    pub fn alias_export(&mut self, inst: u32, name: &str, sort: u8) -> u32 {
        let mut b = vec![sort, 0];
        bin::u32(&mut b, inst);
        bin::name(&mut b, name);
        self.item(SEC_ALIAS, &b);
        self.count(sort)
    }

    /// Export `name` of core instance `inst`.
    pub fn alias_core(&mut self, inst: u32, name: &str, sort: u8) -> u32 {
        let mut b = vec![0, sort, 1];
        bin::u32(&mut b, inst);
        bin::name(&mut b, name);
        self.item(SEC_ALIAS, &b);
        self.core_count(sort)
    }

    /// A `canon` definition making a core function.
    pub fn canon(&mut self, b: &[u8]) -> u32 {
        self.item(SEC_CANON, b);
        bump(&mut self.core_funcs)
    }

    /// A `canon lift`.
    pub fn lift(&mut self, b: &[u8]) -> u32 {
        self.item(SEC_CANON, b);
        bump(&mut self.funcs)
    }

    pub fn module(&mut self, bytes: &[u8]) -> u32 {
        self.raw(SEC_CORE_MODULE, bytes);
        bump(&mut self.modules)
    }

    pub fn component(&mut self, bytes: &[u8]) -> u32 {
        self.raw(SEC_COMPONENT, bytes);
        bump(&mut self.components)
    }

    /// A core instance of exports `items` (name, core sort, index).
    pub fn core_bag(&mut self, items: &[(String, u8, u32)]) -> u32 {
        let mut b = vec![1];
        bin::usize(&mut b, items.len());
        for (n, s, i) in items {
            bin::name(&mut b, n);
            b.push(*s);
            bin::u32(&mut b, *i);
        }
        self.item(SEC_CORE_INSTANCE, &b);
        bump(&mut self.core_instances)
    }

    /// Core module `m` instantiated with core instances `args`.
    pub fn core_instantiate(&mut self, m: u32, args: &[(String, u32)]) -> u32 {
        let mut b = vec![0];
        bin::u32(&mut b, m);
        bin::usize(&mut b, args.len());
        for (n, i) in args {
            bin::name(&mut b, n);
            b.push(0x12);
            bin::u32(&mut b, *i);
        }
        self.item(SEC_CORE_INSTANCE, &b);
        bump(&mut self.core_instances)
    }

    /// Component `c` instantiated with `args` (name, sort, index).
    pub fn instantiate(&mut self, c: u32, args: &[(String, u8, u32)]) -> u32 {
        let mut b = vec![0];
        bin::u32(&mut b, c);
        bin::usize(&mut b, args.len());
        for (n, s, i) in args {
            bin::name(&mut b, n);
            b.push(*s);
            bin::u32(&mut b, *i);
        }
        self.item(SEC_INSTANCE, &b);
        bump(&mut self.instances)
    }
}
