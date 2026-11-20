//! Reading a header as a C (or Objective-C) compiler for the target
//! would: the predefines, the include search, then the declarations.

use crate::Parser;
use crate::ty::Types;
use nabicc_pp::{Error, Pp, sysinc};
use nabicc_target::{Target, TargetInfo};
use std::path::Path;

/// What to read a header as.
pub struct Config<'a> {
    pub target: TargetInfo<'a>,
    /// Compiler options after `--` (`-I`, `-D`, `--sysroot`, ...).
    pub args: &'a [String],
    /// This machine's system headers serve the target.
    pub host: bool,
    /// Apple targets: the SDK.
    pub sdk: Option<&'a Path>,
    /// Read Objective-C (`-x objective-c`): its declarations and
    /// predefines.
    pub objc: bool,
}

/// Read `header` and its includes, then run `f` on the parsed unit.
pub fn header<T>(
    header: &Path,
    cfg: &Config<'_>,
    f: impl FnOnce(&mut Parser<'_, '_>) -> T,
) -> Result<T, Error> {
    let mut target = Target::new(&cfg.target).map_err(Error)?;
    target.objc = cfg.objc;
    let opts = sysinc::Opts::parse(cfg.args).map_err(Error)?;
    let dirs = sysinc::dirs(&target, &opts, cfg.host, cfg.sdk);
    let mut pp = Pp::new(&target, dirs);
    pp.run_text(&(target.predefines() + &opts.defines))?;
    pp.enter_main(header, &opts.include)?;
    let types = Types::new(target.clone());
    let mut p = Parser::new(&mut pp, types);
    if cfg.objc {
        p.objc_mode();
    }
    p.unit()?;
    Ok(f(&mut p))
}
