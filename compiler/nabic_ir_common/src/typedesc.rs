//! What reflection asks the code generator to describe: one entry per
//! type, built by HIR lowering and laid out by codegen (`TypeDesc` in
//! `nabir_abi`). `T` is the IR's type (HIR's, then MIR's).

use crate::{ConstVal, FunctionId};

/// One type to describe.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TypeDesc<T> {
    /// The static whose address is the descriptor.
    pub symbol: String,
    pub ty: T,
    /// `nabir_abi::typedesc::kind`.
    pub kind: u16,
    /// `nabir_abi::typedesc::flags`.
    pub flags: u32,
    /// The key with each named type's path in braces, `{game::Player}`:
    /// codegen writes `Y<pkg8>:game::Player` there.
    pub key: String,
    pub name: String,
    pub path: String,
    /// Its `any` tag.
    pub tag: Option<u32>,
    pub fields: Vec<FieldDesc>,
    pub variants: Vec<VariantDesc>,
    /// Entries of the types it is made of (instance arguments, sum
    /// members, tuple parts, element, key then value).
    pub generics: Vec<u32>,
    pub attrs: Vec<AttrDesc>,
    /// `get`, `set`, `build`, `new_default`.
    pub ops: [Option<FunctionId>; 4],
    /// `len`, `get`, `items`, `from` of a list or array.
    pub list: Option<[Option<FunctionId>; 4]>,
    /// `len`, `items`, `from` of a dict.
    pub dict: Option<[Option<FunctionId>; 3]>,
}

/// One field.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FieldDesc {
    pub name: String,
    pub renamed_from: String,
    /// Entry of its type.
    pub ty: u32,
    /// `nabir_abi::typedesc::field`.
    pub flags: u32,
    /// Its position among the declared fields.
    pub index: u32,
    /// Bytes from the start of the value when lowering fixed it (a
    /// variant's field); `u64::MAX` for a struct's, which codegen lays out.
    pub offset: u64,
    pub attrs: Vec<AttrDesc>,
    /// Hot builds: `fn() -> T` giving the field's default (not renamed
    /// by `TypeDesc::map`: it is called, not held as a value).
    pub init_default: Option<FunctionId>,
}

/// One enum variant.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VariantDesc {
    pub name: String,
    pub tag: u32,
    pub fields: Vec<FieldDesc>,
    pub attrs: Vec<AttrDesc>,
}

/// One runtime attribute: its struct's entry and the value of each of
/// its fields.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AttrDesc {
    pub ty: u32,
    pub values: Vec<ConstVal>,
}

impl<T> TypeDesc<T> {
    /// Every function it points at.
    pub fn functions(&self) -> impl Iterator<Item = FunctionId> + '_ {
        let fields = self
            .fields
            .iter()
            .chain(self.variants.iter().flat_map(|v| &v.fields))
            .map(|f| &f.init_default);
        self.ops
            .iter()
            .chain(self.list.iter().flatten())
            .chain(self.dict.iter().flatten())
            .chain(fields)
            .filter_map(|f| *f)
    }

    /// The same entry over another IR's type.
    pub fn map<U>(&self, ty: U, f: impl Fn(FunctionId) -> FunctionId) -> TypeDesc<U> {
        let g = |x: &Option<FunctionId>| x.map(&f);
        TypeDesc {
            symbol: self.symbol.clone(),
            ty,
            kind: self.kind,
            flags: self.flags,
            key: self.key.clone(),
            name: self.name.clone(),
            path: self.path.clone(),
            tag: self.tag,
            fields: self.fields.clone(),
            variants: self.variants.clone(),
            generics: self.generics.clone(),
            attrs: self.attrs.clone(),
            ops: [
                g(&self.ops[0]),
                g(&self.ops[1]),
                g(&self.ops[2]),
                g(&self.ops[3]),
            ],
            list: self.list.map(|l| [g(&l[0]), g(&l[1]), g(&l[2]), g(&l[3])]),
            dict: self.dict.map(|d| [g(&d[0]), g(&d[1]), g(&d[2])]),
        }
    }
}
