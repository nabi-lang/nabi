//! Each form against the reference: the bytes LLVM's assembler writes for
//! it, the text `llvm-objdump -d` prints for them (`<..>` annotations
//! aside) and the relocated field.

use crate::asm::Listing;
use crate::isa::s390x::enc::*;

type Case = (
    fn(&mut Asm),
    &'static [u8],
    &'static str,
    Option<(u32, RelKind)>,
);

#[rustfmt::skip]
fn cases() -> Vec<Case> {
    vec![
        (|a| a.rr(Rr::Basr, 14, 1), &[0x0d, 0xe1], "basr\t%r14, %r1", None),
        (|a| a.rr(Rr::Ldr, 1, 3), &[0x28, 0x13], "ldr\t%f1, %f3", None),
        (|a| a.br(14), &[0x07, 0xfe], "br\t%r14", None),
        (|a| a.br(0), &[0x07, 0xf0], "br\t0", None),
        (|a| a.rre(Rre::Ltgr, 12, 7), &[0xb9, 0x02, 0x00, 0xc7], "ltgr\t%r12, %r7", None),
        (|a| a.rre(Rre::Lcgr, 12, 7), &[0xb9, 0x03, 0x00, 0xc7], "lcgr\t%r12, %r7", None),
        (|a| a.rre(Rre::Lgr, 12, 7), &[0xb9, 0x04, 0x00, 0xc7], "lgr\t%r12, %r7", None),
        (|a| a.rre(Rre::Lgbr, 12, 7), &[0xb9, 0x06, 0x00, 0xc7], "lgbr\t%r12, %r7", None),
        (|a| a.rre(Rre::Lghr, 12, 7), &[0xb9, 0x07, 0x00, 0xc7], "lghr\t%r12, %r7", None),
        (|a| a.rre(Rre::Agr, 12, 7), &[0xb9, 0x08, 0x00, 0xc7], "agr\t%r12, %r7", None),
        (|a| a.rre(Rre::Sgr, 12, 7), &[0xb9, 0x09, 0x00, 0xc7], "sgr\t%r12, %r7", None),
        (|a| a.rre(Rre::Algr, 12, 7), &[0xb9, 0x0a, 0x00, 0xc7], "algr\t%r12, %r7", None),
        (|a| a.rre(Rre::Slgr, 12, 7), &[0xb9, 0x0b, 0x00, 0xc7], "slgr\t%r12, %r7", None),
        (|a| a.rre(Rre::Msgr, 12, 7), &[0xb9, 0x0c, 0x00, 0xc7], "msgr\t%r12, %r7", None),
        (|a| a.rre(Rre::Dsgr, 12, 7), &[0xb9, 0x0d, 0x00, 0xc7], "dsgr\t%r12, %r7", None),
        (|a| a.rre(Rre::Lrvgr, 12, 7), &[0xb9, 0x0f, 0x00, 0xc7], "lrvgr\t%r12, %r7", None),
        (|a| a.rre(Rre::Lgfr, 12, 7), &[0xb9, 0x14, 0x00, 0xc7], "lgfr\t%r12, %r7", None),
        (|a| a.rre(Rre::Llgfr, 12, 7), &[0xb9, 0x16, 0x00, 0xc7], "llgfr\t%r12, %r7", None),
        (|a| a.rre(Rre::Lrvr, 12, 7), &[0xb9, 0x1f, 0x00, 0xc7], "lrvr\t%r12, %r7", None),
        (|a| a.rre(Rre::Cgr, 12, 7), &[0xb9, 0x20, 0x00, 0xc7], "cgr\t%r12, %r7", None),
        (|a| a.rre(Rre::Clgr, 12, 7), &[0xb9, 0x21, 0x00, 0xc7], "clgr\t%r12, %r7", None),
        (|a| a.rre(Rre::Ngr, 12, 7), &[0xb9, 0x80, 0x00, 0xc7], "ngr\t%r12, %r7", None),
        (|a| a.rre(Rre::Ogr, 12, 7), &[0xb9, 0x81, 0x00, 0xc7], "ogr\t%r12, %r7", None),
        (|a| a.rre(Rre::Xgr, 12, 7), &[0xb9, 0x82, 0x00, 0xc7], "xgr\t%r12, %r7", None),
        (|a| a.rre(Rre::Flogr, 12, 7), &[0xb9, 0x83, 0x00, 0xc7], "flogr\t%r12, %r7", None),
        (|a| a.rre(Rre::Llgcr, 12, 7), &[0xb9, 0x84, 0x00, 0xc7], "llgcr\t%r12, %r7", None),
        (|a| a.rre(Rre::Llghr, 12, 7), &[0xb9, 0x85, 0x00, 0xc7], "llghr\t%r12, %r7", None),
        (|a| a.rre(Rre::Mlgr, 12, 7), &[0xb9, 0x86, 0x00, 0xc7], "mlgr\t%r12, %r7", None),
        (|a| a.rre(Rre::Dlgr, 12, 7), &[0xb9, 0x87, 0x00, 0xc7], "dlgr\t%r12, %r7", None),
        (|a| a.rre(Rre::Lcebr, 1, 3), &[0xb3, 0x03, 0x00, 0x13], "lcebr\t%f1, %f3", None),
        (|a| a.rre(Rre::Ldebr, 1, 3), &[0xb3, 0x04, 0x00, 0x13], "ldebr\t%f1, %f3", None),
        (|a| a.rre(Rre::Cebr, 1, 3), &[0xb3, 0x09, 0x00, 0x13], "cebr\t%f1, %f3", None),
        (|a| a.rre(Rre::Aebr, 1, 3), &[0xb3, 0x0a, 0x00, 0x13], "aebr\t%f1, %f3", None),
        (|a| a.rre(Rre::Sebr, 1, 3), &[0xb3, 0x0b, 0x00, 0x13], "sebr\t%f1, %f3", None),
        (|a| a.rre(Rre::Debr, 1, 3), &[0xb3, 0x0d, 0x00, 0x13], "debr\t%f1, %f3", None),
        (|a| a.rre(Rre::Lcdbr, 1, 3), &[0xb3, 0x13, 0x00, 0x13], "lcdbr\t%f1, %f3", None),
        (|a| a.rre(Rre::Sqebr, 1, 3), &[0xb3, 0x14, 0x00, 0x13], "sqebr\t%f1, %f3", None),
        (|a| a.rre(Rre::Sqdbr, 1, 3), &[0xb3, 0x15, 0x00, 0x13], "sqdbr\t%f1, %f3", None),
        (|a| a.rre(Rre::Meebr, 1, 3), &[0xb3, 0x17, 0x00, 0x13], "meebr\t%f1, %f3", None),
        (|a| a.rre(Rre::Cdbr, 1, 3), &[0xb3, 0x19, 0x00, 0x13], "cdbr\t%f1, %f3", None),
        (|a| a.rre(Rre::Adbr, 1, 3), &[0xb3, 0x1a, 0x00, 0x13], "adbr\t%f1, %f3", None),
        (|a| a.rre(Rre::Sdbr, 1, 3), &[0xb3, 0x1b, 0x00, 0x13], "sdbr\t%f1, %f3", None),
        (|a| a.rre(Rre::Mdbr, 1, 3), &[0xb3, 0x1c, 0x00, 0x13], "mdbr\t%f1, %f3", None),
        (|a| a.rre(Rre::Ddbr, 1, 3), &[0xb3, 0x1d, 0x00, 0x13], "ddbr\t%f1, %f3", None),
        (|a| a.rre(Rre::Ledbr, 1, 3), &[0xb3, 0x44, 0x00, 0x13], "ledbr\t%f1, %f3", None),
        (|a| a.rre(Rre::Cegbr, 1, 7), &[0xb3, 0xa4, 0x00, 0x17], "cegbr\t%f1, %r7", None),
        (|a| a.rre(Rre::Cdgbr, 1, 7), &[0xb3, 0xa5, 0x00, 0x17], "cdgbr\t%f1, %r7", None),
        (|a| a.rre(Rre::Ldgr, 1, 7), &[0xb3, 0xc1, 0x00, 0x17], "ldgr\t%f1, %r7", None),
        (|a| a.rre(Rre::Lgdr, 7, 1), &[0xb3, 0xcd, 0x00, 0x71], "lgdr\t%r7, %f1", None),
        (|a| a.rre(Rre::Lgr, 0, 15), &[0xb9, 0x04, 0x00, 0x0f], "lgr\t%r0, %r15", None),
        (|a| a.rre(Rre::Ear, 0, 1), &[0xb2, 0x4f, 0x00, 0x01], "ear\t%r0, %a1", None),
        (|a| a.rre(Rre::Ear, 0, 0), &[0xb2, 0x4f, 0x00, 0x00], "ear\t%r0, %a0", None),
        (|a| a.rrf(Rrf::Cgebr, 5, 7, 1), &[0xb3, 0xa8, 0x50, 0x71], "cgebr\t%r7, 5, %f1", None),
        (|a| a.rrf(Rrf::Cgdbr, 5, 7, 1), &[0xb3, 0xa9, 0x50, 0x71], "cgdbr\t%r7, 5, %f1", None),
        (|a| a.la(10, 0, 10, 256), &[0x41, 0xa0, 0xa1, 0x00], "la\t%r10, 256(%r10)", None),
        (|a| a.la(9, 0, 9, 4095), &[0x41, 0x90, 0x9f, 0xff], "la\t%r9, 4095(%r9)", None),
        (|a| a.la(7, 1, 9, 0), &[0x41, 0x71, 0x90, 0x00], "la\t%r7, 0(%r1,%r9)", None),
        (|a| a.cs(7, 5, 9, 0), &[0xba, 0x75, 0x90, 0x00], "cs\t%r7, %r5, 0(%r9)", None),
        (|a| a.cs(10, 5, 9, 4095), &[0xba, 0xa5, 0x9f, 0xff], "cs\t%r10, %r5, 4095(%r9)", None),
        (|a| a.rxy(Rxy::Lg, 7, 0, 15, 0), &[0xe3, 0x70, 0xf0, 0x00, 0x00, 0x04], "lg\t%r7, 0(%r15)", None),
        (|a| a.rxy(Rxy::Lgf, 7, 0, 15, 8), &[0xe3, 0x70, 0xf0, 0x08, 0x00, 0x14], "lgf\t%r7, 8(%r15)", None),
        (|a| a.rxy(Rxy::Lgh, 7, 0, 15, -8), &[0xe3, 0x70, 0xff, 0xf8, 0xff, 0x15], "lgh\t%r7, -8(%r15)", None),
        (|a| a.rxy(Rxy::Llgf, 7, 0, 15, 4095), &[0xe3, 0x70, 0xff, 0xff, 0x00, 0x16], "llgf\t%r7, 4095(%r15)", None),
        (|a| a.rxy(Rxy::Stg, 7, 0, 15, 4096), &[0xe3, 0x70, 0xf0, 0x00, 0x01, 0x24], "stg\t%r7, 4096(%r15)", None),
        (|a| a.rxy(Rxy::Sty, 7, 0, 15, -4096), &[0xe3, 0x70, 0xf0, 0x00, 0xff, 0x50], "sty\t%r7, -4096(%r15)", None),
        (|a| a.rxy(Rxy::Sthy, 7, 0, 15, 524287), &[0xe3, 0x70, 0xff, 0xff, 0x7f, 0x70], "sthy\t%r7, 524287(%r15)", None),
        (|a| a.rxy(Rxy::Lay, 7, 0, 15, -524288), &[0xe3, 0x70, 0xf0, 0x00, 0x80, 0x71], "lay\t%r7, -524288(%r15)", None),
        (|a| a.rxy(Rxy::Stcy, 7, 0, 15, 160), &[0xe3, 0x70, 0xf0, 0xa0, 0x00, 0x72], "stcy\t%r7, 160(%r15)", None),
        (|a| a.rxy(Rxy::Lgb, 7, 0, 15, 1234), &[0xe3, 0x70, 0xf4, 0xd2, 0x00, 0x77], "lgb\t%r7, 1234(%r15)", None),
        (|a| a.rxy(Rxy::Llgc, 7, 0, 15, 0), &[0xe3, 0x70, 0xf0, 0x00, 0x00, 0x90], "llgc\t%r7, 0(%r15)", None),
        (|a| a.rxy(Rxy::Llgh, 7, 0, 15, 8), &[0xe3, 0x70, 0xf0, 0x08, 0x00, 0x91], "llgh\t%r7, 8(%r15)", None),
        (|a| a.rxy(Rxy::Ley, 1, 0, 15, -8), &[0xed, 0x10, 0xff, 0xf8, 0xff, 0x64], "ley\t%f1, -8(%r15)", None),
        (|a| a.rxy(Rxy::Ldy, 1, 0, 15, 4095), &[0xed, 0x10, 0xff, 0xff, 0x00, 0x65], "ldy\t%f1, 4095(%r15)", None),
        (|a| a.rxy(Rxy::Stey, 1, 0, 15, 4096), &[0xed, 0x10, 0xf0, 0x00, 0x01, 0x66], "stey\t%f1, 4096(%r15)", None),
        (|a| a.rxy(Rxy::Stdy, 1, 0, 15, -4096), &[0xed, 0x10, 0xf0, 0x00, 0xff, 0x67], "stdy\t%f1, -4096(%r15)", None),
        (|a| a.rxy(Rxy::Lg, 7, 1, 15, -8), &[0xe3, 0x71, 0xff, 0xf8, 0xff, 0x04], "lg\t%r7, -8(%r1,%r15)", None),
        (|a| a.rxy(Rxy::Lg, 7, 1, 11, 0), &[0xe3, 0x71, 0xb0, 0x00, 0x00, 0x04], "lg\t%r7, 0(%r1,%r11)", None),
        (|a| a.rxy(Rxy::Stg, 0, 0, 15, 0), &[0xe3, 0x00, 0xf0, 0x00, 0x00, 0x24], "stg\t%r0, 0(%r15)", None),
        (|a| a.rxy(Rxy::Stdy, 8, 0, 11, -8), &[0xed, 0x80, 0xbf, 0xf8, 0xff, 0x67], "stdy\t%f8, -8(%r11)", None),
        (|a| a.rxy(Rxy::Ldy, 15, 0, 11, -16), &[0xed, 0xf0, 0xbf, 0xf0, 0xff, 0x65], "ldy\t%f15, -16(%r11)", None),
        (|a| a.rsy(Rsy::Stmg, 6, 15, 15, 48), &[0xeb, 0x6f, 0xf0, 0x30, 0x00, 0x24], "stmg\t%r6, %r15, 48(%r15)", None),
        (|a| a.rsy(Rsy::Lmg, 6, 15, 11, 48), &[0xeb, 0x6f, 0xb0, 0x30, 0x00, 0x04], "lmg\t%r6, %r15, 48(%r11)", None),
        (|a| a.rsy(Rsy::Srag, 0, 7, 0, 63), &[0xeb, 0x07, 0x00, 0x3f, 0x00, 0x0a], "srag\t%r0, %r7, 63", None),
        (|a| a.rsy(Rsy::Srlg, 7, 7, 0, 32), &[0xeb, 0x77, 0x00, 0x20, 0x00, 0x0c], "srlg\t%r7, %r7, 32", None),
        (|a| a.rsy(Rsy::Sllg, 7, 7, 8, 0), &[0xeb, 0x77, 0x80, 0x00, 0x00, 0x0d], "sllg\t%r7, %r7, 0(%r8)", None),
        (|a| a.rsy(Rsy::Sllg, 4, 4, 2, 0), &[0xeb, 0x44, 0x20, 0x00, 0x00, 0x0d], "sllg\t%r4, %r4, 0(%r2)", None),
        (|a| a.rsy(Rsy::Csg, 7, 5, 9, 0), &[0xeb, 0x75, 0x90, 0x00, 0x00, 0x30], "csg\t%r7, %r5, 0(%r9)", None),
        (|a| a.rsy(Rsy::Srag, 3, 3, 0, 0), &[0xeb, 0x33, 0x00, 0x00, 0x00, 0x0a], "srag\t%r3, %r3, 0", None),
        (|a| a.rsy(Rsy::Rllg, 1, 2, 0, 13), &[0xeb, 0x12, 0x00, 0x0d, 0x00, 0x1c], "rllg\t%r1, %r2, 13", None),
        (|a| a.rsy(Rsy::Rll, 1, 2, 0, 13), &[0xeb, 0x12, 0x00, 0x0d, 0x00, 0x1d], "rll\t%r1, %r2, 13", None),
        (|a| a.rsy(Rsy::Rllg, 1, 2, 3, 0), &[0xeb, 0x12, 0x30, 0x00, 0x00, 0x1c], "rllg\t%r1, %r2, 0(%r3)", None),
        (|a| a.rsy(Rsy::Rll, 1, 2, 3, 0), &[0xeb, 0x12, 0x30, 0x00, 0x00, 0x1d], "rll\t%r1, %r2, 0(%r3)", None),
        (|a| a.rsy(Rsy::Rllg, 14, 0, 0, 63), &[0xeb, 0xe0, 0x00, 0x3f, 0x00, 0x1c], "rllg\t%r14, %r0, 63", None),
        (|a| a.ri(Ri::Lghi, 7, 0i16 as u16), &[0xa7, 0x79, 0x00, 0x00], "lghi\t%r7, 0", None),
        (|a| a.ri(Ri::Lghi, 7, -1i16 as u16), &[0xa7, 0x79, 0xff, 0xff], "lghi\t%r7, -1", None),
        (|a| a.ri(Ri::Lghi, 7, 32767i16 as u16), &[0xa7, 0x79, 0x7f, 0xff], "lghi\t%r7, 32767", None),
        (|a| a.ri(Ri::Lghi, 7, -32768i16 as u16), &[0xa7, 0x79, 0x80, 0x00], "lghi\t%r7, -32768", None),
        (|a| a.ri(Ri::Aghi, 15, -160i16 as u16), &[0xa7, 0xfb, 0xff, 0x60], "aghi\t%r15, -160", None),
        (|a| a.ri(Ri::Aghi, 15, -4096i16 as u16), &[0xa7, 0xfb, 0xf0, 0x00], "aghi\t%r15, -4096", None),
        (|a| a.ri(Ri::Aghi, 15, 1i16 as u16), &[0xa7, 0xfb, 0x00, 0x01], "aghi\t%r15, 1", None),
        (|a| a.ril(Ril::Lgfi, 7, 0x7fffffff), &[0xc0, 0x71, 0x7f, 0xff, 0xff, 0xff], "lgfi\t%r7, 2147483647", None),
        (|a| a.ril(Ril::Lgfi, 7, 0x80000000), &[0xc0, 0x71, 0x80, 0x00, 0x00, 0x00], "lgfi\t%r7, -2147483648", None),
        (|a| a.ril(Ril::Lgfi, 7, 0xffffffff), &[0xc0, 0x71, 0xff, 0xff, 0xff, 0xff], "lgfi\t%r7, -1", None),
        (|a| a.ril(Ril::Iilf, 7, 0x0), &[0xc0, 0x79, 0x00, 0x00, 0x00, 0x00], "iilf\t%r7, 0", None),
        (|a| a.ril(Ril::Iilf, 7, 0xffffffff), &[0xc0, 0x79, 0xff, 0xff, 0xff, 0xff], "iilf\t%r7, 4294967295", None),
        (|a| a.ril(Ril::Iilf, 7, 0x80000000), &[0xc0, 0x79, 0x80, 0x00, 0x00, 0x00], "iilf\t%r7, 2147483648", None),
        (|a| a.ril(Ril::Llihf, 7, 0x80000000), &[0xc0, 0x7e, 0x80, 0x00, 0x00, 0x00], "llihf\t%r7, 2147483648", None),
        (|a| a.ril(Ril::Llihf, 7, 0x1), &[0xc0, 0x7e, 0x00, 0x00, 0x00, 0x01], "llihf\t%r7, 1", None),
        (|a| a.ril(Ril::Llilf, 7, 0xffffffff), &[0xc0, 0x7f, 0xff, 0xff, 0xff, 0xff], "llilf\t%r7, 4294967295", None),
        (|a| a.ril(Ril::Llilf, 7, 0x8000), &[0xc0, 0x7f, 0x00, 0x00, 0x80, 0x00], "llilf\t%r7, 32768", None),
        (|a| a.ril(Ril::Agfi, 7, 0x80000000), &[0xc2, 0x78, 0x80, 0x00, 0x00, 0x00], "agfi\t%r7, -2147483648", None),
        (|a| a.ril(Ril::Agfi, 7, 0x10000), &[0xc2, 0x78, 0x00, 0x01, 0x00, 0x00], "agfi\t%r7, 65536", None),
        (|a| a.ril(Ril::Cgfi, 7, 0xffffffff), &[0xc2, 0x7c, 0xff, 0xff, 0xff, 0xff], "cgfi\t%r7, -1", None),
        (|a| a.ril(Ril::Cgfi, 7, 0x186a0), &[0xc2, 0x7c, 0x00, 0x01, 0x86, 0xa0], "cgfi\t%r7, 100000", None),
        (|a| a.ril(Ril::Clgfi, 7, 0xffffffff), &[0xc2, 0x7e, 0xff, 0xff, 0xff, 0xff], "clgfi\t%r7, 4294967295", None),
        (|a| a.ril(Ril::Clgfi, 7, 0x7), &[0xc2, 0x7e, 0x00, 0x00, 0x00, 0x07], "clgfi\t%r7, 7", None),
        (|a| a.pcrel(Pcrel::Larl, 7, RelKind::S390Pc, 0), &[0xc0, 0x70, 0x00, 0x00, 0x00, 0x00], "larl\t%r7, 0x0", Some((2, RelKind::S390Pc))),
        (|a| a.pcrel(Pcrel::Lgrl, 7, RelKind::S390Got, 0), &[0xc4, 0x78, 0x00, 0x00, 0x00, 0x00], "lgrl\t%r7, 0x0", Some((2, RelKind::S390Got))),
        (|a| a.pcrel(Pcrel::Lgrl, 12, RelKind::S390TlsIe, 0), &[0xc4, 0xc8, 0x00, 0x00, 0x00, 0x00], "lgrl\t%r12, 0x0", Some((2, RelKind::S390TlsIe))),
        (|a| a.pcrel(Pcrel::Brasl, 14, RelKind::S390Call, 0), &[0xc0, 0xe5, 0x00, 0x00, 0x00, 0x00], "brasl\t%r14, 0x0", Some((2, RelKind::S390Call))),
        (|a| a.ss(Ss::Mvc, 256, 10, 0, 9, 0), &[0xd2, 0xff, 0xa0, 0x00, 0x90, 0x00], "mvc\t0(256,%r10), 0(%r9)", None),
        (|a| a.ss(Ss::Mvc, 17, 10, 768, 9, 768), &[0xd2, 0x10, 0xa3, 0x00, 0x93, 0x00], "mvc\t768(17,%r10), 768(%r9)", None),
        (|a| a.ss(Ss::Xc, 1, 10, 0, 10, 0), &[0xd7, 0x00, 0xa0, 0x00, 0xa0, 0x00], "xc\t0(1,%r10), 0(%r10)", None),
        (|a| a.ss(Ss::Xc, 256, 10, 4095, 10, 4095), &[0xd7, 0xff, 0xaf, 0xff, 0xaf, 0xff], "xc\t4095(256,%r10), 4095(%r10)", None),
        (|a| a.trap(), &[0x00, 0x00], "<unknown>", None),
        (|a| { a.brc_fwd(0); }, &[0xa7, 0x04, 0x00, 0x00], "jnop\t0x0", None),
        (|a| { a.brcl(0); }, &[0xc0, 0x04, 0x00, 0x00, 0x00, 0x00], "jgnop\t0x0", None),
        (|a| { a.brc_fwd(1); }, &[0xa7, 0x14, 0x00, 0x00], "jo\t0x0", None),
        (|a| { a.brcl(1); }, &[0xc0, 0x14, 0x00, 0x00, 0x00, 0x00], "jgo\t0x0", None),
        (|a| { a.brc_fwd(2); }, &[0xa7, 0x24, 0x00, 0x00], "jh\t0x0", None),
        (|a| { a.brcl(2); }, &[0xc0, 0x24, 0x00, 0x00, 0x00, 0x00], "jgh\t0x0", None),
        (|a| { a.brc_fwd(3); }, &[0xa7, 0x34, 0x00, 0x00], "jnle\t0x0", None),
        (|a| { a.brcl(3); }, &[0xc0, 0x34, 0x00, 0x00, 0x00, 0x00], "jgnle\t0x0", None),
        (|a| { a.brc_fwd(4); }, &[0xa7, 0x44, 0x00, 0x00], "jl\t0x0", None),
        (|a| { a.brcl(4); }, &[0xc0, 0x44, 0x00, 0x00, 0x00, 0x00], "jgl\t0x0", None),
        (|a| { a.brc_fwd(5); }, &[0xa7, 0x54, 0x00, 0x00], "jnhe\t0x0", None),
        (|a| { a.brcl(5); }, &[0xc0, 0x54, 0x00, 0x00, 0x00, 0x00], "jgnhe\t0x0", None),
        (|a| { a.brc_fwd(6); }, &[0xa7, 0x64, 0x00, 0x00], "jlh\t0x0", None),
        (|a| { a.brcl(6); }, &[0xc0, 0x64, 0x00, 0x00, 0x00, 0x00], "jglh\t0x0", None),
        (|a| { a.brc_fwd(7); }, &[0xa7, 0x74, 0x00, 0x00], "jne\t0x0", None),
        (|a| { a.brcl(7); }, &[0xc0, 0x74, 0x00, 0x00, 0x00, 0x00], "jgne\t0x0", None),
        (|a| { a.brc_fwd(8); }, &[0xa7, 0x84, 0x00, 0x00], "je\t0x0", None),
        (|a| { a.brcl(8); }, &[0xc0, 0x84, 0x00, 0x00, 0x00, 0x00], "jge\t0x0", None),
        (|a| { a.brc_fwd(9); }, &[0xa7, 0x94, 0x00, 0x00], "jnlh\t0x0", None),
        (|a| { a.brcl(9); }, &[0xc0, 0x94, 0x00, 0x00, 0x00, 0x00], "jgnlh\t0x0", None),
        (|a| { a.brc_fwd(10); }, &[0xa7, 0xa4, 0x00, 0x00], "jhe\t0x0", None),
        (|a| { a.brcl(10); }, &[0xc0, 0xa4, 0x00, 0x00, 0x00, 0x00], "jghe\t0x0", None),
        (|a| { a.brc_fwd(11); }, &[0xa7, 0xb4, 0x00, 0x00], "jnl\t0x0", None),
        (|a| { a.brcl(11); }, &[0xc0, 0xb4, 0x00, 0x00, 0x00, 0x00], "jgnl\t0x0", None),
        (|a| { a.brc_fwd(12); }, &[0xa7, 0xc4, 0x00, 0x00], "jle\t0x0", None),
        (|a| { a.brcl(12); }, &[0xc0, 0xc4, 0x00, 0x00, 0x00, 0x00], "jgle\t0x0", None),
        (|a| { a.brc_fwd(13); }, &[0xa7, 0xd4, 0x00, 0x00], "jnh\t0x0", None),
        (|a| { a.brcl(13); }, &[0xc0, 0xd4, 0x00, 0x00, 0x00, 0x00], "jgnh\t0x0", None),
        (|a| { a.brc_fwd(14); }, &[0xa7, 0xe4, 0x00, 0x00], "jno\t0x0", None),
        (|a| { a.brcl(14); }, &[0xc0, 0xe4, 0x00, 0x00, 0x00, 0x00], "jgno\t0x0", None),
        (|a| { a.brc_fwd(15); }, &[0xa7, 0xf4, 0x00, 0x00], "j\t0x0", None),
        (|a| { a.brcl(15); }, &[0xc0, 0xf4, 0x00, 0x00, 0x00, 0x00], "jg\t0x0", None),
        (|a| a.brc_back(4, 0), &[0xa7, 0x44, 0x00, 0x00], "jl\t0x0", None),
        (|a| a.brctg(1, 0), &[0xa7, 0x17, 0x00, 0x00], "brctg\t%r1, 0x0", None),
        (|a| a.vrx(Vop::Vl, 1, 1, 2, 0, 0), &[0xe7, 0x11, 0x20, 0x00, 0x00, 0x06], "vl\t%v1, 0(%r1,%r2)", None),
        (|a| a.vrx(Vop::Vl, 17, 1, 10, 4095, 0), &[0xe7, 0x11, 0xaf, 0xff, 0x08, 0x06], "vl\t%v17, 4095(%r1,%r10)", None),
        (|a| a.vrx(Vop::Vst, 31, 14, 3, 8, 0), &[0xe7, 0xfe, 0x30, 0x08, 0x08, 0x0e], "vst\t%v31, 8(%r14,%r3)", None),
        (|a| a.vrx(Vop::Vlrep, 2, 0, 15, 16, 0), &[0xe7, 0x20, 0xf0, 0x10, 0x00, 0x05], "vlrepb\t%v2, 16(%r15)", None),
        (|a| a.vrx(Vop::Vlrep, 18, 0, 15, 16, 1), &[0xe7, 0x20, 0xf0, 0x10, 0x18, 0x05], "vlreph\t%v18, 16(%r15)", None),
        (|a| a.vrx(Vop::Vlrep, 2, 0, 15, 16, 2), &[0xe7, 0x20, 0xf0, 0x10, 0x20, 0x05], "vlrepf\t%v2, 16(%r15)", None),
        (|a| a.vrx(Vop::Vlrep, 2, 0, 15, 16, 3), &[0xe7, 0x20, 0xf0, 0x10, 0x30, 0x05], "vlrepg\t%v2, 16(%r15)", None),
        (|a| a.vrx(Vop::Vleg, 3, 1, 2, 0, 0), &[0xe7, 0x31, 0x20, 0x00, 0x00, 0x02], "vleg\t%v3, 0(%r1,%r2), 0", None),
        (|a| a.vrx(Vop::Vlef, 19, 1, 2, 0, 0), &[0xe7, 0x31, 0x20, 0x00, 0x08, 0x03], "vlef\t%v19, 0(%r1,%r2), 0", None),
        (|a| a.vrx(Vop::Vsteg, 3, 1, 2, 0, 0), &[0xe7, 0x31, 0x20, 0x00, 0x00, 0x0a], "vsteg\t%v3, 0(%r1,%r2), 0", None),
        (|a| a.vrx(Vop::Vstef, 19, 1, 2, 0, 0), &[0xe7, 0x31, 0x20, 0x00, 0x08, 0x0b], "vstef\t%v19, 0(%r1,%r2), 0", None),
        (|a| a.vrs(Vop::Vlvg, 20, 14, 0, 0), &[0xe7, 0x4e, 0x00, 0x00, 0x08, 0x22], "vlvgb\t%v20, %r14, 0", None),
        (|a| a.vrs(Vop::Vlvg, 4, 14, 0, 3), &[0xe7, 0x4e, 0x00, 0x00, 0x30, 0x22], "vlvgg\t%v4, %r14, 0", None),
        (|a| a.vrep(20, 20, 0, 0), &[0xe7, 0x44, 0x00, 0x00, 0x0c, 0x4d], "vrepb\t%v20, %v20, 0", None),
        (|a| a.vrep(4, 4, 0, 3), &[0xe7, 0x44, 0x00, 0x00, 0x30, 0x4d], "vrepg\t%v4, %v4, 0", None),
        (|a| a.vri(Vop::Vrepi, 5, 0xffff, 1), &[0xe7, 0x50, 0xff, 0xff, 0x10, 0x45], "vrepih\t%v5, -1", None),
        (|a| a.vri(Vop::Vrepi, 21, 300, 2), &[0xe7, 0x50, 0x01, 0x2c, 0x28, 0x45], "vrepif\t%v21, 300", None),
        (|a| a.vri(Vop::Vgbm, 6, 0, 0), &[0xe7, 0x60, 0x00, 0x00, 0x00, 0x44], "vzero\t%v6", None),
        (|a| a.vri(Vop::Vgbm, 22, 0xffff, 0), &[0xe7, 0x60, 0xff, 0xff, 0x08, 0x44], "vone\t%v22", None),
        (|a| a.vri(Vop::Vgbm, 6, 255, 0), &[0xe7, 0x60, 0x00, 0xff, 0x00, 0x44], "vgbm\t%v6, 255", None),
        (|a| a.vrr(Vop::Vlr, 1, 17, 0, 0, 0), &[0xe7, 0x11, 0x00, 0x00, 0x04, 0x56], "vlr\t%v1, %v17", None),
        (|a| a.vrr(Vop::Vuph, 1, 17, 0, 0, 0), &[0xe7, 0x11, 0x00, 0x00, 0x04, 0xd7], "vuphb\t%v1, %v17", None),
        (|a| a.vrr(Vop::Vuph, 1, 2, 0, 1, 0), &[0xe7, 0x12, 0x00, 0x00, 0x10, 0xd7], "vuphh\t%v1, %v2", None),
        (|a| a.vrr(Vop::Vuph, 1, 2, 0, 2, 0), &[0xe7, 0x12, 0x00, 0x00, 0x20, 0xd7], "vuphf\t%v1, %v2", None),
        (|a| a.vrr(Vop::Vuplh, 1, 17, 0, 0, 0), &[0xe7, 0x11, 0x00, 0x00, 0x04, 0xd5], "vuplhb\t%v1, %v17", None),
        (|a| a.vrr(Vop::Vpk, 1, 2, 18, 1, 0), &[0xe7, 0x12, 0x20, 0x00, 0x12, 0x94], "vpkh\t%v1, %v2, %v18", None),
        (|a| a.vrr(Vop::Vpk, 1, 2, 2, 2, 0), &[0xe7, 0x12, 0x20, 0x00, 0x20, 0x94], "vpkf\t%v1, %v2, %v2", None),
        (|a| a.vrr(Vop::Vpk, 1, 2, 2, 3, 0), &[0xe7, 0x12, 0x20, 0x00, 0x30, 0x94], "vpkg\t%v1, %v2, %v2", None),
        (|a| a.vrr(Vop::Va, 1, 2, 3, 0, 0), &[0xe7, 0x12, 0x30, 0x00, 0x00, 0xf3], "vab\t%v1, %v2, %v3", None),
        (|a| a.vrr(Vop::Va, 17, 18, 19, 2, 0), &[0xe7, 0x12, 0x30, 0x00, 0x2e, 0xf3], "vaf\t%v17, %v18, %v19", None),
        (|a| a.vrr(Vop::Va, 1, 2, 3, 3, 0), &[0xe7, 0x12, 0x30, 0x00, 0x30, 0xf3], "vag\t%v1, %v2, %v3", None),
        (|a| a.vrr(Vop::Vs, 1, 2, 3, 1, 0), &[0xe7, 0x12, 0x30, 0x00, 0x10, 0xf7], "vsh\t%v1, %v2, %v3", None),
        (|a| a.vrr(Vop::Vml, 1, 2, 3, 0, 0), &[0xe7, 0x12, 0x30, 0x00, 0x00, 0xa2], "vmlb\t%v1, %v2, %v3", None),
        (|a| a.vrr(Vop::Vml, 1, 2, 3, 1, 0), &[0xe7, 0x12, 0x30, 0x00, 0x10, 0xa2], "vmlhw\t%v1, %v2, %v3", None),
        (|a| a.vrr(Vop::Vml, 1, 2, 3, 2, 0), &[0xe7, 0x12, 0x30, 0x00, 0x20, 0xa2], "vmlf\t%v1, %v2, %v3", None),
        (|a| a.vrr(Vop::Vmh, 1, 2, 3, 0, 0), &[0xe7, 0x12, 0x30, 0x00, 0x00, 0xa3], "vmhb\t%v1, %v2, %v3", None),
        (|a| a.vrr(Vop::Vmh, 1, 2, 3, 1, 0), &[0xe7, 0x12, 0x30, 0x00, 0x10, 0xa3], "vmhh\t%v1, %v2, %v3", None),
        (|a| a.vrr(Vop::Vmh, 1, 2, 3, 2, 0), &[0xe7, 0x12, 0x30, 0x00, 0x20, 0xa3], "vmhf\t%v1, %v2, %v3", None),
        (|a| a.vrr(Vop::Vmlh, 1, 2, 3, 0, 0), &[0xe7, 0x12, 0x30, 0x00, 0x00, 0xa1], "vmlhb\t%v1, %v2, %v3", None),
        (|a| a.vrr(Vop::Vmlh, 1, 2, 3, 1, 0), &[0xe7, 0x12, 0x30, 0x00, 0x10, 0xa1], "vmlhh\t%v1, %v2, %v3", None),
        (|a| a.vrr(Vop::Vmlh, 1, 2, 3, 2, 0), &[0xe7, 0x12, 0x30, 0x00, 0x20, 0xa1], "vmlhf\t%v1, %v2, %v3", None),
        (|a| a.vrr(Vop::Vn, 1, 2, 3, 0, 0), &[0xe7, 0x12, 0x30, 0x00, 0x00, 0x68], "vn\t%v1, %v2, %v3", None),
        (|a| a.vrr(Vop::Vnc, 1, 2, 3, 0, 0), &[0xe7, 0x12, 0x30, 0x00, 0x00, 0x69], "vnc\t%v1, %v2, %v3", None),
        (|a| a.vrr(Vop::Vo, 1, 2, 3, 0, 0), &[0xe7, 0x12, 0x30, 0x00, 0x00, 0x6a], "vo\t%v1, %v2, %v3", None),
        (|a| a.vrr(Vop::Vno, 1, 2, 2, 0, 0), &[0xe7, 0x12, 0x20, 0x00, 0x00, 0x6b], "vno\t%v1, %v2, %v2", None),
        (|a| a.vrr(Vop::Vx, 1, 2, 3, 0, 0), &[0xe7, 0x12, 0x30, 0x00, 0x00, 0x6d], "vx\t%v1, %v2, %v3", None),
        (|a| a.vrr(Vop::Vceq, 1, 2, 3, 0, 0), &[0xe7, 0x12, 0x30, 0x00, 0x00, 0xf8], "vceqb\t%v1, %v2, %v3", None),
        (|a| a.vrr(Vop::Vceq, 1, 2, 3, 3, 0), &[0xe7, 0x12, 0x30, 0x00, 0x30, 0xf8], "vceqg\t%v1, %v2, %v3", None),
        (|a| a.vrr(Vop::Vch, 1, 2, 3, 1, 0), &[0xe7, 0x12, 0x30, 0x00, 0x10, 0xfb], "vchh\t%v1, %v2, %v3", None),
        (|a| a.vrr(Vop::Vchl, 1, 2, 3, 2, 0), &[0xe7, 0x12, 0x30, 0x00, 0x20, 0xf9], "vchlf\t%v1, %v2, %v3", None),
        (|a| a.vrr(Vop::Vch, 30, 31, 1, 3, 1), &[0xe7, 0xef, 0x10, 0x10, 0x3c, 0xfb], "vchgs\t%v30, %v31, %v1", None),
        (|a| a.vrr(Vop::Vch, 30, 31, 1, 0, 1), &[0xe7, 0xef, 0x10, 0x10, 0x0c, 0xfb], "vchbs\t%v30, %v31, %v1", None),
        (|a| a.vrr(Vop::Vmn, 1, 2, 3, 0, 0), &[0xe7, 0x12, 0x30, 0x00, 0x00, 0xfe], "vmnb\t%v1, %v2, %v3", None),
        (|a| a.vrr(Vop::Vmnl, 1, 2, 3, 3, 0), &[0xe7, 0x12, 0x30, 0x00, 0x30, 0xfc], "vmnlg\t%v1, %v2, %v3", None),
        (|a| a.vrr(Vop::Vmx, 1, 2, 3, 1, 0), &[0xe7, 0x12, 0x30, 0x00, 0x10, 0xff], "vmxh\t%v1, %v2, %v3", None),
        (|a| a.vrr(Vop::Vmxl, 1, 2, 3, 2, 0), &[0xe7, 0x12, 0x30, 0x00, 0x20, 0xfd], "vmxlf\t%v1, %v2, %v3", None),
        (|a| a.vrs(Vop::Vesl, 1, 2, 3, 1), &[0xe7, 0x12, 0x00, 0x03, 0x10, 0x30], "veslh\t%v1, %v2, 3", None),
        (|a| a.vrs(Vop::Vesra, 1, 2, 63, 3), &[0xe7, 0x12, 0x00, 0x3f, 0x30, 0x3a], "vesrag\t%v1, %v2, 63", None),
        (|a| a.vrs(Vop::Vesrl, 1, 2, 7, 0), &[0xe7, 0x12, 0x00, 0x07, 0x00, 0x38], "vesrlb\t%v1, %v2, 7", None),
        (|a| a.vsel(1, 2, 3, 31), &[0xe7, 0x12, 0x30, 0x00, 0xf1, 0x8d], "vsel\t%v1, %v2, %v3, %v31", None),
        (|a| a.vfma(1, 2, 3, 31, 3), &[0xe7, 0x12, 0x33, 0x00, 0xf1, 0x8f], "vfmadb\t%v1, %v2, %v3, %v31", None),
        (|a| a.vfma(1, 2, 3, 4, 2), &[0xe7, 0x12, 0x32, 0x00, 0x40, 0x8f], "vfmasb\t%v1, %v2, %v3, %v4", None),
        (|a| a.madbr(true, 1, 2, 3), &[0xb3, 0x1e, 0x10, 0x23], "madbr\t%f1, %f2, %f3", None),
        (|a| a.madbr(false, 0, 8, 15), &[0xb3, 0x0e, 0x00, 0x8f], "maebr\t%f0, %f8, %f15", None),
        (|a| a.vrr(Vop::Vfa, 1, 2, 3, 3, 0), &[0xe7, 0x12, 0x30, 0x00, 0x30, 0xe3], "vfadb\t%v1, %v2, %v3", None),
        (|a| a.vrr(Vop::Vfs, 1, 2, 3, 3, 0), &[0xe7, 0x12, 0x30, 0x00, 0x30, 0xe2], "vfsdb\t%v1, %v2, %v3", None),
        (|a| a.vrr(Vop::Vfm, 1, 2, 3, 3, 0), &[0xe7, 0x12, 0x30, 0x00, 0x30, 0xe7], "vfmdb\t%v1, %v2, %v3", None),
        (|a| a.vrr(Vop::Vfd, 1, 2, 3, 3, 0), &[0xe7, 0x12, 0x30, 0x00, 0x30, 0xe5], "vfddb\t%v1, %v2, %v3", None),
        (|a| a.vrr(Vop::Vfsq, 1, 2, 0, 3, 0), &[0xe7, 0x12, 0x00, 0x00, 0x30, 0xce], "vfsqdb\t%v1, %v2", None),
        (|a| a.vrr(Vop::Vfce, 1, 2, 3, 3, 0), &[0xe7, 0x12, 0x30, 0x00, 0x30, 0xe8], "vfcedb\t%v1, %v2, %v3", None),
        (|a| a.vrr(Vop::Vfch, 1, 2, 3, 3, 0), &[0xe7, 0x12, 0x30, 0x00, 0x30, 0xeb], "vfchdb\t%v1, %v2, %v3", None),
        (|a| a.vrr(Vop::Vfche, 1, 2, 3, 3, 0), &[0xe7, 0x12, 0x30, 0x00, 0x30, 0xea], "vfchedb\t%v1, %v2, %v3", None),
        (|a| a.vrr(Vop::Vfa, 1, 2, 3, 2, 0), &[0xe7, 0x12, 0x30, 0x00, 0x20, 0xe3], "vfasb\t%v1, %v2, %v3", None),
        (|a| a.vrr(Vop::Vfch, 1, 2, 3, 2, 0), &[0xe7, 0x12, 0x30, 0x00, 0x20, 0xeb], "vfchsb\t%v1, %v2, %v3", None),
        (|a| a.vrr(Vop::Vfsq, 1, 2, 0, 2, 0), &[0xe7, 0x12, 0x00, 0x00, 0x20, 0xce], "vfsqsb\t%v1, %v2", None),
    ]
}

