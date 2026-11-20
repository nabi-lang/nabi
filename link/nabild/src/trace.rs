//! The function table a panic's backtrace reads: every live Nabi
//! function's address range and source name, in a read-only section of
//! the image.
//!
//! The runtime defines `nabi_backtrace_symbols`, a pointer-sized word
//! initialized to 1 (no table). The linker writes there the table's
//! address less the word's own, so the image needs no relocation for it.
//!
//! Layout (target byte order): `u32` count, `u32` version (1), then
//! per function, sorted by address, `i32` start less the table's
//! address, `u32` size, `u32` name offset from the table, `u32` name
//! length; the names (UTF-8) follow.

use crate::symtab::{AtomId, Def, SymbolTable};

/// The runtime's word that is told where the table is.
pub const ANCHOR: &str = "nabi_backtrace_symbols";

const HEADER: u64 = 8;
const ENTRY: u64 = 16;

/// The functions of a table, before layout: canonical atom, offset in
/// it and shown name.
#[derive(Debug, Default)]
pub struct Plan(Vec<(AtomId, u64, String)>);

impl Plan {
    /// The Nabi functions among `names` (`(symbol, atom, offset)`, each
    /// with the platform prefix `prefix`). `live` gives a live atom's
    /// canonical atom (a folded copy's kept one) and its size, `None` for
    /// a dead one. Copies at one place keep one name.
    pub fn new<'a>(
        names: impl Iterator<Item = (&'a str, AtomId, u64)>,
        prefix: &str,
        live: impl Fn(AtomId) -> Option<(AtomId, u64)>,
    ) -> Self {
        let mut v: Vec<(AtomId, u64, String)> = names
            .filter_map(|(sym, atom, off)| {
                let (atom, size) = live(atom)?;
                if off >= size {
                    return None;
                }
                let sym = sym.strip_prefix(prefix)?;
                // The C entry nabic writes around the program's `main`
                // (which it may inline): `nabi_main` under Windows.
                let shown = demangle(sym)
                    .or_else(|| matches!(sym, "main" | "nabi_main").then(|| "main".to_owned()))?;
                Some((atom, off, shown))
            })
            .collect();
        v.sort();
        v.dedup_by(|b, a| (a.0, a.1) == (b.0, b.1));
        Self(v)
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// The table's size in bytes.
    #[must_use]
    pub fn size(&self) -> u64 {
        HEADER + ENTRY * self.0.len() as u64 + self.0.iter().map(|f| f.2.len() as u64).sum::<u64>()
    }

    /// The table at `at`, `big` endian or little; `addr` gives an atom's
    /// address and size. A function ends at the next one or at its
    /// atom's end.
    #[must_use]
    pub fn table(&self, at: u64, big: bool, addr: impl Fn(AtomId) -> (u64, u64)) -> Vec<u8> {
        let mut f: Vec<(u64, u64, &str)> = self
            .0
            .iter()
            .map(|(a, off, name)| {
                let (base, size) = addr(*a);
                (base + off, base + size, name.as_str())
            })
            .collect();
        f.sort_unstable();
        for i in 1..f.len() {
            let next = f[i].0;
            f[i - 1].1 = f[i - 1].1.min(next);
        }
        let mut b = Vec::with_capacity(self.size() as usize);
        let put = |b: &mut Vec<u8>, v: u32| {
            b.extend_from_slice(&if big {
                v.to_be_bytes()
            } else {
                v.to_le_bytes()
            });
        };
        put(&mut b, f.len() as u32);
        put(&mut b, 1);
        let mut name = HEADER + ENTRY * f.len() as u64;
        for &(start, end, n) in &f {
            put(&mut b, start.wrapping_sub(at) as u32);
            put(&mut b, (end - start) as u32);
            put(&mut b, name as u32);
            put(&mut b, n.len() as u32);
            name += n.len() as u64;
        }
        for &(_, _, n) in &f {
            b.extend_from_slice(n.as_bytes());
        }
        b
    }
}

/// The defined global symbols of `syms`, as [`Plan::new`] takes them.
pub fn globals(syms: &SymbolTable) -> impl Iterator<Item = (&str, AtomId, u64)> {
    syms.syms.iter().filter_map(|s| match s.def {
        Def::Defined { atom, off } => Some((s.name.as_str(), atom, off)),
        _ => None,
    })
}

/// What the anchor holds: the table's address `table` less its own
/// `anchor`, in `width` bytes.
#[must_use]
pub fn anchor(table: u64, anchor: u64, width: usize, big: bool) -> Vec<u8> {
    let d = table.wrapping_sub(anchor);
    let mut v = if big {
        d.to_be_bytes().to_vec()
    } else {
        d.to_le_bytes().to_vec()
    };
    if big {
        v.drain(..8 - width);
    } else {
        v.truncate(width);
    }
    v
}

/// The source name of Nabi symbol `sym` (`_Y<pkg>_<path>_<name>_<hash>`):
/// `core::list::List::push`, `main::{closure#0}`; `None` for
/// any other symbol.
#[must_use]
pub fn demangle(sym: &str) -> Option<String> {
    let rest = sym.strip_prefix("_Y")?;
    let pkg = rest.get(..8)?;
    if !pkg.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    // A unit's copy of another package's item: `<symbol>.copy_Y<hash>_`.
    let rest = rest.get(8..)?.strip_prefix('_')?;
    let rest = rest.split_once('.').map_or(rest, |(r, _)| r);
    let body = rest.strip_suffix('_').unwrap_or_else(|| {
        // A generic instance ends in its 16-digit type hash.
        match rest.len().checked_sub(17).map(|i| rest.split_at(i)) {
            Some((b, h)) if h.starts_with('_') && h[1..].bytes().all(|c| c.is_ascii_hexdigit()) => {
                b
            }
            _ => rest,
        }
    });
    let mut segs = vec![String::new()];
    let mut it = body.chars().peekable();
    while let Some(c) = it.next() {
        if c != '_' {
            segs.last_mut()?.push(c);
            continue;
        }
        match it.peek() {
            Some('0') => {
                it.next();
                segs.last_mut()?.push('_');
            }
            Some('1') => {
                it.next();
                let hex: String = it.by_ref().take_while(|&c| c != '_').collect();
                segs.last_mut()?
                    .push(char::from_u32(u32::from_str_radix(&hex, 16).ok()?)?);
            }
            _ => segs.push(String::new()),
        }
    }
    if segs.first().is_some_and(String::is_empty) {
        segs.remove(0);
    }
    if segs.is_empty() || segs.iter().any(String::is_empty) {
        return None;
    }
    let mut out: Vec<String> = Vec::new();
    let last = segs.pop()?;
    out.extend(segs);
    // Methods are `Type__name`, impl items `__impl<hash>_name`, items
    // declared in a fn `outer/name`.
    for part in last.split("__") {
        if part.is_empty() {
            continue;
        }
        match part.strip_prefix("impl") {
            Some(r)
                if r.len() > 9
                    && r.as_bytes()[8] == b'_'
                    && r[..8].bytes().all(|b| b.is_ascii_hexdigit()) =>
            {
                out.push("<impl>".to_owned());
                out.push(r[9..].to_owned());
            }
            _ => out.push(nabic_core::local::qualified(part)),
        }
    }
    Some(out.join("::"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_read_as_source() {
        let d = |s: &str| demangle(s).unwrap();
        assert_eq!(d("_Yc4742f17__main_"), "main");
        assert_eq!(d("_Yc4742f17__P_0_0get_"), "P::get");
        assert_eq!(d("_Yc4742f17__g_ff08cf2bed97673e"), "g");
        assert_eq!(d("_Yc4742f17__main_12f_inner_"), "main::inner");
        assert_eq!(
            d("_Yc4742f17_main__17b_closure_123_0_17d__"),
            "main::{closure#0}"
        );
        assert_eq!(
            d("_Y0b3cd907_core_collections_list_List_0_0push_5b938ee770da2991"),
            "core::collections::list::List::push"
        );
        assert_eq!(
            d("_Y0b3cd907_core_array__0_0impldb1d67aa_0len_311a6d8962675d4c.copy_Y74606dff_"),
            "core::array::<impl>::len"
        );
        assert_eq!(demangle("nabi_panic_at"), None);
        assert_eq!(demangle("_Yzz_main_"), None);
    }

    #[test]
    fn table_ranges_end_at_the_next_function() {
        let names = [
            ("_Yc4742f17__b_", 1, 0),
            ("_Yc4742f17__a_", 0, 0),
            ("_Yc4742f17__a2_", 3, 0),
            ("other", 0, 8),
            ("_Yc4742f17__dead_", 2, 0),
        ];
        let p = Plan::new(names.iter().copied(), "", |a| match a {
            0 | 3 => Some((0, 0x40)),
            1 => Some((1, 0x10)),
            _ => None,
        });
        let t = p.table(0x2000, false, |a| {
            if a == 0 {
                (0x1000, 0x40)
            } else {
                (0x1020, 0x10)
            }
        });
        assert_eq!(t.len() as u64, p.size());
        assert_eq!(&t[..4], &2u32.to_le_bytes());
        assert_eq!(&t[8..12], &0x1000u32.wrapping_sub(0x2000).to_le_bytes());
        // `a` ends where `b` starts.
        assert_eq!(&t[12..16], &0x20u32.to_le_bytes());
        assert_eq!(&t[t.len() - 2..], b"ab");
        assert_eq!(anchor(0x2000, 0x1ff8, 4, true), [0, 0, 0, 8]);
    }
}
