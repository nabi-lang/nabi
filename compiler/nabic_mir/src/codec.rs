//! `Codec` for functions, for the optimized-MIR cache. Every enum is
//! also matched whole below: a variant added without its encoding stops
//! the build instead of making entries that decode wrong.

use crate::{
    BasicBlock, Bind, DebugTy, FnDbg, Inlined, LocalDecl, MirFunction, MirTy, Operand, RValue,
    Scope, Statement, Terminator, UnionKind,
};
use nabic_core::{codec_enum, codec_struct};

codec_struct!(MirFunction {
    id,
    def_id,
    name,
    params,
    locals,
    blocks,
    param_tys,
    ret_ty,
    span,
    facts,
    strips,
    pins,
    dbg
});
codec_struct!(FnDbg {
    binds,
    inlined,
    decls
});
codec_struct!(Bind {
    local,
    value,
    parts,
    line,
    scope
});
codec_struct!(Inlined {
    scope,
    parent,
    name,
    decl,
    call,
    params
});
codec_struct!(Scope { id, parent });
codec_struct!(LocalDecl { id, name, ty });
codec_struct!(BasicBlock {
    id,
    params,
    stmts,
    terminator
});
codec_enum!(Statement {
    Assign { target, rvalue },
    ArrayWrite { array, offset, index, value, elem_ty, len },
    StorageDead(l),
    BufStore { buf, index, value, elem_ty },
    Loc(span, scope),
    RawStore { addr, value, ty, volatile },
    HeapStore { ptr, ty, index, value },
    StaticStore { name, value, ty },
});
codec_enum!(RValue {
    Use(o),
    BinOp { op, op_ty, lhs, rhs },
    UnaryOp { op, op_ty, operand },
    Call { callee, args, ret_ty },
    StructInit { ty, fields },
    FieldRead { target, index, field_ty },
    Cast { source, from_ty, to_ty },
    Bitcast { source, from_ty, to_ty },
    WithField { source, struct_ty, index, value },
    ArrayInit { elem_ty, elements },
    ArrayIndex { array, offset, index, elem_ty, len },
    FnAddr { callee, fn_ty },
    StaticLoad { name, ty },
    HeapBox { value, ty },
    BufAlloc { elem_ty, len },
    RawLoad { addr, ty, volatile },
    AddrOf(l),
    StaticAddr(name),
    SizeOf { ty, align },
    BufLoad { buf, index, elem_ty },
    BufAtomic { op, buf, index, args, elem_ty, order },
    HeapLoad { ptr, ty },
    HeapAlloc { ty },
    HeapField { ptr, ty, index },
    CallIndirect { callee, args, arg_tys, ret_ty, abi },
});
codec_enum!(Operand { Const(c), Copy(l) });
codec_enum!(Terminator {
    Goto { target, args },
    If { cond, then_bb, then_args, else_bb, else_args },
    Return(v),
    Unreachable,
    Suspend { value, resume, is_final },
});
codec_enum!(UnionKind { Sum, Plain });
codec_enum!(MirTy {
    Unit,
    Prim(p),
    Any,
    Tuple(ts),
    Struct(id, fields),
    Array(e, n),
    Fn(ps, r),
    GcRef,
    Union(vs, words, kind),
});
codec_enum!(DebugTy {
    Mir(t),
    Sum(name, t, vs),
    Buf(name, e),
    Any,
    Boxed(name, t),
    Cell(t),
    Closure(name, t, env),
});

// The variants each `codec_enum!` above lists.
const _: fn(&Statement) = |s| match s {
    Statement::Assign { .. }
    | Statement::ArrayWrite { .. }
    | Statement::StorageDead(_)
    | Statement::BufStore { .. }
    | Statement::Loc(..)
    | Statement::RawStore { .. }
    | Statement::HeapStore { .. }
    | Statement::StaticStore { .. } => {}
};
const _: fn(&RValue) = |r| match r {
    RValue::Use(_)
    | RValue::BinOp { .. }
    | RValue::UnaryOp { .. }
    | RValue::Call { .. }
    | RValue::StructInit { .. }
    | RValue::FieldRead { .. }
    | RValue::Cast { .. }
    | RValue::Bitcast { .. }
    | RValue::WithField { .. }
    | RValue::ArrayInit { .. }
    | RValue::ArrayIndex { .. }
    | RValue::FnAddr { .. }
    | RValue::StaticLoad { .. }
    | RValue::HeapBox { .. }
    | RValue::BufAlloc { .. }
    | RValue::RawLoad { .. }
    | RValue::AddrOf(_)
    | RValue::StaticAddr(_)
    | RValue::SizeOf { .. }
    | RValue::BufLoad { .. }
    | RValue::BufAtomic { .. }
    | RValue::HeapLoad { .. }
    | RValue::HeapAlloc { .. }
    | RValue::HeapField { .. }
    | RValue::CallIndirect { .. } => {}
};
const _: fn(&Operand) = |o| match o {
    Operand::Const(_) | Operand::Copy(_) => {}
};
const _: fn(&Terminator) = |t| match t {
    Terminator::Goto { .. }
    | Terminator::If { .. }
    | Terminator::Return(_)
    | Terminator::Unreachable
    | Terminator::Suspend { .. } => {}
};
const _: fn(&UnionKind) = |k| match k {
    UnionKind::Sum | UnionKind::Plain => {}
};
const _: fn(&MirTy) = |t| match t {
    MirTy::Unit
    | MirTy::Prim(_)
    | MirTy::Any
    | MirTy::Tuple(_)
    | MirTy::Struct(..)
    | MirTy::Array(..)
    | MirTy::Fn(..)
    | MirTy::GcRef
    | MirTy::Union(..) => {}
};
const _: fn(&DebugTy) = |t| match t {
    DebugTy::Mir(_)
    | DebugTy::Sum(..)
    | DebugTy::Buf(..)
    | DebugTy::Any
    | DebugTy::Boxed(..)
    | DebugTy::Cell(_)
    | DebugTy::Closure(..) => {}
};
