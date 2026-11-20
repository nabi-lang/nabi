//! s390x instruction encoder: one function per instruction format the
//! emitter uses, the only place that knows opcodes and fields. Code is
//! big endian, an instruction 2, 4 or 6 bytes (the top two bits of its
//! first byte say which). With `Asm::text` on (`--emit=asm`) each
//! function also records its instruction; `lines` decodes the final
//! bytes (branches patched) into what `llvm-objdump -d` prints: its
//! extended mnemonics (`j`, `jgne`, `br`), decimal immediates and
//! displacements, absolute branch targets. Off, nothing is recorded.

#[cfg(test)]
mod tests;

use crate::asm::Listing;
use crate::lir::SymId;
use crate::obj::RelKind;

/// RR: `op r1, r2`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Rr {
    Basr,
    Ldr,
}

impl Rr {
    const fn enc(self) -> (u8, &'static str, Rf, Rf) {
        match self {
            Self::Basr => (0x0d, "basr", Rf::G, Rf::G),
            Self::Ldr => (0x28, "ldr", Rf::F, Rf::F),
        }
    }
}

/// RRE: `op r1, r2` (16-bit opcode).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Rre {
    Ltgr,
    Lcgr,
    Lgr,
    Lgbr,
    Lghr,
    Agr,
    Sgr,
    Algr,
    Slgr,
    Msgr,
    Dsgr,
    Lrvgr,
    Lgfr,
    Llgfr,
    Lrvr,
    Cgr,
    Clgr,
    Ngr,
    Ogr,
    Xgr,
    Flogr,
    Llgcr,
    Llghr,
    Mlgr,
    Dlgr,
    /// `ear r1, a2`.
    Ear,
    Lcebr,
    Ldebr,
    Cebr,
    Aebr,
    Sebr,
    Debr,
    Lcdbr,
    Sqebr,
    Sqdbr,
    Meebr,
    Cdbr,
    Adbr,
    Sdbr,
    Mdbr,
    Ddbr,
    Ledbr,
    Cegbr,
    Cdgbr,
    Ldgr,
    Lgdr,
}

impl Rre {
    const fn enc(self) -> (u16, &'static str, Rf, Rf) {
        match self {
            Self::Ltgr => (0xb902, "ltgr", Rf::G, Rf::G),
            Self::Lcgr => (0xb903, "lcgr", Rf::G, Rf::G),
            Self::Lgr => (0xb904, "lgr", Rf::G, Rf::G),
            Self::Lgbr => (0xb906, "lgbr", Rf::G, Rf::G),
            Self::Lghr => (0xb907, "lghr", Rf::G, Rf::G),
            Self::Agr => (0xb908, "agr", Rf::G, Rf::G),
            Self::Sgr => (0xb909, "sgr", Rf::G, Rf::G),
            Self::Algr => (0xb90a, "algr", Rf::G, Rf::G),
            Self::Slgr => (0xb90b, "slgr", Rf::G, Rf::G),
            Self::Msgr => (0xb90c, "msgr", Rf::G, Rf::G),
            Self::Dsgr => (0xb90d, "dsgr", Rf::G, Rf::G),
            Self::Lrvgr => (0xb90f, "lrvgr", Rf::G, Rf::G),
            Self::Lgfr => (0xb914, "lgfr", Rf::G, Rf::G),
            Self::Llgfr => (0xb916, "llgfr", Rf::G, Rf::G),
            Self::Lrvr => (0xb91f, "lrvr", Rf::G, Rf::G),
            Self::Cgr => (0xb920, "cgr", Rf::G, Rf::G),
            Self::Clgr => (0xb921, "clgr", Rf::G, Rf::G),
            Self::Ngr => (0xb980, "ngr", Rf::G, Rf::G),
            Self::Ogr => (0xb981, "ogr", Rf::G, Rf::G),
            Self::Xgr => (0xb982, "xgr", Rf::G, Rf::G),
            Self::Flogr => (0xb983, "flogr", Rf::G, Rf::G),
            Self::Llgcr => (0xb984, "llgcr", Rf::G, Rf::G),
            Self::Llghr => (0xb985, "llghr", Rf::G, Rf::G),
            Self::Mlgr => (0xb986, "mlgr", Rf::G, Rf::G),
            Self::Dlgr => (0xb987, "dlgr", Rf::G, Rf::G),
            Self::Ear => (0xb24f, "ear", Rf::G, Rf::A),
            Self::Lcebr => (0xb303, "lcebr", Rf::F, Rf::F),
            Self::Ldebr => (0xb304, "ldebr", Rf::F, Rf::F),
            Self::Cebr => (0xb309, "cebr", Rf::F, Rf::F),
            Self::Aebr => (0xb30a, "aebr", Rf::F, Rf::F),
            Self::Sebr => (0xb30b, "sebr", Rf::F, Rf::F),
            Self::Debr => (0xb30d, "debr", Rf::F, Rf::F),
            Self::Lcdbr => (0xb313, "lcdbr", Rf::F, Rf::F),
            Self::Sqebr => (0xb314, "sqebr", Rf::F, Rf::F),
            Self::Sqdbr => (0xb315, "sqdbr", Rf::F, Rf::F),
            Self::Meebr => (0xb317, "meebr", Rf::F, Rf::F),
            Self::Cdbr => (0xb319, "cdbr", Rf::F, Rf::F),
            Self::Adbr => (0xb31a, "adbr", Rf::F, Rf::F),
            Self::Sdbr => (0xb31b, "sdbr", Rf::F, Rf::F),
            Self::Mdbr => (0xb31c, "mdbr", Rf::F, Rf::F),
            Self::Ddbr => (0xb31d, "ddbr", Rf::F, Rf::F),
            Self::Ledbr => (0xb344, "ledbr", Rf::F, Rf::F),
            Self::Cegbr => (0xb3a4, "cegbr", Rf::F, Rf::G),
            Self::Cdgbr => (0xb3a5, "cdgbr", Rf::F, Rf::G),
            Self::Ldgr => (0xb3c1, "ldgr", Rf::F, Rf::G),
            Self::Lgdr => (0xb3cd, "lgdr", Rf::G, Rf::F),
        }
    }
}

