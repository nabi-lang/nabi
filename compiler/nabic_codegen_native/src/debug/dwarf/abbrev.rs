//! DWARF constants and the abbreviation table.

#[allow(clippy::wildcard_imports)]
use crate::debug::dwarf::*;

pub(crate) const DW_TAG_ARRAY_TYPE: u8 = 0x01;

pub(crate) const DW_TAG_FORMAL_PARAMETER: u8 = 0x05;

pub(crate) const DW_TAG_LEXICAL_BLOCK: u8 = 0x0b;

pub(crate) const DW_TAG_MEMBER: u8 = 0x0d;

pub(crate) const DW_TAG_POINTER_TYPE: u8 = 0x0f;

pub(crate) const DW_TAG_COMPILE_UNIT: u8 = 0x11;

pub(crate) const DW_TAG_STRUCTURE_TYPE: u8 = 0x13;

pub(crate) const DW_TAG_SUBROUTINE_TYPE: u8 = 0x15;

pub(crate) const DW_TAG_VARIANT: u8 = 0x19;

pub(crate) const DW_TAG_SUBRANGE_TYPE: u8 = 0x21;

pub(crate) const DW_TAG_VARIANT_PART: u8 = 0x33;

pub(crate) const DW_TAG_BASE_TYPE: u8 = 0x24;

pub(crate) const DW_TAG_SUBPROGRAM: u8 = 0x2e;

pub(crate) const DW_TAG_VARIABLE: u8 = 0x34;

pub(crate) const DW_TAG_INLINED_SUBROUTINE: u8 = 0x1d;

pub(crate) const DW_AT_NAME: u8 = 0x03;

pub(crate) const DW_AT_BYTE_SIZE: u8 = 0x0b;

pub(crate) const DW_AT_STMT_LIST: u8 = 0x10;

pub(crate) const DW_AT_LOW_PC: u8 = 0x11;

pub(crate) const DW_AT_HIGH_PC: u8 = 0x12;

pub(crate) const DW_AT_LANGUAGE: u8 = 0x13;

pub(crate) const DW_AT_COMP_DIR: u8 = 0x1b;

pub(crate) const DW_AT_PRODUCER: u8 = 0x25;

pub(crate) const DW_AT_DISCR: u8 = 0x15;

pub(crate) const DW_AT_DISCR_VALUE: u8 = 0x16;

pub(crate) const DW_AT_COUNT: u8 = 0x37;

pub(crate) const DW_AT_DATA_MEMBER_LOCATION: u8 = 0x38;

pub(crate) const DW_AT_DECL_FILE: u8 = 0x3a;

pub(crate) const DW_AT_DECLARATION: u8 = 0x3c;

pub(crate) const DW_AT_ARTIFICIAL: u8 = 0x34;

pub(crate) const DW_AT_DECL_LINE: u8 = 0x3b;

pub(crate) const DW_AT_ENCODING: u8 = 0x3e;

pub(crate) const DW_AT_FRAME_BASE: u8 = 0x40;

pub(crate) const DW_AT_LOCATION: u8 = 0x02;

pub(crate) const DW_AT_TYPE: u8 = 0x49;

pub(crate) const DW_AT_RANGES: u8 = 0x55;

pub(crate) const DW_AT_INLINE: u8 = 0x20;

pub(crate) const DW_AT_ABSTRACT_ORIGIN: u8 = 0x31;

pub(crate) const DW_AT_CALL_COLUMN: u8 = 0x57;

pub(crate) const DW_AT_CALL_FILE: u8 = 0x58;

pub(crate) const DW_AT_CALL_LINE: u8 = 0x59;

/// `DW_AT_APPLE_optimized` (a 2-byte ULEB128 code).
pub(crate) const DW_AT_APPLE_OPTIMIZED: u16 = 0x3fe1;

/// `DW_INL_inlined`.
pub(crate) const DW_INL_INLINED: u8 = 1;

pub(crate) const DW_FORM_ADDR: u8 = 0x01;

pub(crate) const DW_FORM_DATA1: u8 = 0x0b;

pub(crate) const DW_FORM_DATA2: u8 = 0x05;

pub(crate) const DW_FORM_DATA4: u8 = 0x06;

pub(crate) const DW_FORM_STRING: u8 = 0x08;

pub(crate) const DW_FORM_SDATA: u8 = 0x0d;

pub(crate) const DW_FORM_UDATA: u8 = 0x0f;

pub(crate) const DW_FORM_REF4: u8 = 0x13;

pub(crate) const DW_FORM_SEC_OFFSET: u8 = 0x17;

