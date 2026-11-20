//! Standard library intrinsics.

#[allow(clippy::wildcard_imports)]
use crate::lower::collect::*;

/// What a `#[intrinsic("nabi.NAME")]` function is: its
/// calls become the operation; the body is only a fallback.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Intrinsic {
    /// `nabi.OP.TYPE` on a primitive's method.
    Prim(PrimOp),
    /// `nabi.cast.FROM.TO` / `nabi.clone.copy`: the operand as the return type.
    Convert,
    BufNew,
    BufGet,
    BufSet,
    BufSame,
    BufErase,
    /// `nabi.buf_cast`: the reference as a `GcBuf` of the expected type.
    BufCast,
    PtrRead {
        volatile: bool,
    },
    PtrWrite {
        volatile: bool,
    },
    SizeOf,
    AlignOf,
    /// `nabi.addr_of`: a pointer (the declared return type's) to the
    /// argument's place, or to a `ref self` receiver's.
    AddrOf,
    /// `nabi.drop_in_place`: run the drop glue of the `T` a `Ptr<T>`
    /// points at.
    DropInPlace,
    /// `nabi.atomic.OP`: the op `HirExprKind::BufAtomic` carries.
    Atomic(&'static str),
    AnyCast,
    MethodTable,
    FieldTable,
    /// `nabi.type_desc`: the address of `T`'s descriptor.
    TypeDesc,
    /// `nabi.type_name`: `T`'s name as a `str` constant.
    TypeName,
    /// `nabi.shown`: the argument as `{x:?}`, or `<T>` without `Debug`.
    Shown,
    /// `nabi.transfer`: a hand-off copy of the value.
    Transfer,
    /// `nabi.transfer_keep`: note every collection reached as its own copy.
    TransferKeep,
    /// `nabi.transfer_visit`: note every collection reached for the memo's
    /// current owner (`par`'s distinct items).
    TransferVisit,
    /// `nabi.transfer_needed`: whether `nabi.transfer` copies anything.
    TransferNeeded,
    /// `nabi.transfer_plain`: whether a type holds no GC reference.
    TransferPlain,
    /// `nabi.transfer_flat`: one listed handle whose elements need no copy.
    TransferFlat,
    /// `nabi.hand_over`: the value itself when `nabi.transfer` would copy
    /// nothing, else a call of `marker::hand_over_copy`.
    HandOver,
    /// `nabi.plugin_get`: a code address as a function value of `F`.
    PluginGet,
    /// `nabi.plugin_sig`: the hash of `F`'s signature key.
    PluginSig,
}

impl Intrinsic {
    pub(crate) fn parse(name: &str) -> Option<Self> {
        let rest = name.strip_prefix("nabi.")?;
        Some(match rest {
            "buf_new" => Self::BufNew,
            "buf_get" => Self::BufGet,
            "buf_set" => Self::BufSet,
            "buf_same" => Self::BufSame,
            "buf_erase" => Self::BufErase,
            "buf_cast" => Self::BufCast,
            "clone.copy" => Self::Convert,
            "ptr.read" => Self::PtrRead { volatile: false },
            "ptr.write" => Self::PtrWrite { volatile: false },
            "ptr.read_volatile" => Self::PtrRead { volatile: true },
            "ptr.write_volatile" => Self::PtrWrite { volatile: true },
            "size_of" => Self::SizeOf,
            "align_of" => Self::AlignOf,
            "addr_of" => Self::AddrOf,
            "drop_in_place" => Self::DropInPlace,
            "any_cast" => Self::AnyCast,
            "method_table" => Self::MethodTable,
            "field_table" => Self::FieldTable,
            "type_desc" => Self::TypeDesc,
            "type_name" => Self::TypeName,
            "shown" => Self::Shown,
            "transfer" => Self::Transfer,
            "transfer_keep" => Self::TransferKeep,
            "transfer_visit" => Self::TransferVisit,
            "transfer_needed" => Self::TransferNeeded,
            "transfer_plain" => Self::TransferPlain,
            "transfer_flat" => Self::TransferFlat,
            "hand_over" => Self::HandOver,
            "plugin_get" => Self::PluginGet,
            "plugin_sig" => Self::PluginSig,
            _ => {
                if let Some(tys) = rest.strip_prefix("cast.") {
                    return (tys.split('.').count() == 2).then_some(Self::Convert);
                }
                if let Some(op) = rest.strip_prefix("atomic.") {
                    return [
                        "load", "store", "xchg", "add", "sub", "and", "or", "xor", "cas",
                    ]
                    .into_iter()
                    .find(|o| *o == op)
                    .map(Self::Atomic);
                }
                // `nabi.OP_assign.TYPE`: the op, written
                // back through the `ref self` receiver.
                let op = rest.split('.').next()?;
                Self::Prim(match op.strip_suffix("_assign").unwrap_or(op) {
                    "add" => PrimOp::Add,
                    "sub" => PrimOp::Sub,
                    "mul" => PrimOp::Mul,
                    "div" => PrimOp::Div,
                    "rem" => PrimOp::Rem,
                    "mod" => PrimOp::Mod,
                    "and" => PrimOp::BitAnd,
                    "or" => PrimOp::BitOr,
                    "xor" => PrimOp::BitXor,
                    "shl" => PrimOp::Shl,
                    "shr" => PrimOp::Shr,
                    "eq" => PrimOp::Eq,
                    "ne" => PrimOp::Ne,
                    "lt" => PrimOp::Lt,
                    "le" => PrimOp::Le,
                    "gt" => PrimOp::Gt,
                    "ge" => PrimOp::Ge,
                    "neg" => PrimOp::Neg,
                    // Bitwise on integers, logical on `bool` (picked at the call).
                    "not" => PrimOp::Not,
                    "sqrt" => PrimOp::Sqrt,
                    "wrapping_add" => PrimOp::WrappingAdd,
                    "wrapping_sub" => PrimOp::WrappingSub,
                    "wrapping_mul" => PrimOp::WrappingMul,
                    "ctpop" => PrimOp::CountOnes,
                    "ctlz" => PrimOp::LeadingZeros,
                    "cttz" => PrimOp::TrailingZeros,
                    "bswap" => PrimOp::SwapBytes,
                    _ => return None,
                })
            }
        })
    }
}