/// RRF: a float -> integer conversion `op r1, m3, f2`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Rrf {
    Cgebr,
    Cgdbr,
}

impl Rrf {
    const fn enc(self) -> (u16, &'static str) {
        match self {
            Self::Cgebr => (0xb3a8, "cgebr"),
            Self::Cgdbr => (0xb3a9, "cgdbr"),
        }
    }
}

/// RXY: `op r1, d2(x2, b2)`, a 20-bit signed displacement.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Rxy {
    Lg,
    Lgf,
    Lgh,
    Llgf,
    Stg,
    Sty,
    Sthy,
    Lay,
    Stcy,
    Lgb,
    Llgc,
    Llgh,
    Ley,
    Ldy,
    Stey,
    Stdy,
}

impl Rxy {
    const fn enc(self) -> (u16, &'static str, Rf) {
        match self {
            Self::Lg => (0xe304, "lg", Rf::G),
            Self::Lgf => (0xe314, "lgf", Rf::G),
            Self::Lgh => (0xe315, "lgh", Rf::G),
            Self::Llgf => (0xe316, "llgf", Rf::G),
            Self::Stg => (0xe324, "stg", Rf::G),
            Self::Sty => (0xe350, "sty", Rf::G),
            Self::Sthy => (0xe370, "sthy", Rf::G),
            Self::Lay => (0xe371, "lay", Rf::G),
            Self::Stcy => (0xe372, "stcy", Rf::G),
            Self::Lgb => (0xe377, "lgb", Rf::G),
            Self::Llgc => (0xe390, "llgc", Rf::G),
            Self::Llgh => (0xe391, "llgh", Rf::G),
            Self::Ley => (0xed64, "ley", Rf::F),
            Self::Ldy => (0xed65, "ldy", Rf::F),
            Self::Stey => (0xed66, "stey", Rf::F),
            Self::Stdy => (0xed67, "stdy", Rf::F),
        }
    }
}

/// RSY: `op r1, r3, d2(b2)` (a shift: `b2 + d2` its amount).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Rsy {
    Lmg,
    Srag,
    Srlg,
    Sllg,
    Rllg,
    Rll,
    Stmg,
    Csg,
}

impl Rsy {
    const fn enc(self) -> (u16, &'static str) {
        match self {
            Self::Lmg => (0xeb04, "lmg"),
            Self::Srag => (0xeb0a, "srag"),
            Self::Srlg => (0xeb0c, "srlg"),
            Self::Sllg => (0xeb0d, "sllg"),
            Self::Rllg => (0xeb1c, "rllg"),
            Self::Rll => (0xeb1d, "rll"),
            Self::Stmg => (0xeb24, "stmg"),
            Self::Csg => (0xeb30, "csg"),
        }
    }
}

/// RI: `op r1, i2`, a signed 16-bit immediate.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Ri {
    Lghi,
    Aghi,
}

/// RIL: `op r1, i2`, a 32-bit immediate.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Ril {
    Lgfi,
    Iilf,
    Llihf,
    Llilf,
    Agfi,
    Cgfi,
    Clgfi,
}

impl Ril {
    /// The 12-bit opcode, the name, whether the immediate is signed.
    const fn enc(self) -> (u16, &'static str, bool) {
        match self {
            Self::Lgfi => (0xc01, "lgfi", true),
            Self::Iilf => (0xc09, "iilf", false),
            Self::Llihf => (0xc0e, "llihf", false),
            Self::Llilf => (0xc0f, "llilf", false),
            Self::Agfi => (0xc28, "agfi", true),
            Self::Cgfi => (0xc2c, "cgfi", true),
            Self::Clgfi => (0xc2e, "clgfi", false),
        }
    }
}

/// RIL of a symbol: `op r1, sym`, the halfword distance relocated.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Pcrel {
    Larl,
    Lgrl,
    Brasl,
}

