//! Tools on the compiler front end: formatter, linter, doc generator, and
//! the completion and hover the language server (`nabils`) and the REPL
//! (`nabirepl`) share. Each takes its configuration as arguments, so any
//! CLI front can host them.

pub mod analysis;
pub mod complete;
pub mod cursor;
pub mod doc;
pub mod fmt;
pub mod hover;
pub mod lint;
pub mod members;
pub mod roots;
