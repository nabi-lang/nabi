//! A function's code as assembly source: the encoder's text with branch
//! targets as local labels and each relocated field as the expression an
//! assembler turns back into that relocation. An instruction an assembler
//! would encode otherwise is forced (`{disp32}`, a `.s` suffix) or left
//! as its bytes, its text in a comment.

use crate::asm::Listing;
use crate::lir::SymId;
use crate::obj::{Obj, RelKind};
use std::fmt::Write;

/// A function as source (`Obj::src`): `name` as the object spells it,
/// `len` bytes of code.
pub struct FnSrc {
    pub name: String,
    pub len: u32,
    pub text: String,
    /// `(code offset, where in text)` of each instruction's line, past
    /// its labels, and of the end: where unwind directives go.
    pub marks: Vec<(u32, usize)>,
}

/// The instruction set and object format of the code.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Isa {
    X64,
    I386,
    A64,
    Arm,
    Riscv,
    Ppc64,
    S390x,
}

impl Isa {
    pub const fn of(obj: &Obj) -> Self {
        if obj.x86 {
            Self::X64
        } else if obj.i386 {
            Self::I386
        } else if obj.arm {
            Self::Arm
        } else if obj.riscv {
            Self::Riscv
        } else if obj.ppc64 {
            Self::Ppc64
        } else if obj.s390x {
            Self::S390x
        } else {
            Self::A64
        }
    }

    /// The assembler's line comment.
    pub const fn comment(self) -> &'static str {
        match self {
            Self::A64 => "//",
            Self::Arm => "@",
            _ => "#",
        }
    }
}

/// A local label: `.L` on ELF and COFF, `L` on Mach-O and i386 COFF
/// (as LLVM spells them, else they stay in the symbol table).
pub fn local(obj: &Obj, name: &str) -> String {
    if obj.elf || (obj.coff && !obj.i386) {
        format!(".L{name}")
    } else {
        format!("L{name}")
    }
}

/// `name` as an assembler operand, quoted unless plain.
pub fn quote(name: &str) -> String {
    let plain = !name.is_empty()
        && !name.starts_with(|c: char| c.is_ascii_digit())
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'$'));
    if plain {
        name.to_owned()
    } else {
        let mut s = String::from("\"");
        for c in name.chars() {
            if c == '"' || c == '\\' {
                s.push('\\');
            }
            s.push(c);
        }
        s.push('"');
        s
    }
}

/// `+ k` as an addend to an expression (empty for 0).
fn plus(k: i64) -> String {
    match k {
        0 => String::new(),
        k if k < 0 => format!("-{:#x}", k.unsigned_abs()),
        k => format!("+{k:#x}"),
    }
}

/// One instruction's text: in source, or its bytes.
enum Out {
    Text(String),
    Bytes(String),
}

