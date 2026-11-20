//! Checks of names and feature tables.

#[allow(clippy::wildcard_imports)]
use crate::manifest::*;

/// A feature name: `[a-z0-9][a-z0-9_-]*`.
pub fn check_feature(name: &str) -> Result<(), String> {
    let ok = name.starts_with(|c: char| c.is_ascii_lowercase() || c.is_ascii_digit())
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_');
    if ok {
        Ok(())
    } else {
        Err(format!(
            "`{name}` is not a valid feature name: lowercase ASCII letters, digits, `-` and `_`, \
             starting with a letter or digit"
        ))
    }
}

/// Where a feature table is wrong.
#[derive(Debug, PartialEq, Eq)]
pub enum At {
    /// The feature's name.
    Feature(usize),
    /// An entry of its list.
    Entry(usize, usize),
    /// A dependency (by index in `deps`).
    Dep(usize),
}

/// Check names and references, that no feature reaches itself, and that
/// some feature enables each optional dependency.
pub fn check_features(features: &Features, deps: &[Dep]) -> Result<(), (At, String)> {
    // 0 new, 1 on the path, 2 done.
    fn visit(
        i: usize,
        features: &Features,
        state: &mut [u8],
        path: &mut Vec<usize>,
    ) -> Result<(), (At, String)> {
        state[i] = 1;
        path.push(i);
        for (j, e) in features[i].1.iter().enumerate() {
            let Enables::Feature(f) = Enables::parse(e) else {
                continue;
            };
            let Some(k) = features.iter().position(|(n, _)| n == f) else {
                continue;
            };
            if state[k] == 1 {
                let from = path.iter().position(|&p| p == k).unwrap_or(0);
                let names: Vec<&str> = path[from..]
                    .iter()
                    .chain([&k])
                    .map(|&p| features[p].0.as_str())
                    .collect();
                return Err((
                    At::Entry(i, j),
                    format!("feature cycle: {}", names.join(" -> ")),
                ));
            }
            if state[k] == 0 {
                visit(k, features, state, path)?;
            }
        }
        path.pop();
        state[i] = 2;
        Ok(())
    }
    let find = |d: &str| deps.iter().position(|x| same(&x.name, d));
    for (i, (name, list)) in features.iter().enumerate() {
        check_feature(name).map_err(|e| (At::Feature(i), e))?;
        for (j, e) in list.iter().enumerate() {
            let at = || At::Entry(i, j);
            match Enables::parse(e) {
                Enables::Feature(f) => {
                    check_feature(f).map_err(|m| (at(), m))?;
                    if !features.iter().any(|(n, _)| n == f) {
                        return Err((at(), format!("`{f}` is not a feature of this package")));
                    }
                }
                Enables::Dep(d) => match find(d) {
                    Some(k) if deps[k].optional => {}
                    Some(_) => {
                        return Err((
                            at(),
                            format!("`{e}`: dependency `{d}` is not `optional: true`"),
                        ));
                    }
                    None => return Err((at(), format!("`{e}`: there is no dependency `{d}`"))),
                },
                Enables::DepFeature { dep, feature, weak } => {
                    check_feature(feature).map_err(|m| (at(), format!("`{e}`: {m}")))?;
                    match find(dep) {
                        Some(k) if weak && !deps[k].optional => {
                            return Err((
                                at(),
                                format!(
                                    "`{e}`: `?` is for an optional dependency, and `{dep}` is not"
                                ),
                            ));
                        }
                        Some(_) => {}
                        None => {
                            return Err((at(), format!("`{e}`: there is no dependency `{dep}`")));
                        }
                    }
                }
            }
        }
    }
    let mut state = vec![0; features.len()];
    for i in 0..features.len() {
        if state[i] == 0 {
            visit(i, features, &mut state, &mut Vec::new())?;
        }
    }
    for (k, d) in deps.iter().enumerate().filter(|(_, d)| d.optional) {
        let enabled = features
            .iter()
            .flat_map(|(_, l)| l)
            .any(|e| match Enables::parse(e) {
                Enables::Dep(x)
                | Enables::DepFeature {
                    dep: x,
                    weak: false,
                    ..
                } => same(x, &d.name),
                _ => false,
            });
        if !enabled {
            return Err((
                At::Dep(k),
                format!(
                    "optional dependency `{}` is not enabled by any feature (add \"dep:{}\" to \
                     one)",
                    d.name, d.name
                ),
            ));
        }
    }
    Ok(())
}

/// A package name: `[a-z][a-z0-9_-]*`, a letter first since code spells
/// `-` as `_` (`code_name`).
pub fn check_name(name: &str) -> Result<(), String> {
    let valid = |n: &str| {
        n.starts_with(|c: char| c.is_ascii_lowercase())
            && n.chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
    };
    if let Some(e) = nabic_core::reserved_package(&name.replace('-', "_")) {
        return Err(e);
    }
    if valid(name) {
        return Ok(());
    }
    let fixed: String = name
        .to_ascii_lowercase()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect();
    Err(format!(
        "`{name}` is not a valid package name: lowercase ASCII letters, digits, `-` and `_`, \
         starting with a letter{}",
        if valid(&fixed) {
            format!("; try `{fixed}`")
        } else {
            String::new()
        }
    ))
}

/// `check_name`, and a name every OS can store (`nabic_core::check_os_name`):
/// what `nabi new`, `init`, `add` and `publish` require. An existing
/// manifest only warns (`parse`).
pub fn check_new_name(name: &str) -> Result<(), String> {
    check_name(name)?;
    nabic_core::check_os_name("package", name)
}

/// The name code uses for package `name` (`use`, `--extern`): `-` as `_`.
pub fn code_name(name: &str) -> String {
    name.replace('-', "_")
}

/// Identity: names equal up to case and `-` / `_` are one package.
pub fn key(name: &str) -> String {
    code_name(&name.to_ascii_lowercase())
}

pub fn same(a: &str, b: &str) -> bool {
    key(a) == key(b)
}