/// `text` without objdump's `<..>` annotations.
fn bare(text: &str) -> String {
    match text.find(" <") {
        Some(i) => text[..i].to_owned(),
        None => text.to_owned(),
    }
}

#[test]
fn forms_match_the_reference_assembler() {
    for (n, (f, bytes, text, rel)) in cases().into_iter().enumerate() {
        let mut a = Asm::new(true);
        f(&mut a);
        assert_eq!(a.code, bytes, "case {n}: {text}");
        let l = Listing {
            name: "f",
            code: &a.code,
            labels: Vec::new(),
            relocs: &a.relocs,
        };
        let lines = a.lines(&l);
        assert_eq!(lines.len(), 1, "case {n}: {text}");
        assert_eq!(bare(&lines[0].1), text, "case {n}");
        let relocs: Vec<_> = a.relocs.iter().map(|r| (r.0, r.1)).collect();
        assert_eq!(
            relocs,
            rel.into_iter().collect::<Vec<_>>(),
            "case {n}: {text}"
        );
        // Text off: the same bytes, nothing recorded.
        let mut b = Asm::new(false);
        f(&mut b);
        assert_eq!(b.code, bytes);
        assert!(b.lines(&l).is_empty());
    }
}

/// A listing names labels, patched branch targets and relocations.
#[test]
fn listing_shows_labels_targets_and_relocations() {
    let mut obj = crate::obj::Obj::new(false, true, false);
    obj.s390x = true;
    let g = obj.sym("g");
    let mut a = Asm::new(true);
    a.rre(Rre::Lgr, 11, 15);
    let top = a.pos();
    a.pcrel(Pcrel::Brasl, 14, RelKind::S390Call, g);
    let fwd = a.brc_fwd(8);
    let far = a.brcl(7);
    a.brctg(1, top);
    a.here(fwd);
    a.br(14);
    a.patch_brcl(far, top);
    let l = Listing {
        name: "f",
        code: &a.code,
        labels: vec![(top as u32, 3)],
        relocs: &a.relocs,
    };
    let lines = a.lines(&l);
    crate::asm::function(&mut obj, &l, &lines, crate::asm::Bytes::Short);
    let want = "
<f>:
       0: b9 04 00 bf  \tlgr\t%r11, %r15
.L3:
       4: c0 e5 00 00 00 00    \tbrasl\t%r14, 0x4 <.L3>
\t\t0000000000000006:  R_390_PLT32DBL\tg+0x2
       a: a7 84 00 07  \tje\t0x18 <f+0x18>
       e: c0 74 ff ff ff fb    \tjgne\t0x4 <.L3>
      14: a7 17 ff f8  \tbrctg\t%r1, 0x4 <.L3>
      18: 07 fe        \tbr\t%r14
";
    assert_eq!(obj.asm.as_deref(), Some(want));
}
