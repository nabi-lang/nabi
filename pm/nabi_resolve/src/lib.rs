//! Dependencies: resolution to one package per name, the registry index
//! and its source archives, the git cache, native packages, and the
//! official packages a toolchain carries.

pub mod archive;
pub mod bundle;
pub mod git;
pub mod native;
pub mod registry;
pub mod resolve;

use nabi_manifest::{VERSION, lock, manifest, platform, semver, skipped, warn_once, workspace};
use nabi_toolchain::toolchain;