impl Pcrel {
    const fn enc(self) -> (u16, &'static str) {
        match self {
            Self::Larl => (0xc00, "larl"),
            Self::Lgrl => (0xc48, "lgrl"),
            Self::Brasl => (0xc05, "brasl"),
        }
    }
}

/// SS: `op d1(l, b1), d2(b2)`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Ss {
    Mvc,
    Xc,
}

/// A register file, as an operand prints.
#[derive(Clone, Copy, Debug)]
enum Rf {
    G,
    F,
    A,
}

/// How a recorded instruction's fields print.
#[derive(Clone, Copy, Debug)]
enum Fmt {
    /// RR / RRE `r1, r2` (the fields in the last byte).
    R2(Rf, Rf),
    /// RRF `r1, m3, f2`.
    Rrf,
    /// RRD `f1, f3, f2`.
    Rrd,
    /// RX / RXY `r1, d2(x2, b2)`.
    Rx(Rf),
    /// RS `r1, r3, d2(b2)`, 12-bit displacement.
    Rs,
    /// RSY, 20-bit displacement.
    Rsy,
    /// RI `r1, i2`.
    Ri,
    /// RIL `r1, i2`, signed or not.
    Ril(bool),
    /// RI / RIL `r1, target`.
    RiPc,
    RilPc,
    /// `brc` / `brcl`: the mask spelled in the mnemonic.
    Brc,
    Brcl,
    /// `bcr 15, r2`.
    Br,
    Ss,
    /// No instruction (`00 00`).
    Unknown,
    /// Vector forms (`E7`): `v1, d2(x2, b2)` and an element index or
    /// not (`vleg`).
    Vrx(bool),
    /// `v1`, `v1, v2`, `v1, v2, v3`, `v1, v2, v3, v4`.
    V1,
    V2,
    V3,
    V4,
    /// `v1, i2` (signed or a mask), `v1, v3, i2`.
    Vri(bool),
    Vri3,
    /// `v1, v3, d2` (a shift), `v1, r3, d2` (`vlvg`).
    Vrs,
    VrsR,
}

/// A recorded instruction.
#[derive(Clone, Copy, Debug)]
struct Line {
    at: u32,
    mn: &'static str,
    fmt: Fmt,
}

/// Vector operations (z13): the last opcode byte, the mnemonics by
/// element size (`b h f g`, or one for all) and the format.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Vop {
    Vl,
    Vst,
    Vlrep,
    Vleg,
    Vlef,
    Vsteg,
    Vstef,
    Vlvg,
    Vrep,
    Vrepi,
    Vgbm,
    Vlr,
    Vuph,
    Vuplh,
    Vpk,
    Va,
    Vs,
    Vml,
    Vmh,
    Vmlh,
    Vn,
    Vnc,
    Vo,
    Vno,
    Vx,
    Vceq,
    Vch,
    Vchl,
    Vmn,
    Vmnl,
    Vmx,
    Vmxl,
    Vesl,
    Vesra,
    Vesrl,
    Vsel,
    Vfma,
    Vfa,
    Vfs,
    Vfm,
    Vfd,
    Vfsq,
    Vfce,
    Vfch,
    Vfche,
}