/// Append `l` as source to `obj.src`: `lines` are the encoder's
/// `(offset, text)`, as the listing has them.
pub fn function(obj: &mut Obj, l: &Listing<'_>, lines: &[(u32, String)]) {
    let isa = Isa::of(obj);
    let f = obj.src.as_ref().map_or(0, Vec::len);
    let len = l.code.len() as u32;
    let mut relocs = l.relocs.to_vec();
    relocs.sort_by_key(|r| r.0);
    // Labels by offset: the encoder's, then any other branch target.
    let mut labels: Vec<(u32, String)> = l
        .labels
        .iter()
        .map(|&(at, n)| (at, local(obj, &format!("BB{f}_{n}"))))
        .collect();
    // RISC-V: the `auipc` of each `pcrel_lo`, as the ELF writer numbers
    // them in the text section's order.
    let pcrel: Vec<(u32, String)> = if isa == Isa::Riscv {
        let before = obj.sects.get(&crate::obj::Sect::Text).map_or(0, |t| {
            t.relocs
                .iter()
                .filter(|x| x.kind == RelKind::RvPcLoI)
                .count()
        });
        let mut los: Vec<u32> = relocs
            .iter()
            .filter(|r| r.1 == RelKind::RvPcLoI)
            .map(|r| r.0 - 4)
            .collect();
        los.sort_unstable();
        los.into_iter()
            .enumerate()
            .map(|(n, at)| (at, format!(".Lpcrel_hi{}", before + n)))
            .collect()
    } else {
        Vec::new()
    };
    // powerpc64: the global entry, which the TOC pointer's setup counts
    // from.
    if relocs.iter().any(|r| r.1 == RelKind::PpcEntryHa) {
        labels.push((0, local(obj, &format!("gep{f}"))));
    }
    let mut outs: Vec<(u32, Out)> = Vec::with_capacity(lines.len());
    let mut skip = false;
    for (n, (at, text)) in lines.iter().enumerate() {
        if std::mem::take(&mut skip) {
            continue;
        }
        let end = lines.get(n + 1).map_or(len, |x| x.0);
        let code = &l.code[*at as usize..end as usize];
        let rs: Vec<(u32, RelKind, SymId)> = relocs
            .iter()
            .filter(|r| (*at..end).contains(&r.0))
            .map(|&(off, k, s)| (off - at, k, s))
            .collect();
        let out = if rs.is_empty() {
            plain(isa, text, code)
        } else {
            let next = lines.get(n + 1).map(|x| x.1.as_str());
            match reloc(obj, isa, text, code, &rs, next, &pcrel, f, *at) {
                Some((t, both)) => {
                    skip = both;
                    Out::Text(t)
                }
                None => Out::Bytes(text.clone()),
            }
        };
        let out = match out {
            Out::Text(t) => Out::Text(targets(obj, isa, &t, f, &mut labels)),
            b @ Out::Bytes(_) => b,
        };
        outs.push((*at, out));
    }
    labels.sort();
    labels.dedup();
    let mut s = String::new();
    let mut marks = Vec::with_capacity(outs.len() + 1);
    let mut k = 0;
    let put_labels = |s: &mut String, upto: u32, k: &mut usize| {
        while let Some((at, name)) = labels.get(*k).filter(|x| x.0 <= upto) {
            let _ = at;
            let _ = writeln!(s, "{name}:");
            *k += 1;
        }
    };
    // RISC-V: 16-bit forms (their `c.` text) under `.option rvc`, the
    // rest under the header's `norvc`, which no assembler compresses.
    let mut rvc = false;
    for (i, (at, out)) in outs.iter().enumerate() {
        put_labels(&mut s, *at, &mut k);
        if let Some((_, p)) = pcrel.iter().find(|p| p.0 == *at) {
            let _ = writeln!(s, "{p}:");
        }
        marks.push((*at, s.len()));
        match out {
            Out::Text(t) => {
                if isa == Isa::Riscv {
                    let end = outs.get(i + 1).map_or(len, |x| x.0);
                    let small = end - at == 2;
                    if small != rvc {
                        rvc = small;
                        let o = if small { "rvc" } else { "norvc" };
                        let _ = writeln!(s, "\t.option\t{o}");
                    }
                }
                let _ = writeln!(s, "\t{t}");
            }
            Out::Bytes(t) => {
                let end = outs.get(i + 1).map_or(len, |x| x.0);
                let b: Vec<String> = l.code[*at as usize..end as usize]
                    .iter()
                    .map(|b| format!("{b:#04x}"))
                    .collect();
                let _ = writeln!(
                    s,
                    "\t.byte\t{}\t{} {}",
                    b.join(", "),
                    isa.comment(),
                    t.replace('\t', " ")
                );
            }
        }
    }
    if rvc {
        s.push_str("\t.option\tnorvc\n");
    }
    put_labels(&mut s, len, &mut k);
    marks.push((len, s.len()));
    obj.src.get_or_insert_with(Vec::new).push(FnSrc {
        name: l.name.to_owned(),
        len,
        text: s,
        marks,
    });
}

