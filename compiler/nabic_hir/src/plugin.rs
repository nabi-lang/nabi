//! What a plugin, or a program that loads plugins, needs besides its code.
//!
//! The type keys of its `any` tags, its exports and its `any Tr`
//! dispatchers. Keys write each named type's path in braces
//! (`{game::Player}`); codegen puts the package hash there.

use nabic_ir_common::FunctionId;

/// How the module meets other modules at run time.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Mode {
    /// A program that loads no plugin: nothing changes.
    #[default]
    None,
    /// `--crate-type=plugin`: tags and GC type ids are the loader's.
    Plugin,
    /// A program that reaches `std::plugin`: keeps its compile-time tags,
    /// describes them, and its dispatchers ask the loaded modules.
    Host,
}

/// One function a plugin gives the process.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Export {
    pub id: FunctionId,
    /// `enemies::spawn`: what `Plugin::get` looks up.
    pub path: String,
    /// Its type's key, `fn({engine::World},i32)->List<..>`.
    pub sig: String,
    /// Its type as `type_name` writes it, for error messages.
    pub shown: String,
}

/// One `any Tr` dispatcher.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Dispatcher {
    pub id: FunctionId,
    /// `<trait path>.<method>`: equal in every module.
    pub method: String,
    /// The tags its arms take.
    pub tags: Vec<u32>,
}

/// What a hot-reload build (`--hot`) adds.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Hot {
    /// The hot packages: the plugin's own first, then `--hot-dep`s.
    pub packages: Vec<String>,
    /// The type key of each hot-package function's type.
    pub sigs: Vec<(FunctionId, String)>,
    /// Every struct and enum of a hot package the module uses.
    pub types: Vec<HotType>,
    /// The type key of each static's type, by static.
    pub statics: Vec<(String, String)>,
}

/// One struct or enum of a hot package.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HotType {
    pub ty: crate::HirTy,
    /// Its key, the path in braces.
    pub key: String,
    /// `game::Enemy`, instance arguments left out.
    pub path: String,
    /// As messages print it.
    pub name: String,
    pub span: nabic_diagnostic::Span,
    /// Its entry in `HirModule::type_descs`.
    pub desc: u32,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Info {
    /// `--hot`.
    pub hot: Option<Hot>,
    pub mode: Mode,
    /// The key of each `HirModule::any_tags` entry, in order.
    pub tag_keys: Vec<String>,
    /// A plugin's `pub` non-generic functions.
    pub exports: Vec<Export>,
    pub dispatchers: Vec<Dispatcher>,
}

/// What `Plugin::get::<F>` reads F's signature hash from: a `u64`
/// static codegen fills, named for the key so a replayed body still
/// says which.
const SIG: &str = "nabi.plugin.sig.";

#[must_use]
pub fn sig_static(key: &str) -> String {
    let mut s = String::from(SIG);
    for b in key.bytes() {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

/// The key a `sig_static` name stands for.
#[must_use]
pub fn sig_key(name: &str) -> Option<String> {
    let hex = name.strip_prefix(SIG)?;
    let bytes: Option<Vec<u8>> = (0..hex.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(hex.get(i..i + 2)?, 16).ok())
        .collect();
    String::from_utf8(bytes?).ok()
}
