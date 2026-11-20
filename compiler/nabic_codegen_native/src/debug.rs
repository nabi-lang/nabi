//! `-g`: debug info, DWARF (`dwarf`) and `CodeView` (`codeview`), and
//! where variables are in optimized code (`locs`).

pub(crate) mod codeview;
pub(crate) mod dwarf;
pub(crate) mod locs;