/// Branch targets in `t` (`0x1c <...>`, as `Listing::target` writes
/// them) as labels, made where missing; other `<...>` notes dropped.
fn targets(obj: &Obj, isa: Isa, t: &str, f: usize, labels: &mut Vec<(u32, String)>) -> String {
    let _ = isa;
    let mut out = String::new();
    let mut rest = t;
    while let Some(i) = rest.find(" <") {
        let Some(j) = rest[i..].find('>') else {
            break;
        };
        // The hex number before the note, if any.
        let head = &rest[..i];
        let num_at = head
            .rfind(|c: char| !c.is_ascii_hexdigit() && c != 'x')
            .map_or(0, |p| p + 1);
        let num = &head[num_at..];
        match num
            .strip_prefix("0x")
            .and_then(|h| u32::from_str_radix(h, 16).ok())
        {
            Some(at) => {
                out.push_str(&head[..num_at]);
                let name = if let Some((_, n)) = labels.iter().find(|x| x.0 == at) {
                    n.clone()
                } else {
                    let n = local(obj, &format!("BB{f}_x{at:x}"));
                    labels.push((at, n.clone()));
                    n
                };
                out.push_str(&name);
            }
            None => out.push_str(head),
        }
        rest = &rest[i + j + 1..];
    }
    out.push_str(rest);
    out
}

/// An instruction without relocations; x86 forms an assembler would
/// encode otherwise are forced or left as bytes.
fn plain(isa: Isa, text: &str, code: &[u8]) -> Out {
    // s390x's trap: two zero bytes, no instruction.
    if text == "<unknown>" {
        return Out::Bytes("trap".to_owned());
    }
    if !matches!(isa, Isa::X64 | Isa::I386) {
        return Out::Text(text.to_owned());
    }
    match x86_form(isa == Isa::X64, code) {
        Form::Same => Out::Text(text.to_owned()),
        Form::Rex
            if ["spl", "bpl", "sil", "dil"]
                .iter()
                .any(|r| text.contains(r)) =>
        {
            Out::Text(text.to_owned())
        }
        Form::Rex => Out::Text(format!("{{rex}} {text}")),
        Form::Disp32 => Out::Text(format!("{{disp32}} {text}")),
        Form::Disp8 => Out::Text(format!("{{disp8}} {text}")),
        Form::Swap => match text.split_once('\t') {
            Some((mn, ops)) => Out::Text(format!("{mn}.s\t{ops}")),
            None => Out::Bytes(text.to_owned()),
        },
        Form::Bytes | Form::AbsMem => Out::Bytes(text.to_owned()),
    }
}

/// How an assembler would encode x86 instruction `code` from its text.
#[derive(PartialEq, Eq, Debug)]
enum Form {
    Same,
    /// A rel32 branch an assembler would shorten.
    Disp32,
    /// A `[base]` operand with a zero disp8.
    Disp8,
    /// A register-register `mov` an assembler encodes the other way.
    Swap,
    /// An empty REX prefix: needed for `spl`..`dil` only.
    Rex,
    /// No text gives these bytes.
    Bytes,
    /// i386: an absolute address in a longer form than an assembler's.
    AbsMem,
}