/// `[byte, b, h, f, g]`: the operation byte and its mnemonics (floats:
/// `f` short, `g` long).
const fn vop(op: Vop) -> (u8, [&'static str; 4]) {
    const fn one(s: &'static str) -> [&'static str; 4] {
        [s, s, s, s]
    }
    match op {
        Vop::Vl => (0x06, one("vl")),
        Vop::Vst => (0x0e, one("vst")),
        Vop::Vlrep => (0x05, ["vlrepb", "vlreph", "vlrepf", "vlrepg"]),
        Vop::Vleg => (0x02, one("vleg")),
        Vop::Vlef => (0x03, one("vlef")),
        Vop::Vsteg => (0x0a, one("vsteg")),
        Vop::Vstef => (0x0b, one("vstef")),
        Vop::Vlvg => (0x22, ["vlvgb", "vlvgh", "vlvgf", "vlvgg"]),
        Vop::Vrep => (0x4d, ["vrepb", "vreph", "vrepf", "vrepg"]),
        Vop::Vrepi => (0x45, ["vrepib", "vrepih", "vrepif", "vrepig"]),
        Vop::Vgbm => (0x44, one("vgbm")),
        Vop::Vlr => (0x56, one("vlr")),
        Vop::Vuph => (0xd7, ["vuphb", "vuphh", "vuphf", "vuphf"]),
        Vop::Vuplh => (0xd5, ["vuplhb", "vuplhh", "vuplhf", "vuplhf"]),
        Vop::Vpk => (0x94, ["vpkh", "vpkh", "vpkf", "vpkg"]),
        Vop::Va => (0xf3, ["vab", "vah", "vaf", "vag"]),
        Vop::Vs => (0xf7, ["vsb", "vsh", "vsf", "vsg"]),
        Vop::Vml => (0xa2, ["vmlb", "vmlhw", "vmlf", "vmlf"]),
        Vop::Vmh => (0xa3, ["vmhb", "vmhh", "vmhf", "vmhf"]),
        Vop::Vmlh => (0xa1, ["vmlhb", "vmlhh", "vmlhf", "vmlhf"]),
        Vop::Vn => (0x68, one("vn")),
        Vop::Vnc => (0x69, one("vnc")),
        Vop::Vo => (0x6a, one("vo")),
        Vop::Vno => (0x6b, one("vno")),
        Vop::Vx => (0x6d, one("vx")),
        Vop::Vceq => (0xf8, ["vceqb", "vceqh", "vceqf", "vceqg"]),
        Vop::Vch => (0xfb, ["vchb", "vchh", "vchf", "vchg"]),
        Vop::Vchl => (0xf9, ["vchlb", "vchlh", "vchlf", "vchlg"]),
        Vop::Vmn => (0xfe, ["vmnb", "vmnh", "vmnf", "vmng"]),
        Vop::Vmnl => (0xfc, ["vmnlb", "vmnlh", "vmnlf", "vmnlg"]),
        Vop::Vmx => (0xff, ["vmxb", "vmxh", "vmxf", "vmxg"]),
        Vop::Vmxl => (0xfd, ["vmxlb", "vmxlh", "vmxlf", "vmxlg"]),
        Vop::Vesl => (0x30, ["veslb", "veslh", "veslf", "veslg"]),
        Vop::Vesra => (0x3a, ["vesrab", "vesrah", "vesraf", "vesrag"]),
        Vop::Vesrl => (0x38, ["vesrlb", "vesrlh", "vesrlf", "vesrlg"]),
        Vop::Vsel => (0x8d, one("vsel")),
        Vop::Vfma => (0x8f, ["", "", "vfmasb", "vfmadb"]),
        Vop::Vfa => (0xe3, ["", "", "vfasb", "vfadb"]),
        Vop::Vfs => (0xe2, ["", "", "vfssb", "vfsdb"]),
        Vop::Vfm => (0xe7, ["", "", "vfmsb", "vfmdb"]),
        Vop::Vfd => (0xe5, ["", "", "vfdsb", "vfddb"]),
        Vop::Vfsq => (0xce, ["", "", "vfsqsb", "vfsqdb"]),
        Vop::Vfce => (0xe8, ["", "", "vfcesb", "vfcedb"]),
        Vop::Vfch => (0xeb, ["", "", "vfchsb", "vfchdb"]),
        Vop::Vfche => (0xea, ["", "", "vfchesb", "vfchedb"]),
    }
}

/// The code of a function in the making.
#[derive(Default)]
pub struct Asm {
    pub code: Vec<u8>,
    /// `(offset, kind, symbol)` of each relocated field.
    pub relocs: Vec<(u32, RelKind, SymId)>,
    text: Option<Vec<Line>>,
}

impl Asm {
    /// `text`: record each instruction for `lines`.
    #[must_use]
    pub fn new(text: bool) -> Self {
        Self {
            text: text.then(Vec::new),
            ..Self::default()
        }
    }

    /// The offset of the next instruction.
    #[must_use]
    pub fn pos(&self) -> usize {
        self.code.len()
    }

    #[inline]
    fn rec(&mut self, at: usize, mn: &'static str, fmt: Fmt) {
        if let Some(t) = &mut self.text {
            t.push(Line {
                at: at as u32,
                mn,
                fmt,
            });
        }
    }

    fn put(&mut self, b: &[u8]) {
        self.code.extend_from_slice(b);
    }

    /// RR `op r1, r2`.
    pub fn rr(&mut self, op: Rr, r1: u8, r2: u8) {
        let (o, mn, f1, f2) = op.enc();
        let at = self.pos();
        self.put(&[o, (r1 << 4) | r2]);
        self.rec(at, mn, Fmt::R2(f1, f2));
    }

    /// `br r2` (`bcr 15, r2`).
    pub fn br(&mut self, r2: u8) {
        let at = self.pos();
        self.put(&[0x07, 0xf0 | r2]);
        self.rec(at, "br", Fmt::Br);
    }

    /// RRE `op r1, r2`.
    pub fn rre(&mut self, op: Rre, r1: u8, r2: u8) {
        let (o, mn, f1, f2) = op.enc();
        let [a, b] = o.to_be_bytes();
        let at = self.pos();
        self.put(&[a, b, 0, (r1 << 4) | r2]);
        self.rec(at, mn, Fmt::R2(f1, f2));
    }

    /// `madbr` / `maebr f1, f3, f2`: `f1 = f3 * f2 + f1`, rounded once.
    pub fn madbr(&mut self, d: bool, f1: u8, f3: u8, f2: u8) {
        let (o, mn) = if d {
            (0xb31e_u16, "madbr")
        } else {
            (0xb30e, "maebr")
        };
        let [a, b] = o.to_be_bytes();
        let at = self.pos();
        self.put(&[a, b, f1 << 4, (f3 << 4) | f2]);
        self.rec(at, mn, Fmt::Rrd);
    }

    /// RRF `op r1, m3, f2` (`m3`: the rounding mode).
    pub fn rrf(&mut self, op: Rrf, m3: u8, r1: u8, r2: u8) {
        let (o, mn) = op.enc();
        let [a, b] = o.to_be_bytes();
        let at = self.pos();
        self.put(&[a, b, m3 << 4, (r1 << 4) | r2]);
        self.rec(at, mn, Fmt::Rrf);
    }

    /// `la r1, d(x2, b2)`: a 12-bit unsigned displacement.
    pub fn la(&mut self, r1: u8, x2: u8, b2: u8, d: u32) {
        debug_assert!(d < 4096);
        let at = self.pos();
        self.put(&[0x41, (r1 << 4) | x2, (b2 << 4) | (d >> 8) as u8, d as u8]);
        self.rec(at, "la", Fmt::Rx(Rf::G));
    }

    /// `cs r1, r3, d(b2)`: a 12-bit unsigned displacement.
    pub fn cs(&mut self, r1: u8, r3: u8, b2: u8, d: u32) {
        debug_assert!(d < 4096);
        let at = self.pos();
        self.put(&[0xba, (r1 << 4) | r3, (b2 << 4) | (d >> 8) as u8, d as u8]);
        self.rec(at, "cs", Fmt::Rs);
    }

    fn rxy_raw(&mut self, op: u16, r1: u8, x2: u8, b2: u8, d: i32) {
        debug_assert!((-0x8_0000..0x8_0000).contains(&d));
        let [a, b] = op.to_be_bytes();
        let d = d as u32;
        self.put(&[
            a,
            (r1 << 4) | x2,
            (b2 << 4) | ((d >> 8) & 0xf) as u8,
            d as u8,
            (d >> 12) as u8,
            b,
        ]);
    }

    /// RXY `op r1, d(x2, b2)`.
    pub fn rxy(&mut self, op: Rxy, r1: u8, x2: u8, b2: u8, d: i32) {
        let (o, mn, f) = op.enc();
        let at = self.pos();
        self.rxy_raw(o, r1, x2, b2, d);
        self.rec(at, mn, Fmt::Rx(f));
    }

    /// RSY `op r1, r3, d(b2)`.
    pub fn rsy(&mut self, op: Rsy, r1: u8, r3: u8, b2: u8, d: i32) {
        let (o, mn) = op.enc();
        let at = self.pos();
        self.rxy_raw(o, r1, r3, b2, d);
        self.rec(at, mn, Fmt::Rsy);
    }

    fn ri_raw(&mut self, op: u16, r1: u8, i: u16) {
        let [h, l] = i.to_be_bytes();
        self.put(&[(op >> 4) as u8, (r1 << 4) | (op & 0xf) as u8, h, l]);
    }

    /// RI `op r1, i`.
    pub fn ri(&mut self, op: Ri, r1: u8, i: u16) {
        let (o, mn) = match op {
            Ri::Lghi => (0xa79, "lghi"),
            Ri::Aghi => (0xa7b, "aghi"),
        };
        let at = self.pos();
        self.ri_raw(o, r1, i);
        self.rec(at, mn, Fmt::Ri);
    }

    fn ril_raw(&mut self, op: u16, r1: u8, i: u32) {
        self.put(&[(op >> 4) as u8, (r1 << 4) | (op & 0xf) as u8]);
        self.put(&i.to_be_bytes());
    }

    /// RIL `op r1, i`.
    pub fn ril(&mut self, op: Ril, r1: u8, i: u32) {
        let (o, mn, signed) = op.enc();
        let at = self.pos();
        self.ril_raw(o, r1, i);
        self.rec(at, mn, Fmt::Ril(signed));
    }

    /// RIL `op r1, sym`: the field relocated by `kind`.
    pub fn pcrel(&mut self, op: Pcrel, r1: u8, kind: RelKind, sym: SymId) {
        let (o, mn) = op.enc();
        let at = self.pos();
        self.relocs.push((at as u32 + 2, kind, sym));
        self.ril_raw(o, r1, 0);
        self.rec(at, mn, Fmt::RilPc);
    }

    /// SS `op d1(len, b1), d2(b2)`: `len` bytes, 1..=256.
    pub fn ss(&mut self, op: Ss, len: u32, b1: u8, d1: u32, b2: u8, d2: u32) {
        debug_assert!((1..=256).contains(&len) && d1 < 4096 && d2 < 4096);
        let (o, mn) = match op {
            Ss::Mvc => (0xd2, "mvc"),
            Ss::Xc => (0xd7, "xc"),
        };
        let at = self.pos();
        self.put(&[
            o,
            (len - 1) as u8,
            (b1 << 4) | (d1 >> 8) as u8,
            d1 as u8,
            (b2 << 4) | (d2 >> 8) as u8,
            d2 as u8,
        ]);
        self.rec(at, mn, Fmt::Ss);
    }

    /// Two zero bytes: an illegal instruction.
    pub fn trap(&mut self) {
        let at = self.pos();
        self.put(&[0, 0]);
        self.rec(at, "<unknown>", Fmt::Unknown);
    }

    // Branches: halfword distances from the instruction.

    /// `brcl mask` to a spot patched by `patch_brcl`: its offset.
    pub fn brcl(&mut self, mask: u8) -> usize {
        let at = self.pos();
        self.ril_raw(0xc04, mask, 0);
        self.rec(at, "", Fmt::Brcl);
        at
    }

    /// Point the `brcl` at `at` to `target`.
    pub fn patch_brcl(&mut self, at: usize, target: usize) {
        let d = (target as i64 - at as i64) / 2;
        self.code[at + 2..at + 6].copy_from_slice(&(d as i32).to_be_bytes());
    }

    /// Point the `brc` at `at` to `target`; whether it reaches.
    pub fn patch_brc(&mut self, at: usize, target: usize) -> bool {
        let d = (target as i64 - at as i64) / 2;
        let Ok(d) = i16::try_from(d) else {
            return false;
        };
        self.code[at + 2..at + 4].copy_from_slice(&d.to_be_bytes());
        true
    }

    /// A short forward `brc mask` to a spot not known yet: its offset.
    pub fn brc_fwd(&mut self, mask: u8) -> usize {
        let at = self.pos();
        self.ri_raw(0xa74, mask, 0);
        self.rec(at, "", Fmt::Brc);
        at
    }

    /// Point the short branch at `at` here.
    pub fn here(&mut self, at: usize) {
        let d = (self.pos() - at) / 2;
        assert!(d < 0x8000, "s390x: a short branch past 64 KiB");
        self.code[at + 2..at + 4].copy_from_slice(&(d as u16).to_be_bytes());
    }

    /// `brc mask` back to offset `top`.
    pub fn brc_back(&mut self, mask: u8, top: usize) {
        let at = self.pos();
        let d = (top as i64 - at as i64) / 2;
        self.ri_raw(0xa74, mask, d as i16 as u16);
        self.rec(at, "", Fmt::Brc);
    }

    /// A vector instruction: `f1 f2 f3` the register (or 4-bit) fields
    /// of bits 8..20, `mid` bits 20..32, `f4` bits 32..36; `v` which of
    /// the four are vector registers (their fifth bits into RXB).
    #[allow(clippy::too_many_arguments)]
    fn vraw(&mut self, op: Vop, size: usize, f: [u8; 4], mid: u16, v: [bool; 4], fmt: Fmt) {
        let (o, mut mn) = vop(op);
        if op == Vop::Vgbm && matches!(fmt, Fmt::V1) {
            mn = if mid == 0 { ["vzero"; 4] } else { ["vone"; 4] };
        }
        // A compare that sets the condition code (`cs`).
        if mid & 0x10 != 0 {
            match op {
                Vop::Vceq => mn = ["vceqbs", "vceqhs", "vceqfs", "vceqgs"],
                Vop::Vch => mn = ["vchbs", "vchhs", "vchfs", "vchgs"],
                Vop::Vchl => mn = ["vchlbs", "vchlhs", "vchlfs", "vchlgs"],
                _ => {}
            }
        }
        let mut rxb = 0;
        for (i, (r, is_v)) in f.iter().zip(v).enumerate() {
            if is_v && *r >= 16 {
                rxb |= 8 >> i;
            }
        }
        let at = self.pos();
        self.put(&[
            0xe7,
            ((f[0] & 15) << 4) | (f[1] & 15),
            ((f[2] & 15) << 4) | (mid >> 8) as u8,
            mid as u8,
            ((f[3] & 15) << 4) | rxb,
            o,
        ]);
        self.rec(at, mn[size], fmt);
    }

    /// VRX `op v1, d2(x2, b2), m3`: a 12-bit unsigned displacement.
    pub fn vrx(&mut self, op: Vop, v1: u8, x2: u8, b2: u8, d2: u32, m3: u8) {
        debug_assert!(d2 < 4096);
        let elem = matches!(op, Vop::Vleg | Vop::Vlef | Vop::Vsteg | Vop::Vstef);
        let size = if op == Vop::Vlrep { m3 as usize } else { 0 };
        self.vraw(
            op,
            size,
            [v1, x2, b2, m3],
            d2 as u16,
            [true, false, false, false],
            Fmt::Vrx(elem),
        );
    }

    /// VRR `op v1, v2, v3` (`m4`: the element size, or a float's format;
    /// `m5`: `cs` of a compare). `vuph` and `vfsq` take `v2` alone, `vlr`
    /// and `vno` of one register two.
    pub fn vrr(&mut self, op: Vop, v1: u8, v2: u8, v3: u8, m4: u8, m5: u8) {
        let size = m4 as usize & 3;
        match op {
            // VRR-a: m3 the size, in the fourth field.
            Vop::Vuph | Vop::Vuplh | Vop::Vfsq | Vop::Vlr => {
                let m3 = if op == Vop::Vlr { 0 } else { m4 };
                self.vraw(
                    op,
                    size,
                    [v1, v2, 0, m3],
                    0,
                    [true, true, false, false],
                    Fmt::V2,
                );
            }
            // VRR-b and VRR-c: a compare's cs at bits 24..28.
            _ => {
                let mid = u16::from(m5) << 4;
                self.vraw(
                    op,
                    size,
                    [v1, v2, v3, m4],
                    mid,
                    [true, true, true, false],
                    Fmt::V3,
                );
            }
        }
    }

    /// VRR-e `vsel v1, v2, v3, v4`: `v2` where `v4`'s bits are set, else
    /// `v3`.
    pub fn vsel(&mut self, v1: u8, v2: u8, v3: u8, v4: u8) {
        self.vraw(Vop::Vsel, 0, [v1, v2, v3, v4], 0, [true; 4], Fmt::V4);
    }

    /// VRR-e `vfma v1, v2, v3, v4`: `v2 * v3 + v4` per lane, rounded
    /// once (`m6`: the float format; `m5`, single element, 0).
    pub fn vfma(&mut self, v1: u8, v2: u8, v3: u8, v4: u8, m6: u8) {
        let size = m6 as usize & 3;
        let mid = u16::from(m6) << 8;
        self.vraw(Vop::Vfma, size, [v1, v2, v3, v4], mid, [true; 4], Fmt::V4);
    }

    /// VRI-a `op v1, i2, m3` (`vrepi`, `vgbm`).
    pub fn vri(&mut self, op: Vop, v1: u8, i2: u16, m3: u8) {
        let fmt = if op == Vop::Vgbm {
            if i2 == 0 || i2 == 0xffff {
                Fmt::V1
            } else {
                Fmt::Vri(false)
            }
        } else {
            Fmt::Vri(true)
        };
        let i = i2;
        self.vraw(
            op,
            m3 as usize & 3,
            [v1, 0, (i >> 12) as u8, m3],
            i & 0xfff,
            [true, false, false, false],
            fmt,
        );
    }

    /// VRI-c `vrep v1, v3, i2, m4`: element `i2` of `v3` in every one.
    pub fn vrep(&mut self, v1: u8, v3: u8, i2: u16, m4: u8) {
        self.vraw(
            Vop::Vrep,
            m4 as usize & 3,
            [v1, v3, (i2 >> 12) as u8, m4],
            i2 & 0xfff,
            [true, true, false, false],
            Fmt::Vri3,
        );
    }

    /// VRS `op v1, v3, d2(b2), m4`: a shift by `d2` (`vlvg`: `r3` into
    /// element `d2`).
    pub fn vrs(&mut self, op: Vop, v1: u8, v3: u8, d2: u16, m4: u8) {
        let gr = op == Vop::Vlvg;
        self.vraw(
            op,
            m4 as usize & 3,
            [v1, v3, 0, m4],
            d2 & 0xfff,
            [true, !gr, false, false],
            if gr { Fmt::VrsR } else { Fmt::Vrs },
        );
    }

    /// `brctg r1` back to offset `top`.
    pub fn brctg(&mut self, r1: u8, top: usize) {
        let at = self.pos();
        let d = (top as i64 - at as i64) / 2;
        self.ri_raw(0xa77, r1, d as i16 as u16);
        self.rec(at, "brctg", Fmt::RiPc);
    }

    /// Each recorded instruction's offset and text.
    #[must_use]
    pub fn lines(&self, l: &Listing<'_>) -> Vec<(u32, String)> {
        let Some(t) = &self.text else {
            return Vec::new();
        };
        t.iter()
            .map(|line| (line.at, text(&self.code[line.at as usize..], line, l)))
            .collect()
    }
}

const fn len(first: u8) -> usize {
    match first >> 6 {
        0 => 2,
        3 => 6,
        _ => 4,
    }
}

fn reg(f: Rf, r: u8) -> String {
    match f {
        Rf::G => format!("%r{r}"),
        Rf::F => format!("%f{r}"),
        Rf::A => format!("%a{r}"),
    }
}

/// `d(x,b)`: the index and base left out when 0.
fn bdx(d: i64, x: u8, b: u8) -> String {
    match (x, b) {
        (0, 0) => format!("{d}"),
        (0, b) => format!("{d}(%r{b})"),
        (x, 0) => format!("{d}(%r{x},0)"),
        (x, b) => format!("{d}(%r{x},%r{b})"),
    }
}

/// The 20-bit signed displacement of an RXY / RSY.
fn d20(c: &[u8]) -> i64 {
    let d = (u32::from(c[4]) << 12) | (u32::from(c[2] & 0xf) << 8) | u32::from(c[3]);
    i64::from(((d << 12) as i32) >> 12)
}

fn d12(c: &[u8]) -> i64 {
    i64::from((u32::from(c[2] & 0xf) << 8) | u32::from(c[3]))
}

/// The mnemonic suffix of branch mask `m`.
const fn cond(m: u8) -> &'static str {
    const N: [&str; 16] = [
        "nop", "o", "h", "nle", "l", "nhe", "lh", "ne", "e", "nlh", "he", "nl", "le", "nh", "no",
        "",
    ];
    N[(m & 0xf) as usize]
}

