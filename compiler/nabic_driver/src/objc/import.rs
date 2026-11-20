//! `ffi::objc::import { "AppKit/AppKit.h", .. }`: the headers' bindings
//! as `extern "objc"` blocks, read by the Objective-C front end and kept
//! in the toolchain's cache per SDK, so a second build reads one file.

use crate::objc::decl::err;
use crate::{MetaCtx, ModItem, UseEntry};
use nabic_ast::Item;
use nabic_diagnostic::{NabiDiagnostic, SourceMap, Span};
use nabic_parser::ExtOps;
use std::path::{Path, PathBuf};

/// Bump when the bindings change for the same headers.
const FORMAT: u32 = 1;

/// The cache file of these bindings and what keys it.
fn key(target: &nabic_mir::TargetSpec, sdk: &Path, version: &str, headers: &[String]) -> u64 {
    let exe = nabic_core::current_exe()
        .and_then(|e| std::fs::metadata(e).ok())
        .map(|m| {
            let t = m
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map_or(0, |d| d.as_nanos());
            format!("{}:{t}", m.len())
        })
        .unwrap_or_default();
    let text = format!(
        "{FORMAT}|{exe}|{}|{}|{}|{version}|{}",
        target.name,
        target.c_triple(),
        sdk.display(),
        headers.join("\n")
    );
    nabic_core::hash::fnv(text.as_bytes())
}

/// The directory a header names: `Metal/Metal.h` is the Metal
/// framework's headers, `objc/runtime.h` under the SDK's `usr/include`,
/// a path relative to the importing file that file's.
fn root_of(sdk: &Path, src_dir: &Path, h: &str) -> Option<PathBuf> {
    let local = src_dir.join(h);
    if local.is_file() {
        return local.parent().map(Path::to_path_buf);
    }
    if let Some((fw, rest)) = h.split_once('/') {
        let d = sdk.join(format!("System/Library/Frameworks/{fw}.framework/Headers"));
        if d.join(rest).is_file() {
            return Some(d);
        }
    }
    let inc = sdk.join("usr/include").join(h);
    inc.is_file().then(|| inc.parent().map(Path::to_path_buf))?
}

/// The bindings of `headers` (imported from a file in `src_dir`): from
/// the cache, else generated and kept; the file they are in.
fn bindings(
    target: &nabic_mir::TargetSpec,
    headers: &[String],
    src_dir: &Path,
) -> Result<(PathBuf, String), String> {
    let sdk = nabic_target::apple::sdk_for(target)?;
    let version = nabic_target::apple::sdk_version(&sdk).unwrap_or_else(|| "unknown".to_owned());
    let k = key(target, &sdk, &version, headers);
    let dir = crate::cache_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join("objc")
        .join(&version);
    let file = dir.join(format!("{k:016x}.nabi"));
    if let Ok(s) = std::fs::read_to_string(&file) {
        return Ok((file, s));
    }
    let mut roots = Vec::new();
    let mut umbrella = String::new();
    for h in headers {
        let Some(r) = root_of(&sdk, src_dir, h) else {
            return Err(format!(
                "no header `{h}` in the SDK ({}) or beside the importing file: name a \
                 framework header as `Framework/Header.h` (`\"AppKit/AppKit.h\"`)",
                sdk.display()
            ));
        };
        let local = src_dir.join(h);
        if local.is_file() {
            umbrella.push_str(&format!("#import \"{}\"\n", local.display()));
        } else {
            umbrella.push_str(&format!("#import <{h}>\n"));
        }
        roots.push(r);
    }
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let h = dir.join(format!("{k:016x}.h"));
    std::fs::write(&h, &umbrella).map_err(|e| format!("{}: {e}", h.display()))?;
    let triple = target.c_triple();
    let opts = nabicc_bindgen::Options {
        header: &h,
        target: &target.name,
        info: nabicc_bindgen::TargetInfo {
            triple: &triple,
            arch: &target.arch,
            os: &target.os,
            env: &target.env,
            vendor: &target.vendor,
            pointer_width: target.pointer_width,
            big_endian: target.endian == "big",
        },
        stdcall: false,
        sdk: Some(sdk),
        clang_args: &[],
        shim: false,
        system_headers: true,
        objc: true,
        roots: &roots,
    };
    let out = nabicc_bindgen::generate(&opts)?;
    // Written whole, then renamed: a parallel build reads all or none.
    let tmp = dir.join(format!("{k:016x}.{}.tmp", std::process::id()));
    std::fs::write(&tmp, &out).map_err(|e| format!("{}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, &file).map_err(|e| format!("{}: {e}", file.display()))?;
    Ok((file, out))
}

/// Replace each `ffi::objc::import` of `items` with its bindings.
#[allow(clippy::too_many_arguments)] // the parse context
pub(crate) fn expand(
    items: &mut Vec<ModItem>,
    uses: &mut Vec<UseEntry>,
    target: &nabic_mir::TargetSpec,
    ext: &ExtOps,
    metas: &MetaCtx,
    diags: &mut Vec<NabiDiagnostic>,
    files: &mut SourceMap,
) {
    let mut imports: Vec<(String, Vec<String>, Span)> = Vec::new();
    items.retain(|(module, item)| {
        let Item::Extern(b) = item else { return true };
        let Some(h) = &b.headers else { return true };
        imports.push((module.clone(), h.clone(), b.span));
        false
    });
    for (module, headers, span) in imports {
        if target.vendor != "apple" {
            err(
                diags,
                span,
                format!(
                    "`ffi::objc::import` reads Objective-C headers, which exist only on Apple \
                     targets (macOS and iOS), not {}: put it and the code using it under \
                     `#[cfg(any(target_os = \"macos\", target_os = \"ios\"))]`",
                    target.name
                ),
            );
            continue;
        }
        let src_dir = files
            .path(span.file)
            .and_then(|p| Path::new(p).parent().map(Path::to_path_buf))
            .unwrap_or_default();
        match bindings(target, &headers, &src_dir) {
            Ok((file, source)) => {
                // A changed header source rebuilds through the key.
                crate::objc::parse_into(
                    &module,
                    &source,
                    Some(&file.to_string_lossy()),
                    items,
                    uses,
                    ext,
                    metas,
                    diags,
                    files,
                );
            }
            Err(e) => err(diags, span, format!("Objective-C headers: {e}")),
        }
    }
}