pub(crate) const DW_FORM_EXPRLOC: u8 = 0x18;

pub(crate) const DW_FORM_FLAG_PRESENT: u8 = 0x19;

pub(crate) const DW_OP_FBREG: u8 = 0x91;

pub(crate) const DW_OP_DEREF: u8 = 0x06;

pub(crate) const DW_OP_PLUS_UCONST: u8 = 0x23;

pub(crate) const DW_OP_WASM_LOCATION: u8 = 0xed;

pub(crate) const DW_OP_REG0: u8 = 0x50;

pub(crate) const DW_OP_BREG0: u8 = 0x70;

pub(crate) const DW_OP_REGX: u8 = 0x90;

pub(crate) const DW_OP_BREGX: u8 = 0x92;

pub(crate) const DW_OP_CONSTU: u8 = 0x10;

pub(crate) const DW_OP_PIECE: u8 = 0x93;

pub(crate) const DW_OP_STACK_VALUE: u8 = 0x9f;

/// `DW_OP_reg29` (arm64 `x29`) / `DW_OP_reg6` (`x86_64` `rbp`) /
/// `DW_OP_reg5` (i386 `ebp`).
pub(crate) const DW_OP_REG29: u8 = 0x6d;

pub(crate) const DW_OP_REG6: u8 = 0x56;

pub(crate) const DW_OP_REG5: u8 = 0x55;

/// `DW_OP_reg11` (32-bit ARM `r11`).
pub(crate) const DW_OP_REG11: u8 = 0x5b;

/// `DW_OP_reg8` (RISC-V `s0`).
pub(crate) const DW_OP_REG8: u8 = 0x58;

/// `DW_OP_reg31` (powerpc64 `r31`).
pub(crate) const DW_OP_REG31: u8 = 0x6f;

/// Rust: the language debuggers read variant parts in (lldb only there); nabi's
/// types are C-like otherwise.
pub(crate) const DW_LANG_RUST: u16 = 0x1c;

pub(crate) const DW_ATE_BOOLEAN: u8 = 0x02;

pub(crate) const DW_ATE_FLOAT: u8 = 0x04;

pub(crate) const DW_ATE_SIGNED: u8 = 0x05;

pub(crate) const DW_ATE_SIGNED_CHAR: u8 = 0x06;

pub(crate) const DW_ATE_UNSIGNED: u8 = 0x07;

pub(crate) const DW_ATE_UTF: u8 = 0x10;

// Abbreviation codes.
pub(crate) const A_CU: u8 = 1;

pub(crate) const A_SUB: u8 = 2;

pub(crate) const A_PARAM: u8 = 3;

pub(crate) const A_VAR: u8 = 4;

pub(crate) const A_BLOCK: u8 = 5;

pub(crate) const A_BASE: u8 = 6;

pub(crate) const A_PTR_NAMED: u8 = 7;

pub(crate) const A_PTR_TO: u8 = 8;

pub(crate) const A_STRUCT: u8 = 9;

pub(crate) const A_MEMBER1: u8 = 10;

pub(crate) const A_ARRAY: u8 = 11;

pub(crate) const A_SUBRANGE: u8 = 12;

pub(crate) const A_STRUCT_EMPTY: u8 = 13;

pub(crate) const A_MEMBER4: u8 = 14;

pub(crate) const A_STRUCT_DECL: u8 = 15;

pub(crate) const A_ARRAY_OPEN: u8 = 16;

pub(crate) const A_VARIANT_PART: u8 = 17;

pub(crate) const A_VARIANT: u8 = 18;

pub(crate) const A_DISCR: u8 = 19;

pub(crate) const A_SUB_RET: u8 = 20;

// Subprograms without parameters or locals: no child list (as LLVM).
pub(crate) const A_SUB_LEAF: u8 = 21;

pub(crate) const A_SUB_RET_LEAF: u8 = 22;

// Subroutine types: with a return type or not, with parameters or not.
pub(crate) const A_FN_RET: u8 = 23;

pub(crate) const A_FN: u8 = 24;

pub(crate) const A_FN_RET_LEAF: u8 = 25;

pub(crate) const A_FN_LEAF: u8 = 26;

pub(crate) const A_FN_PARAM: u8 = 27;

// Optimized code: variables by location list or of none; abstract
// subprograms and their inlined copies.
pub(crate) const A_PARAM_LIST: u8 = 28;

pub(crate) const A_VAR_LIST: u8 = 29;

pub(crate) const A_PARAM_NONE: u8 = 30;