fn text(c: &[u8], line: &Line, l: &Listing<'_>) -> String {
    let c = &c[..len(c[0]).min(c.len())];
    let (r1, r2) = (c[1] >> 4, c[1] & 0xf);
    let target = |d: i64| l.target(i64::from(line.at) + 2 * d);
    let ops = match line.fmt {
        Fmt::Unknown => return line.mn.to_owned(),
        Fmt::R2(f1, f2) => {
            let b = c[c.len() - 1];
            format!("{}, {}", reg(f1, b >> 4), reg(f2, b & 0xf))
        }
        Fmt::Rrf => format!("%r{}, {}, %f{}", c[3] >> 4, c[2] >> 4, c[3] & 0xf),
        Fmt::Rrd => format!("%f{}, %f{}, %f{}", c[2] >> 4, c[3] >> 4, c[3] & 0xf),
        Fmt::Rx(f) => {
            let d = if c.len() == 6 { d20(c) } else { d12(c) };
            format!("{}, {}", reg(f, r1), bdx(d, r2, c[2] >> 4))
        }
        Fmt::Rs | Fmt::Rsy => {
            let d = if c.len() == 6 { d20(c) } else { d12(c) };
            format!("%r{r1}, %r{r2}, {}", bdx(d, 0, c[2] >> 4))
        }
        Fmt::Ri => format!("%r{r1}, {}", i16::from_be_bytes([c[2], c[3]])),
        Fmt::Ril(signed) => {
            let i = u32::from_be_bytes([c[2], c[3], c[4], c[5]]);
            if signed {
                format!("%r{r1}, {}", i as i32)
            } else {
                format!("%r{r1}, {i}")
            }
        }
        Fmt::RiPc => {
            let d = i16::from_be_bytes([c[2], c[3]]);
            format!("%r{r1}, {}", target(i64::from(d)))
        }
        Fmt::RilPc => {
            let d = i32::from_be_bytes([c[2], c[3], c[4], c[5]]);
            format!("%r{r1}, {}", target(i64::from(d)))
        }
        Fmt::Brc => {
            let d = i16::from_be_bytes([c[2], c[3]]);
            return format!("j{}\t{}", cond(r1), target(i64::from(d)));
        }
        Fmt::Brcl => {
            let d = i32::from_be_bytes([c[2], c[3], c[4], c[5]]);
            return format!("jg{}\t{}", cond(r1), target(i64::from(d)));
        }
        Fmt::Br if r2 == 0 => "0".to_owned(),
        Fmt::Br => format!("%r{r2}"),
        Fmt::Ss => format!(
            "{}({},%r{}), {}",
            d12(c),
            u32::from(c[1]) + 1,
            c[2] >> 4,
            bdx(d12(&c[2..]), 0, c[4] >> 4)
        ),
        Fmt::Vrx(_)
        | Fmt::V1
        | Fmt::V2
        | Fmt::V3
        | Fmt::V4
        | Fmt::Vri(_)
        | Fmt::Vri3
        | Fmt::Vrs
        | Fmt::VrsR => vtext(c, line.fmt),
    };
    format!("{}\t{ops}", line.mn)
}