fn x86_form(x64: bool, code: &[u8]) -> Form {
    let mut i = 0;
    let mut opsize = false;
    while let Some(&b) = code.get(i) {
        match b {
            0x66 => opsize = true,
            0xf0 | 0xf2 | 0xf3 | 0x2e | 0x3e | 0x26 | 0x36 | 0x64 | 0x65 => {}
            _ => break,
        }
        i += 1;
    }
    let mut w = false;
    let mut rex = false;
    if x64 && code.get(i).is_some_and(|b| b & 0xf0 == 0x40) {
        w = code[i] & 8 != 0;
        rex = code[i] == 0x40;
        i += 1;
    }
    let Some(&op) = code.get(i) else {
        return Form::Same;
    };
    // VEX: a `[base]` operand always has a displacement.
    if op == 0xc4 || op == 0xc5 {
        let m = i + if op == 0xc4 { 4 } else { 3 };
        return match code.get(m) {
            Some(&modrm) if modrm >> 6 == 1 && modrm & 7 != 4 && code.get(m + 1) == Some(&0) => {
                if modrm & 7 == 5 {
                    Form::Same
                } else {
                    Form::Disp8
                }
            }
            Some(&modrm) if modrm >> 6 == 1 && modrm & 7 == 4 => {
                let base = code.get(m + 1).map_or(0, |s| s & 7);
                if base != 5 && code.get(m + 2) == Some(&0) {
                    Form::Disp8
                } else {
                    Form::Same
                }
            }
            _ => Form::Same,
        };
    }
    let modrm = code.get(i + 1).copied().unwrap_or(0);
    let reg_reg = modrm >> 6 == 3;
    // i386: `eax` from / to an absolute address has a short form; an
    // absolute address needs no SIB.
    if !x64 && matches!(op, 0x88..=0x8b) && modrm == 0x05 {
        return Form::AbsMem;
    }
    if !x64 && op != 0x0f && modrm & 0xc7 == 0x04 && code.get(i + 2) == Some(&0x25) {
        return Form::AbsMem;
    }
    let imm32 = |at: usize| {
        code.get(at..at + 4)
            .map(|b| i32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    };
    let small = |v: Option<i32>| v.is_some_and(|v| (-128..128).contains(&v));
    match op {
        0xe9 => Form::Disp32,
        0x0f if code.get(i + 1).is_some_and(|b| b & 0xf0 == 0x80) => Form::Disp32,
        // `op r, r/m` between registers: an assembler writes `op r/m, r`.
        0x8a | 0x8b if reg_reg => Form::Swap,
        0x02 | 0x03 | 0x0a | 0x0b | 0x12 | 0x13 | 0x1a | 0x1b | 0x22 | 0x23 | 0x2a | 0x2b
        | 0x32 | 0x33 | 0x3a | 0x3b
            if reg_reg =>
        {
            Form::Bytes
        }
        // `op r/m, imm32`: an imm8 form when it fits, the accumulator's
        // short form when not.
        0x81 if !opsize => {
            let v = imm32(i + 2 + modrm_len(code, i + 1));
            if small(v) || (reg_reg && modrm.trailing_zeros() >= 3) {
                Form::Bytes
            } else {
                Form::Same
            }
        }
        0x69 if !opsize => {
            if small(imm32(i + 2 + modrm_len(code, i + 1))) {
                Form::Bytes
            } else {
                Form::Same
            }
        }
        // `test al / eax, imm`, `mov r32, imm32`.
        0xf6 | 0xf7 if reg_reg && modrm.trailing_zeros() >= 6 => Form::Bytes,
        0xc7 if reg_reg && !w => Form::Bytes,
        // Shift by 1.
        0xc0 | 0xc1 if code.get(i + 2) == Some(&1) && reg_reg => Form::Bytes,
        _ if rex => Form::Rex,
        _ => Form::Same,
    }
}

/// The length of the `ModRM` at `at` with its SIB and displacement.
fn modrm_len(code: &[u8], at: usize) -> usize {
    let m = code.get(at).copied().unwrap_or(0);
    let (md, rm) = (m >> 6, m & 7);
    if md == 3 {
        return 0;
    }
    let (n, base) = if rm == 4 {
        (1, code.get(at + 1).map_or(0, |s| s & 7))
    } else {
        (0, rm)
    };
    n + match md {
        0 if rm == 5 || base == 5 => 4,
        1 => 1,
        2 => 4,
        _ => 0,
    }
}

/// The 32-bit little-endian field at `at` of `code`.
fn field32(code: &[u8], at: u32) -> i64 {
    let at = at as usize;
    code.get(at..at + 4).map_or(0, |b| {
        i64::from(i32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    })
}

/// The mnemonic and operands of `t`.
fn split(t: &str) -> (&str, &str) {
    t.split_once(['\t', ' ']).unwrap_or((t, ""))
}

/// `t` with its last operand `op`.
fn last(t: &str, op: &str) -> String {
    let (mn, ops) = split(t);
    let sep = &t[mn.len()..=mn.len()];
    match ops.rfind(", ") {
        Some(i) => format!("{mn}{sep}{}, {op}", &ops[..i]),
        None => format!("{mn}{sep}{op}"),
    }
}

/// `t`'s memory operand `[base...]` as `[base + e]`.
fn mem(t: &str, e: &str) -> Option<String> {
    let a = t.find('[')?;
    let b = a + t[a..].find(']')?;
    let inner = &t[a + 1..b];
    let base = inner.split(" + 0x").next()?.split(" - 0x").next()?;
    let base = if base.starts_with("0x") { "" } else { base };
    let addr = if base.is_empty() {
        e.to_owned()
    } else {
        format!("{base} + {e}")
    };
    Some(format!("{}[{addr}]{}", &t[..a], &t[b + 1..]))
}

/// `t` with the immediate (last operand) `e`, as Intel syntax writes an
/// address: `offset e`.
fn imm(t: &str, e: &str) -> String {
    last(t, &format!("offset {e}"))
}

/// The source of a relocated instruction (and whether it takes the
/// next line too), or `None` to leave it as bytes.
#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
fn reloc(
    obj: &Obj,
    isa: Isa,
    text: &str,
    code: &[u8],
    rs: &[(u32, RelKind, SymId)],
    next: Option<&str>,
    pcrel: &[(u32, String)],
    f: usize,
    at: u32,
) -> Option<(String, bool)> {
    let elf = obj.elf;
    let macho = !obj.elf && !obj.coff;
    let one = |t: String| Some((t, false));
    let &[(fld, kind, sym)] = rs else {
        return None;
    };
    let name = quote(&crate::asm::sym_name(obj, &obj.syms[sym as usize].name));
    let n = &name;
    let len = code.len() as u32;
    match isa {
        Isa::X64 | Isa::I386 => {
            // The addend past the field's end: what an assembler puts in
            // for an operand ahead of an immediate.
            let k = i64::from(len - fld) - 4;
            let i386 = isa == Isa::I386;
            // i386 keeps the addend in the place.
            let inplace = if i386 { field32(code, fld) } else { 0 };
            let pre = match x86_form(!i386, code) {
                Form::Disp8 => "{disp8} ",
                Form::Rex => "{rex} ",
                // The bytes, the field an absolute address.
                Form::AbsMem if matches!(kind, RelKind::I386Abs32 | RelKind::Abs64) => {
                    let f = fld as usize;
                    let b = |r: &[u8]| {
                        r.iter()
                            .map(|b| format!("{b:#04x}"))
                            .collect::<Vec<_>>()
                            .join(", ")
                    };
                    let mut t = format!(".byte\t{}\t# {}", b(&code[..f]), text.replace('\t', " "));
                    let _ = write!(t, "\n\t.long\t{n}{}", plus(inplace));
                    if f + 4 < code.len() {
                        let _ = write!(t, "\n\t.byte\t{}", b(&code[f + 4..]));
                    }
                    return one(t);
                }
                // A relocated immediate is an expression: an assembler
                // keeps its 32 bits.
                Form::AbsMem | Form::Swap => return None,
                _ => "",
            };
            let t = match kind {
                RelKind::X86Call | RelKind::I386Call => {
                    let (mn, _) = split(text);
                    if elf {
                        format!("{mn}\t{n}@PLT")
                    } else {
                        format!("{mn}\t{n}")
                    }
                }
                RelKind::X86Pc32 => mem(text, &format!("{n}{}", plus(k)))?,
                RelKind::X86GotLoad => mem(text, &format!("{n}@GOTPCREL{}", plus(k)))?,
                RelKind::X86Tls => mem(text, &format!("{n}@GOTTPOFF{}", plus(k)))?,
                RelKind::X86SecRel => mem(text, &format!("{n}@SECREL32{}", plus(inplace)))?,
                RelKind::I386GotOff => mem(text, &format!("{n}@GOTOFF{}", plus(inplace)))?,
                RelKind::I386Got32X => mem(text, &format!("{n}@GOT{}", plus(inplace)))?,
                RelKind::I386TlsGotIe => mem(text, &format!("{n}@GOTNTPOFF{}", plus(inplace)))?,
                RelKind::I386GotPc => {
                    // An assembler adds the field's offset in the
                    // instruction to the GOT's address.
                    imm(text, &format!("{n}{}", plus(inplace - i64::from(fld))))
                }
                RelKind::I386Abs32 | RelKind::Abs64 if text.contains('[') => {
                    mem(text, &format!("{n}{}", plus(inplace)))?
                }
                RelKind::I386Abs32 | RelKind::Abs64 => imm(text, &format!("{n}{}", plus(inplace))),
                _ => return None,
            };
            one(format!("{pre}{t}"))
        }
        Isa::A64 => {
            let coff = obj.coff;
            let ldst = text.contains('[');
            let lo = |m: &str, apple: &str| {
                if macho {
                    format!("{n}@{apple}")
                } else {
                    format!(":{m}:{n}")
                }
            };
            let t = match kind {
                RelKind::Page21 if macho => last(text, &format!("{n}@PAGE")),
                RelKind::Branch26 | RelKind::Page21 => last(text, n),
                RelKind::GotPage21 if macho => last(text, &format!("{n}@GOTPAGE")),
                RelKind::GotPage21 => last(text, &format!(":got:{n}")),
                RelKind::TlvPage21 if macho => last(text, &format!("{n}@TLVPPAGE")),
                RelKind::TlvPage21 => last(text, &format!(":gottprel:{n}")),
                RelKind::PageOff12 | RelKind::GotPageOff12 | RelKind::TlvPageOff12 => {
                    let e = match kind {
                        RelKind::PageOff12 => lo("lo12", "PAGEOFF"),
                        RelKind::GotPageOff12 => lo("got_lo12", "GOTPAGEOFF"),
                        _ => lo("gottprel_lo12", "TLVPPAGEOFF"),
                    };
                    if ldst {
                        a64_mem(text, &e)?
                    } else {
                        last(text, &e)
                    }
                }
                RelKind::SecRelHi12 if coff => {
                    let t = text.strip_suffix(", lsl #12")?;
                    last(t, &format!(":secrel_hi12:{n}"))
                }
                RelKind::SecRelLo12 if coff => last(text, &format!(":secrel_lo12:{n}")),
                _ => return None,
            };
            one(t)
        }
        Isa::Arm => {
            let w = u32::from_le_bytes(code.get(..4)?.try_into().ok()?);
            let t = match kind {
                RelKind::ArmCall => last(text, n),
                RelKind::ArmMovwPrel | RelKind::ArmMovtPrel => {
                    // The addend, in the place: imm4:imm12, sign extended.
                    let a = i64::from((((w >> 4) & 0xf000) | (w & 0xfff)) as u16 as i16);
                    let half = if kind == RelKind::ArmMovwPrel {
                        "lower16"
                    } else {
                        "upper16"
                    };
                    last(text, &format!("#:{half}:({n}-(.{}))", plus(-a)))
                }
                RelKind::ArmGotPrel => format!(".word\t{n}(GOT_PREL){}", plus(field32(code, 0))),
                RelKind::ArmTlsIe => format!(".word\t{n}(gottpoff){}", plus(field32(code, 0))),
                RelKind::Abs64 if text.starts_with(".word") => {
                    format!(".word\t{n}{}", plus(field32(code, 0)))
                }
                _ => return None,
            };
            one(t)
        }
        Isa::Riscv => {
            let (mn, ops) = split(text);
            let reg0 = ops.split(", ").next()?;
            match kind {
                RelKind::RvCall => {
                    // `auipc rX, 0` / `jalr rY, rX`: `call`, `tail`.
                    let next = next?;
                    let (jm, jops) = split(next);
                    if mn != "auipc" || jm != "jalr" {
                        return None;
                    }
                    let jops = jops.split(" <").next()?;
                    match (reg0, jops) {
                        ("ra", "ra") => Some((format!("call\t{n}"), true)),
                        ("t1", "t1") => Some((format!("tail\t{n}"), true)),
                        _ => None,
                    }
                }
                RelKind::RvPcHi | RelKind::RvGotHi | RelKind::RvTlsIeHi => {
                    let m = match kind {
                        RelKind::RvPcHi => "pcrel_hi",
                        RelKind::RvGotHi => "got_pcrel_hi",
                        _ => "tls_ie_pcrel_hi",
                    };
                    (mn == "auipc").then(|| (format!("auipc\t{reg0}, %{m}({n})"), false))
                }
                RelKind::RvTpHi => {
                    (mn == "lui").then(|| (format!("lui\t{reg0}, %tprel_hi({n})"), false))
                }
                RelKind::RvTpAdd => {
                    (mn == "add").then(|| (format!("{text}, %tprel_add({n})"), false))
                }
                RelKind::RvPcLoI | RelKind::RvTpLo => {
                    let e = if kind == RelKind::RvPcLoI {
                        let l = &pcrel.iter().find(|p| p.0 + 4 == at + fld)?.1;
                        format!("%pcrel_lo({l})")
                    } else {
                        format!("%tprel_lo({n})")
                    };
                    rv_lo(text, &e).map(|t| (t, false))
                }
                _ => None,
            }
        }
        Isa::Ppc64 => {
            let (mn, ops) = split(text);
            let t = match kind {
                RelKind::PpcCall => last(text, n),
                RelKind::PpcEntryHa | RelKind::PpcEntryLo => {
                    let gep = local(obj, &format!("gep{f}"));
                    let m = if kind == RelKind::PpcEntryHa {
                        "ha"
                    } else {
                        "l"
                    };
                    last(text, &format!(".TOC.-{gep}@{m}"))
                }
                RelKind::PpcTocHa => last(text, &format!("{n}@toc@ha")),
                RelKind::PpcGotHa => last(text, &format!("{n}@got@ha")),
                RelKind::PpcTlsIeHa => last(text, &format!("{n}@got@tprel@ha")),
                RelKind::PpcTocLo | RelKind::PpcGotLo | RelKind::PpcTlsIeLo => {
                    let e = match kind {
                        RelKind::PpcTocLo => format!("{n}@toc@l"),
                        RelKind::PpcGotLo => format!("{n}@got@l"),
                        _ => format!("{n}@got@tprel@l"),
                    };
                    // `ld rX, 0(rY)` or `addi rX, rY, 0`.
                    match ops.rsplit_once(", 0(") {
                        Some((a, b)) => format!("{mn} {a}, {e}({b}"),
                        None => last(text, &e),
                    }
                }
                RelKind::PpcTls => last(text, &format!("{n}@tls")),
                _ => return None,
            };
            one(t)
        }
        Isa::S390x => {
            let t = match kind {
                RelKind::S390Call => last(text, &format!("{n}@PLT")),
                RelKind::S390Pc => last(text, n),
                RelKind::S390Got => last(text, &format!("{n}@GOTENT")),
                RelKind::S390TlsIe => last(text, &format!("{n}@INDNTPOFF")),
                _ => return None,
            };
            one(t)
        }
    }
}

/// arm64 `ldr x0, [x1]` / `[x1, #0x8]` with the offset `e`.
fn a64_mem(t: &str, e: &str) -> Option<String> {
    let a = t.find('[')?;
    let b = a + t[a..].find(']')?;
    let base = t[a + 1..b].split(", ").next()?;
    Some(format!("{}[{base}, {e}]{}", &t[..a], &t[b + 1..]))
}

/// RISC-V I-type `t` (`addi`, its `mv` alias, a load) with the low
/// 12 bits `e`.
fn rv_lo(t: &str, e: &str) -> Option<String> {
    let (mn, ops) = split(t);
    if mn == "mv" {
        let (rd, rs) = ops.split_once(", ")?;
        return Some(format!("addi\t{rd}, {rs}, {e}"));
    }
    if let Some((a, b)) = ops.rsplit_once("0x0(") {
        return Some(format!("{mn}\t{a}{e}({b}"));
    }
    (mn == "addi").then(|| last(t, e))
}