pub(crate) const A_VAR_NONE: u8 = 31;

pub(crate) const A_ABSTRACT: u8 = 32;

pub(crate) const A_INLINED: u8 = 33;

/// `split`: a code section per function (ELF), so ranges are per function
/// and a dropped one's is empty (both ends 0). `opt`: optimized code's
/// too (`apple`: the unit says so to lldb).
pub(crate) fn abbrevs(split: bool, opt: bool, apple: bool) -> Vec<u8> {
    let cu_end = if split {
        (DW_AT_RANGES, DW_FORM_SEC_OFFSET)
    } else {
        (DW_AT_HIGH_PC, DW_FORM_DATA4)
    };
    let mut b = Vec::new();
    let mut add = |code: u8, tag: u8, children: bool, attrs: &[(u8, u8)]| {
        b.push(code);
        b.push(tag);
        b.push(u8::from(children));
        for (a, f) in attrs {
            b.push(*a);
            b.push(*f);
        }
        if code == A_CU && opt && apple {
            uleb(&mut b, u64::from(DW_AT_APPLE_OPTIMIZED));
            b.push(DW_FORM_FLAG_PRESENT);
        }
        b.extend_from_slice(&[0, 0]);
    };
    add(
        A_CU,
        DW_TAG_COMPILE_UNIT,
        true,
        &[
            (DW_AT_PRODUCER, DW_FORM_STRING),
            (DW_AT_LANGUAGE, DW_FORM_DATA2),
            (DW_AT_NAME, DW_FORM_STRING),
            (DW_AT_COMP_DIR, DW_FORM_STRING),
            (DW_AT_STMT_LIST, DW_FORM_SEC_OFFSET),
            (DW_AT_LOW_PC, DW_FORM_ADDR),
            cu_end,
        ],
    );
    let sub = [
        (DW_AT_NAME, DW_FORM_STRING),
        (DW_AT_DECL_FILE, DW_FORM_UDATA),
        (DW_AT_DECL_LINE, DW_FORM_UDATA),
        (DW_AT_LOW_PC, DW_FORM_ADDR),
        (
            DW_AT_HIGH_PC,
            if split { DW_FORM_ADDR } else { DW_FORM_DATA4 },
        ),
        (DW_AT_FRAME_BASE, DW_FORM_EXPRLOC),
        (DW_AT_TYPE, DW_FORM_REF4),
    ];
    add(A_SUB, DW_TAG_SUBPROGRAM, true, &sub[..6]);
    add(A_SUB_RET, DW_TAG_SUBPROGRAM, true, &sub);
    add(A_SUB_LEAF, DW_TAG_SUBPROGRAM, false, &sub[..6]);
    add(A_SUB_RET_LEAF, DW_TAG_SUBPROGRAM, false, &sub);
    let var = [
        (DW_AT_NAME, DW_FORM_STRING),
        (DW_AT_DECL_FILE, DW_FORM_UDATA),
        (DW_AT_DECL_LINE, DW_FORM_UDATA),
        (DW_AT_TYPE, DW_FORM_REF4),
        (DW_AT_LOCATION, DW_FORM_EXPRLOC),
    ];
    add(A_PARAM, DW_TAG_FORMAL_PARAMETER, false, &var);
    add(A_VAR, DW_TAG_VARIABLE, false, &var);
    add(
        A_BLOCK,
        DW_TAG_LEXICAL_BLOCK,
        true,
        &[(DW_AT_RANGES, DW_FORM_SEC_OFFSET)],
    );
    add(
        A_BASE,
        DW_TAG_BASE_TYPE,
        false,
        &[
            (DW_AT_NAME, DW_FORM_STRING),
            (DW_AT_ENCODING, DW_FORM_DATA1),
            (DW_AT_BYTE_SIZE, DW_FORM_UDATA),
        ],
    );
    add(
        A_PTR_NAMED,
        DW_TAG_POINTER_TYPE,
        false,
        &[
            (DW_AT_NAME, DW_FORM_STRING),
            (DW_AT_BYTE_SIZE, DW_FORM_UDATA),
        ],
    );
    add(
        A_PTR_TO,
        DW_TAG_POINTER_TYPE,
        false,
        &[(DW_AT_TYPE, DW_FORM_REF4), (DW_AT_BYTE_SIZE, DW_FORM_UDATA)],
    );
    add(
        A_STRUCT,
        DW_TAG_STRUCTURE_TYPE,
        true,
        &[
            (DW_AT_NAME, DW_FORM_STRING),
            (DW_AT_BYTE_SIZE, DW_FORM_UDATA),
        ],
    );
    add(
        A_MEMBER1,
        DW_TAG_MEMBER,
        false,
        &[
            (DW_AT_NAME, DW_FORM_STRING),
            (DW_AT_TYPE, DW_FORM_REF4),
            (DW_AT_DATA_MEMBER_LOCATION, DW_FORM_DATA1),
        ],
    );
    add(
        A_ARRAY,
        DW_TAG_ARRAY_TYPE,
        true,
        &[(DW_AT_TYPE, DW_FORM_REF4)],
    );
    add(
        A_SUBRANGE,
        DW_TAG_SUBRANGE_TYPE,
        false,
        &[(DW_AT_COUNT, DW_FORM_UDATA)],
    );
    add(
        A_STRUCT_EMPTY,
        DW_TAG_STRUCTURE_TYPE,
        false,
        &[
            (DW_AT_NAME, DW_FORM_STRING),
            (DW_AT_BYTE_SIZE, DW_FORM_UDATA),
        ],
    );
    add(
        A_MEMBER4,
        DW_TAG_MEMBER,
        false,
        &[
            (DW_AT_NAME, DW_FORM_STRING),
            (DW_AT_TYPE, DW_FORM_REF4),
            (DW_AT_DATA_MEMBER_LOCATION, DW_FORM_DATA4),
        ],
    );
    add(
        A_STRUCT_DECL,
        DW_TAG_STRUCTURE_TYPE,
        false,
        &[
            (DW_AT_NAME, DW_FORM_STRING),
            (DW_AT_DECLARATION, DW_FORM_FLAG_PRESENT),
        ],
    );
    add(
        A_ARRAY_OPEN,
        DW_TAG_ARRAY_TYPE,
        false,
        &[(DW_AT_TYPE, DW_FORM_REF4)],
    );
    add(
        A_VARIANT_PART,
        DW_TAG_VARIANT_PART,
        true,
        &[(DW_AT_DISCR, DW_FORM_REF4)],
    );
    add(
        A_VARIANT,
        DW_TAG_VARIANT,
        true,
        &[(DW_AT_DISCR_VALUE, DW_FORM_SDATA)],
    );
    add(
        A_DISCR,
        DW_TAG_MEMBER,
        false,
        &[
            (DW_AT_TYPE, DW_FORM_REF4),
            (DW_AT_DATA_MEMBER_LOCATION, DW_FORM_DATA1),
            (DW_AT_ARTIFICIAL, DW_FORM_FLAG_PRESENT),
        ],
    );
    let ret = [(DW_AT_TYPE, DW_FORM_REF4)];
    add(A_FN_RET, DW_TAG_SUBROUTINE_TYPE, true, &ret);
    add(A_FN, DW_TAG_SUBROUTINE_TYPE, true, &[]);
    add(A_FN_RET_LEAF, DW_TAG_SUBROUTINE_TYPE, false, &ret);
    add(A_FN_LEAF, DW_TAG_SUBROUTINE_TYPE, false, &[]);
    add(A_FN_PARAM, DW_TAG_FORMAL_PARAMETER, false, &ret);
    if opt {
        let mut list = var;
        list[4].1 = DW_FORM_SEC_OFFSET;
        add(A_PARAM_LIST, DW_TAG_FORMAL_PARAMETER, false, &list);
        add(A_VAR_LIST, DW_TAG_VARIABLE, false, &list);
        add(A_PARAM_NONE, DW_TAG_FORMAL_PARAMETER, false, &var[..4]);
        add(A_VAR_NONE, DW_TAG_VARIABLE, false, &var[..4]);
        add(
            A_ABSTRACT,
            DW_TAG_SUBPROGRAM,
            false,
            &[
                (DW_AT_NAME, DW_FORM_STRING),
                (DW_AT_DECL_FILE, DW_FORM_UDATA),
                (DW_AT_DECL_LINE, DW_FORM_UDATA),
                (DW_AT_INLINE, DW_FORM_DATA1),
            ],
        );
        add(
            A_INLINED,
            DW_TAG_INLINED_SUBROUTINE,
            true,
            &[
                (DW_AT_ABSTRACT_ORIGIN, DW_FORM_REF4),
                (DW_AT_RANGES, DW_FORM_SEC_OFFSET),
                (DW_AT_CALL_FILE, DW_FORM_UDATA),
                (DW_AT_CALL_LINE, DW_FORM_UDATA),
                (DW_AT_CALL_COLUMN, DW_FORM_UDATA),
            ],
        );
    }
    b.push(0);
    b
}