/// The operands of a vector instruction `c`.
fn vtext(c: &[u8], fmt: Fmt) -> String {
    let rxb = c[4] & 15;
    let v = |field: u8, i: u32| u32::from(field) | (u32::from((rxb >> (3 - i)) & 1) << 4);
    let (v1, v2, v3, v4) = (
        v(c[1] >> 4, 0),
        v(c[1] & 15, 1),
        v(c[2] >> 4, 2),
        v(c[4] >> 4, 3),
    );
    let i16v = u16::from_be_bytes([c[2], c[3]]);
    match fmt {
        Fmt::Vrx(elem) => {
            let at = bdx(d12(c), c[1] & 15, c[2] >> 4);
            if elem {
                format!("%v{v1}, {at}, {}", c[4] >> 4)
            } else {
                format!("%v{v1}, {at}")
            }
        }
        Fmt::V1 => format!("%v{v1}"),
        Fmt::V2 => format!("%v{v1}, %v{v2}"),
        Fmt::V3 => format!("%v{v1}, %v{v2}, %v{v3}"),
        Fmt::V4 => format!("%v{v1}, %v{v2}, %v{v3}, %v{v4}"),
        Fmt::Vri(true) => format!("%v{v1}, {}", i16v as i16),
        Fmt::Vri(false) => format!("%v{v1}, {i16v}"),
        Fmt::Vri3 => format!("%v{v1}, %v{v2}, {i16v}"),
        Fmt::Vrs => format!("%v{v1}, %v{v2}, {}", d12(c)),
        _ => format!("%v{v1}, %r{}, {}", c[1] & 15, d12(c)),
    }
}