/// `m`'s `#[intrinsic(..)]`, standard library only.
pub(crate) fn intrinsic_of(
    m: &FnDef,
    module: &str,
    diags: &mut Vec<nabic_diagnostic::NabiDiagnostic>,
) -> Option<Intrinsic> {
    let attr = m.attrs.iter().find(|a| a.name() == "intrinsic")?;
    let mut err = |msg: String| {
        diags.push(nabic_diagnostic::NabiDiagnostic::error(
            Code::INVALID_INTRINSIC,
            m.span,
            msg,
        ));
        None
    };
    if !nabic_core::in_toolchain(module) {
        return err(format!(
            "`#[intrinsic]` on `{}`: the intrinsic attribute is for the standard library only",
            m.name
        ));
    }
    let name = match &attr.body {
        nabic_ast::AttrBody::Args(args) => args.iter().find_map(|a| match a {
            nabic_ast::AttrArg::Lit(nabic_ast::Literal::Str(s)) => Some(s.clone()),
            _ => None,
        }),
        _ => None,
    };
    // A `core::simd` op stays a call, which a back end may emit as vector
    // code (`FnFacts::simd`).
    if let Some(n) = name.as_deref().filter(|n| n.starts_with("nabi.simd.")) {
        if nabic_ir_common::SimdOp::parse(n).is_none() || m.generics.params.is_empty() {
            return err(format!(
                "`#[intrinsic(\"{n}\")]` on `{}`: unknown `nabi.simd` op, or not generic",
                m.name
            ));
        }
        return None;
    }
    let Some(intrinsic) = name.as_deref().and_then(Intrinsic::parse) else {
        return err(format!(
            "`#[intrinsic({})]` on `{}`: unknown intrinsic - expected `nabi.OP.TYPE`",
            name.map_or_else(String::new, |n| format!("\"{n}\"")),
            m.name
        ));
    };
    // A primitive op is a concrete method; the others take their types
    // from a generic function's arguments.
    if intrinsic == Intrinsic::Convert {
        if !m.generics.params.is_empty() || m.params.len() != 1 {
            return err(format!(
                "`#[intrinsic]` on `{}`: a conversion is a non-generic method of one operand",
                m.name
            ));
        }
        return Some(intrinsic);
    }
    let Intrinsic::Prim(op) = intrinsic else {
        if m.generics.params.is_empty() {
            return err(format!(
                "`#[intrinsic]` on `{}`: this intrinsic is a generic function",
                m.name
            ));
        }
        return Some(intrinsic);
    };
    if !m.generics.params.is_empty() {
        return err(format!(
            "`#[intrinsic]` on `{}`: a primitive op is not generic",
            m.name
        ));
    }
    let arity = if matches!(
        op,
        PrimOp::Neg
            | PrimOp::Not
            | PrimOp::Sqrt
            | PrimOp::CountOnes
            | PrimOp::LeadingZeros
            | PrimOp::TrailingZeros
            | PrimOp::SwapBytes
    ) {
        1
    } else {
        2
    };
    if m.params.len() != arity {
        return err(format!(
            "`#[intrinsic]` on `{}`: the op takes {arity} operand(s)",
            m.name
        ));
    }
    let assign = name
        .as_deref()
        .is_some_and(|n| n.split('.').nth(1).is_some_and(|o| o.ends_with("_assign")));
    if assign != (arity == 2 && m.params[0].by_ref) {
        return err(format!(
            "`#[intrinsic]` on `{}`: a `nabi.OP_assign.TYPE` op takes `ref self`, and only it",
            m.name
        ));
    }
    Some(intrinsic)
}
