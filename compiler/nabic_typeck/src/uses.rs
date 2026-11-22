//! `use` declarations flattened into per-module `UseEntry` lists, read
//! when a bare name misses the local scopes and module ancestors.

use nabic_ast::{EnumVariantKind, ExternItem, ImplItem, Item, StructKind, UseTree, UseTreeKind};
use nabic_core::{HashMap, HashSet, Visibility};
use nabic_diagnostic::{Code, NabiDiagnostic, Span};

/// One resolved `use` entry. Paths are fully qualified (no `pkg::` /
/// `sup::` left), so lookup is plain concatenation.
#[derive(Clone, Debug)]
pub enum UseEntry {
    /// `use foo::bar [as baz];`: `local` resolves to `full`.
    Single { local: String, full: String },
    /// `use foo::*;`: bare `name` is tried as `prefix::name`.
    Glob { prefix: String },
    /// `use foo::**;`: bare `name` is tried as `prefix::name` and in every
    /// descendant of `prefix`.
    GlobDeep { prefix: String },
}

/// Per-module use list, keyed by the module path the `use` appeared in.
/// Empty key = root module.
pub type UseTable = HashMap<String, Vec<UseEntry>>;

/// `pub use mod_path::name as alias;`, installed as an fn-table alias
/// at `module_path::short_name`.
#[derive(Clone, Debug)]
pub struct ReExport {
    pub module_path: String,
    pub short_name: String,
    pub full_path: String,
    pub vis: Visibility,
}

/// Per-module use table and re-export list from `(module, vis, UseTree)`.
///
/// A `pub use` gives both an entry and a re-export. A short name imported
/// from two paths keeps its first entry; `check_use_clashes` reports it
/// once the re-exports are known.
#[must_use]
pub fn build_use_table(
    raw: &[(String, Visibility, UseTree)],
    packages: &[String],
    shadows: &[String],
) -> (UseTable, Vec<ReExport>) {
    crate::lower::set_packages(packages.to_vec());
    crate::lower::set_shadows(shadows.to_vec());
    let mut table: UseTable = HashMap::default();
    let mut reexports: Vec<ReExport> = Vec::new();
    for (module_path, vis, tree) in raw {
        let mut entries = Vec::new();
        let existing = table.entry(module_path.clone()).or_default();
        flatten_tree(tree, None, module_path, existing, &mut entries);
        // The same full path imported twice is fine.
        let existing = table.entry(module_path.clone()).or_default();
        for (entry, _) in entries {
            if let UseEntry::Single { local, full } = &entry {
                if *vis != Visibility::Private {
                    reexports.push(ReExport {
                        module_path: module_path.clone(),
                        short_name: local.clone(),
                        full_path: full.clone(),
                        vis: *vis,
                    });
                }
                // A second target is kept: a module and a value may share
                // the name, and lookup tries each in its namespace.
                let taken = existing.iter().any(
                    |e| matches!(e, UseEntry::Single { local: l, full: f } if l == local && f == full),
                );
                if taken {
                    continue;
                }
            }
            existing.push(entry);
        }
    }
    (table, reexports)
}

/// A short name imported twice in one module whose paths reach different
/// items. Two paths to one item (through a `pub use`) are one import.
#[must_use]
pub fn check_use_clashes(
    raw: &[(String, Visibility, UseTree)],
    table: &UseTable,
    reexports: &[ReExport],
    is_value: &dyn Fn(&str) -> bool,
) -> Vec<NabiDiagnostic> {
    let mut diags = Vec::new();
    // Values are a namespace of their own, as where items are declared: a
    // name may import a module or type and a function (`channel`).
    let mut first: HashMap<(&str, String, bool), String> = HashMap::default();
    let ends = Ends::of(reexports);
    for (module_path, _, tree) in raw {
        let earlier = table.get(module_path).map_or(&[][..], Vec::as_slice);
        let mut leaves = Vec::new();
        flatten_tree(tree, None, module_path, earlier, &mut leaves);
        for (entry, span) in leaves {
            let UseEntry::Single { local, full } = entry else {
                continue;
            };
            let value = is_value(&ends.end(reexports, &full));
            let prev = first
                .entry((module_path.as_str(), local.clone(), value))
                .or_insert_with(|| full.clone());
            if *prev == full || ends.end(reexports, prev) == ends.end(reexports, &full) {
                continue;
            }
            let module = if module_path.is_empty() {
                "<root>"
            } else {
                module_path
            };
            diags.push(
                NabiDiagnostic::error(
                    Code::IMPORT_CONFLICT,
                    span,
                    format!(
                        "name `{local}` imported twice with different targets: `{prev}` and \
                         `{full}` in module `{module}`"
                    ),
                )
                .with_help(format!(
                    "alias one: `use {full} as {};`",
                    alias_for(&full, &local)
                )),
            );
        }
    }
    diags
}

/// `Module::name` keyed as the fn table spells it.
fn reexport_key(module: &str, name: &str) -> String {
    if module.is_empty() {
        name.to_owned()
    } else {
        format!("{module}::{name}")
    }
}

/// Re-exports by alias key (the first of each) and by `(module, name)`,
/// kept in step with a growing list.
#[derive(Default)]
struct Ends {
    by_key: HashMap<String, usize>,
    by_part: HashMap<(String, String), usize>,
}

impl Ends {
    fn of(out: &[ReExport]) -> Self {
        let mut e = Self::default();
        for (n, r) in out.iter().enumerate() {
            e.add(n, &r.module_path, &r.short_name);
        }
        e
    }

    fn add(&mut self, n: usize, module: &str, name: &str) {
        self.by_key.entry(reexport_key(module, name)).or_insert(n);
        self.by_part
            .entry((module.to_owned(), name.to_owned()))
            .or_insert(n);
    }

    /// The first re-export installed at `module::name`.
    fn at<'a>(&self, out: &'a [ReExport], module: &str, name: &str) -> Option<&'a ReExport> {
        self.by_part
            .get(&(module.to_owned(), name.to_owned()))
            .map(|&n| &out[n])
    }

    /// The item `path` names once re-exports are followed.
    fn end(&self, out: &[ReExport], path: &str) -> String {
        let mut path = path.to_owned();
        for _ in 0..out.len() {
            match self.by_key.get(&path) {
                Some(&n) => path.clone_from(&out[n].full_path),
                None => break,
            }
        }
        path
    }
}

/// A name for `use full as ..;` that keeps both: `bar::Item` gives
/// `BarItem`, `bar::f` gives `bar_f`.
fn alias_for(full: &str, local: &str) -> String {
    let parent = full
        .rsplit("::")
        .nth(1)
        .filter(|m| !m.is_empty())
        .unwrap_or("other");
    if local.starts_with(char::is_uppercase) {
        let camel: String = parent
            .split('_')
            .map(|w| {
                let mut c = w.chars();
                c.next()
                    .map(|f| f.to_uppercase().chain(c).collect::<String>())
                    .unwrap_or_default()
            })
            .collect();
        format!("{camel}{local}")
    } else {
        format!("{parent}_{local}")
    }
}

/// `reexports` plus one per item a `pub use p::*` / `p::**` reaches,
/// through re-exported modules too. Items the module can't see are left
/// out; narrower ones keep their reach. A name that already means
/// something else in the module is an error.
#[must_use]
pub fn glob_reexports(
    raw: &[(String, Visibility, UseTree)],
    items: &[&nabic_ast::Item],
    table: &UseTable,
    reexports: &[ReExport],
) -> (Vec<ReExport>, Vec<NabiDiagnostic>) {
    struct Glob {
        module: String,
        vis: Visibility,
        prefix: String,
        deep: bool,
        span: Span,
        /// The modules it reads through: a module re-exported into
        /// itself is a cycle, not expanded.
        chain: Vec<String>,
    }
    let mut globs = Vec::new();
    for (module, vis, tree) in raw.iter().filter(|(_, v, _)| *v != Visibility::Private) {
        let earlier = table.get(module).map_or(&[][..], Vec::as_slice);
        let mut leaves = Vec::new();
        flatten_tree(tree, None, module, earlier, &mut leaves);
        for (entry, span) in leaves {
            let (prefix, deep) = match entry {
                UseEntry::Glob { prefix } => (prefix, false),
                UseEntry::GlobDeep { prefix } => (prefix, true),
                UseEntry::Single { .. } => continue,
            };
            globs.push(Glob {
                chain: vec![module.clone(), prefix.clone()],
                module: module.clone(),
                vis: *vis,
                prefix,
                deep,
                span,
            });
        }
    }
    let mut out = reexports.to_vec();
    let mut diags = Vec::new();
    let mods: HashMap<&str, Visibility> = items
        .iter()
        .filter_map(|i| match *i {
            Item::Mod(m) => Some((m.name.as_str(), m.vis)),
            _ => None,
        })
        .collect();
    if globs.is_empty() && !out.iter().any(|r| mods.contains_key(r.full_path.as_str())) {
        return (out, diags);
    }
    let own: HashMap<&str, Visibility> = items
        .iter()
        .filter_map(|i| match *i {
            Item::Fn(f) => Some((f.name.as_str(), f.vis)),
            Item::Struct(s) => Some((s.name.as_str(), s.vis)),
            Item::Union(u) => Some((u.name.as_str(), u.vis)),
            Item::Trait(t) => Some((t.name.as_str(), t.vis)),
            Item::Const(c) => Some((c.name.as_str(), c.vis)),
            Item::Static(s) => Some((s.name.as_str(), s.vis)),
            Item::TypeAlias(a) => Some((a.name.as_str(), a.vis)),
            Item::Enum(e) => Some((e.name.as_str(), e.vis)),
            Item::Mod(m) => Some((m.name.as_str(), m.vis)),
            _ => None,
        })
        .collect();
    let key = reexport_key;
    // Re-exports chain to an item: two paths to it are one name.
    let mut ends = Ends::of(&out);
    // The modules each re-export was read through; a named one starts
    // at its module.
    let mut chains: HashMap<String, Vec<String>> = out
        .iter()
        .map(|r| {
            (
                key(&r.module_path, &r.short_name),
                vec![r.module_path.clone()],
            )
        })
        .collect();
    let mut expanded: HashSet<String> = HashSet::default();
    let mut reported = HashSet::default();
    // To a fixed point: a glob may reach what another glob re-exported.
    loop {
        let mut added = false;
        // A re-exported module reads as `pub use <module>::*` inside
        // the alias.
        for r in &out {
            let alias = key(&r.module_path, &r.short_name);
            let target = ends.end(&out, &r.full_path);
            if !mods.contains_key(target.as_str()) || !expanded.insert(alias.clone()) {
                continue;
            }
            let mut chain = chains.get(&alias).cloned().unwrap_or_default();
            if chain.contains(&target) {
                continue;
            }
            chain.push(target.clone());
            globs.push(Glob {
                module: alias,
                vis: r.vis,
                prefix: target,
                deep: false,
                span: Span::default(),
                chain,
            });
        }
        for g in &globs {
            let (module, vis, prefix, deep, span) =
                (&g.module, &g.vis, &g.prefix, &g.deep, &g.span);
            let under = |path: &str| {
                let parent = path.rsplit_once("::").map_or("", |(m, _)| m);
                // `pkg::**` of the package being built stays in it.
                let below = if prefix.is_empty() {
                    let first = parent.split("::").next().unwrap_or("");
                    !parent.is_empty()
                        && !nabic_core::TIERS.contains(&first)
                        && !crate::lower::is_dependency(first)
                } else {
                    parent.starts_with(&format!("{prefix}::"))
                };
                parent == prefix || (*deep && below)
            };
            let reached: Vec<(String, Visibility)> = own
                .iter()
                .map(|(n, v)| ((*n).to_owned(), *v))
                .chain(
                    out.iter()
                        .map(|r| (key(&r.module_path, &r.short_name), r.vis)),
                )
                .filter(|(n, _)| under(n))
                .collect();
            for (path, item_vis) in reached {
                let (parent, short) = path.rsplit_once("::").unwrap_or(("", &path));
                let alias = key(module, short);
                if alias == path || !crate::lower::visible_from(module, parent, item_vis) {
                    continue;
                }
                let target = ends.end(&out, &path);
                let clash = if own.contains_key(alias.as_str()) {
                    Some(alias.clone())
                } else {
                    ends.at(&out, module, short)
                        .map(|r| ends.end(&out, &r.full_path))
                        .filter(|t| *t != target)
                };
                match clash {
                    Some(other) => {
                        if reported.insert(alias.clone()) {
                            diags.push(NabiDiagnostic::error(
                                Code::IMPORT_CONFLICT,
                                *span,
                                format!(
                                    "glob re-export of `{target}` as `{alias}` conflicts with \
                                     `{other}`"
                                ),
                            ));
                        }
                    }
                    None if ends.at(&out, module, short).is_some() => {}
                    None => {
                        let rank = crate::lower::visibility_rank;
                        ends.add(out.len(), module, short);
                        out.push(ReExport {
                            module_path: (*module).clone(),
                            short_name: short.to_owned(),
                            full_path: path.clone(),
                            vis: if rank(item_vis) < rank(*vis) {
                                item_vis
                            } else {
                                *vis
                            },
                        });
                        chains.insert(alias, g.chain.clone());
                        added = true;
                    }
                }
            }
        }
        if !added {
            return (out, diags);
        }
    }
}

/// How the user's code gets a package it lacks: as a script, or as a
/// package (its dependencies by top-level name).
pub struct Missing<'a> {
    pub script: Option<&'a crate::Script>,
    pub packages: &'a [String],
}

impl Missing<'_> {
    /// Help for `use <root>::..` when `root` names nothing (none inside a
    /// dependency); a script gets a note when its environment has `root`.
    fn help(&self, root: &str, module: &str, path: &str) -> Option<(String, Option<String>)> {
        let first = module.split("::").next().unwrap_or_default();
        if self.packages.iter().any(|p| p == first) {
            return None;
        }
        // Another language's roots.
        let other = match root {
            "crate" => Some("this package's root is `pkg`: write `use pkg::..`".to_owned()),
            "super" => {
                let written = path
                    .strip_prefix(module)
                    .map_or(path, |r| r.trim_start_matches("::"));
                let segs: Vec<&str> = written.split("::").collect();
                Some(format!(
                    "write the path from the package root: `use {};`",
                    crate::lower::up_path(module, &segs)
                ))
            }
            _ => None,
        };
        if let Some(h) = other {
            return Some((h, None));
        }
        let Some(script) = self.script else {
            let help = format!(
                "`{root}` is not a dependency of this package: add it to nabi.nabion (`nabi add \
                 {root}`)"
            );
            return Some((help, None));
        };
        let help = match &script.file {
            Some(file) => format!("run `nabi add {root} {file}`"),
            None => format!(
                "`:add {root}` in the REPL; a script file gets it with `nabi add {root} <file>`"
            ),
        };
        let note = script
            .env
            .as_ref()
            .filter(|(_, libs)| libs.iter().any(|l| l.replace('-', "_") == root))
            .map(|(env, _)| {
                format!(
                    "`{root}` is installed in environment `{env}`, but a script uses only the \
                     libraries of its own `//! nabion` block (installed libraries are for `nabi \
                     repl` / `nabi jupyter`)"
                )
            });
        Some((help, note))
    }
}

/// Help for a path from `root` in `module` when `root` is a package of
/// the build that the module's package does not depend on, and nothing
/// of the module takes the name.
fn foreign_package(root: &str, module: &str, known: &HashSet<String>) -> Option<String> {
    let local = qualified(module, root);
    let taken = known
        .iter()
        .any(|k| *k == local || k.starts_with(&format!("{local}::")));
    crate::lower::foreign_help(module, root).filter(|_| !taken)
}

/// Unresolved module-level imports, reported at the leaf, and each item
/// hiding a named import of its name, with its error.
///
/// A leaf names an item, variant, module or re-export; a path rooted at a
/// bare name may also start with what a glob or the implicit core glob
/// brings in (`use Result::*`).
#[must_use]
pub fn check_use_targets(
    raw: &[(String, Visibility, UseTree)],
    items: &[&nabic_ast::Item],
    table: &UseTable,
    reexports: &[ReExport],
    missing: Option<Missing<'_>>,
) -> (Vec<NabiDiagnostic>, Vec<(NabiDiagnostic, Span)>) {
    if raw.is_empty() {
        return (Vec::new(), Vec::new());
    }
    let mut known: HashSet<String> = HashSet::default();
    for item in items {
        match *item {
            Item::Fn(f) => drop(known.insert(f.name.clone())),
            Item::Struct(s) => drop(known.insert(s.name.clone())),
            Item::Union(u) => drop(known.insert(u.name.clone())),
            Item::Trait(t) => drop(known.insert(t.name.clone())),
            Item::Const(c) => drop(known.insert(c.name.clone())),
            Item::Static(s) => drop(known.insert(s.name.clone())),
            Item::TypeAlias(a) => drop(known.insert(a.name.clone())),
            Item::Enum(e) => {
                known.insert(e.name.clone());
                known.extend(e.variants.iter().map(|v| format!("{}::{}", e.name, v.name)));
            }
            // Methods carry only their module (`a::get`).
            Item::Impl(i) => known.extend(i.items.iter().filter_map(|m| match m {
                ImplItem::Method(f) => f.name.rsplit_once("::").map(|(m, _)| m.to_owned()),
                _ => None,
            })),
            Item::Extern(b) => known.extend(b.items.iter().map(|x| match x {
                ExternItem::Fn(f) => f.name.clone(),
                ExternItem::Static(s) => s.name.clone(),
            })),
            Item::Mod(_) | Item::Use(_) => {}
        }
    }
    known.extend(raw.iter().map(|(m, _, _)| m.clone()));
    known.extend(
        reexports
            .iter()
            .map(|r| qualified(&r.module_path, &r.short_name)),
    );
    // Every ancestor of a known path is a module (or an enum).
    let modules: Vec<String> = known
        .iter()
        .flat_map(|k| k.match_indices("::").map(|(i, _)| k[..i].to_owned()))
        .collect();
    known.extend(modules);
    let mod_vis = crate::lower::module_visibility(items.iter().copied());
    let ends = Ends::of(reexports);
    let ended: Vec<(String, String)> = (reexports.iter())
        .filter(|r| r.vis == Visibility::Pub)
        .map(|r| {
            let key = qualified(&r.module_path, &r.short_name);
            let end = ends.end(reexports, &key);
            (key, end)
        })
        .collect();
    let visible = crate::lower::visible_paths(
        ended.iter().map(|(a, e)| (a.as_str(), e.as_str())),
        &mod_vis,
    );
    let item_vis: HashMap<&str, Visibility> = items
        .iter()
        .filter_map(|i| match *i {
            Item::Fn(f) => Some((f.name.as_str(), f.vis)),
            Item::Struct(s) => Some((s.name.as_str(), s.vis)),
            Item::Union(u) => Some((u.name.as_str(), u.vis)),
            Item::Trait(t) => Some((t.name.as_str(), t.vis)),
            Item::Const(c) => Some((c.name.as_str(), c.vis)),
            Item::Static(s) => Some((s.name.as_str(), s.vis)),
            Item::TypeAlias(a) => Some((a.name.as_str(), a.vis)),
            Item::Enum(e) => Some((e.name.as_str(), e.vis)),
            Item::Mod(m) => Some((m.name.as_str(), m.vis)),
            _ => None,
        })
        .collect();
    let reexported: HashMap<String, &str> = reexports
        .iter()
        .map(|r| {
            (
                qualified(&r.module_path, &r.short_name),
                r.full_path.as_str(),
            )
        })
        .collect();
    let names = namespaces(items);
    let mut hiding = Vec::new();
    let mut diags = Vec::new();
    for (module_path, use_vis, tree) in raw {
        let mut written = HashMap::default();
        written_leaves(tree, &mut Vec::new(), &mut written);
        let bare = tree.prefix.segments.first().is_some_and(|s| {
            crate::lower::root_path(&[s.name.as_str(), ""], module_path).is_none()
        });
        let earlier = table.get(module_path).map_or(&[][..], Vec::as_slice);
        let mut leaves = Vec::new();
        flatten_tree(tree, None, module_path, earlier, &mut leaves);
        for (entry, span) in leaves {
            let path = match &entry {
                UseEntry::Single { full, .. } => full,
                UseEntry::Glob { prefix } | UseEntry::GlobDeep { prefix } => prefix,
            };
            if let UseEntry::Single { local, full } = &entry {
                let me = qualified(module_path, local);
                if let Some(hidden) =
                    shadowed_import(&names, &reexported, &me, local, full, span, &written)
                {
                    hiding.push(hidden);
                    continue;
                }
                // Re-exports that lead back to themselves.
                if *use_vis != Visibility::Private {
                    let mut at = full.as_str();
                    let mut chain = vec![me.as_str()];
                    while let Some(&next) = reexported
                        .get(at)
                        .filter(|_| chain.len() <= reexported.len())
                    {
                        chain.push(at);
                        if at == me {
                            break;
                        }
                        at = next;
                    }
                    if chain.len() > 1 && chain.last() == Some(&me.as_str()) {
                        diags.push(NabiDiagnostic::error(
                            Code::INVALID_REEXPORT,
                            span,
                            format!("circular re-export: {}", {
                                let ring: Vec<String> =
                                    chain.iter().map(|c| format!("`{c}`")).collect();
                                ring.join(" -> ")
                            }),
                        ));
                        continue;
                    }
                }
                if let Some(m) = crate::lower::hidden_module(&mod_vis, module_path, full) {
                    diags.push(NabiDiagnostic::error(
                        Code::PRIVATE_ITEM,
                        span,
                        crate::lower::hidden_module_msg(
                            full,
                            &m,
                            module_path,
                            Some(visible.get(full).map_or(full.as_str(), String::as_str)),
                        ),
                    ));
                    continue;
                }
                // A named re-export reaches no further than its item.
                if let Some(&vis) = item_vis.get(full.as_str()) {
                    let rank = crate::lower::visibility_rank;
                    if *use_vis != Visibility::Private && rank(*use_vis) > rank(vis) {
                        diags.push(NabiDiagnostic::error(
                            Code::INVALID_REEXPORT,
                            span,
                            format!(
                                "`{full}` is {} but re-exported as {} - a re-export can't \
                                 be wider than its item",
                                vis_word(vis),
                                vis_word(*use_vis)
                            ),
                        ));
                        continue;
                    }
                }
            }
            if path.is_empty() || known.contains(path) {
                continue;
            }
            // A bare root may also be a name this module sees through a glob of
            // its own or the implicit import.
            if bare {
                let written = path
                    .strip_prefix(module_path.as_str())
                    .and_then(|r| r.strip_prefix("::"))
                    .unwrap_or(path);
                let tail = format!("::{written}");
                let under = |prefix: &str| {
                    let prefix = format!("{prefix}::");
                    known
                        .iter()
                        .any(|k| k.starts_with(&prefix) && k.ends_with(&tail))
                };
                let implicit = crate::lower::implicit_use(module_path);
                let globbed = earlier.iter().chain(&implicit).any(|e| match e {
                    UseEntry::Glob { prefix } => {
                        known.contains(&format!("{prefix}{tail}"))
                            || through_module(&reexported, &known, &format!("{prefix}{tail}"))
                    }
                    UseEntry::GlobDeep { prefix } => under(prefix),
                    UseEntry::Single { .. } => false,
                });
                if globbed {
                    continue;
                }
            }
            // The prelude names std's OS Ext traits: a build of core
            // alone (`--bootstrap`) has no std to take them from.
            if module_path.as_str() == "core::prelude"
                && path.starts_with("std::")
                && !known.iter().any(|k| k.starts_with("std::"))
            {
                continue;
            }
            let mut d = NabiDiagnostic::error(
                Code::UNRESOLVED_IMPORT,
                unresolved_at(tree, span, path, &known),
                format!("unresolved import: `{path}` does not exist"),
            );
            let root = tree.prefix.segments.first().map(|s| s.name.as_str());
            let unknown = |r: &str| {
                let (head, inner) = (format!("{r}::"), format!("::{r}::"));
                !known.iter().any(|k| {
                    k == r
                        || k.starts_with(&head)
                        || k.contains(&inner)
                        || k.strip_suffix(r).is_some_and(|s| s.ends_with("::"))
                })
            };
            if path.starts_with("std::collections::")
                && !known.iter().any(|k| k.starts_with("std::collections::"))
            {
                d = d.with_help(
                    "`List`, `Dict`, `Set` and `Deque` are in the prelude: delete this `use`",
                );
            } else if let Some((tier, found)) = other_tier(path, &known) {
                d = d.with_help(format!("`{path}` is not in `{tier}`: write `use {found};`"));
            } else if let Some(help) = root
                .filter(|_| bare)
                .and_then(|r| foreign_package(r, module_path, &known))
            {
                d = d.with_help(help);
            } else if let Some((help, note)) = root
                .filter(|r| bare && unknown(r))
                .zip(missing.as_ref())
                .and_then(|(r, m)| m.help(r, module_path, path))
            {
                d = d.with_help(help);
                if let Some(n) = note {
                    d = d.with_note(n);
                }
            }
            diags.push(d);
        }
    }
    (diags, hiding)
}

/// `std::time::Instant` that another toolchain root has
/// (`core::time::Instant`): the written root and that path.
fn other_tier<'p>(path: &'p str, known: &HashSet<String>) -> Option<(&'p str, String)> {
    let (root, rest) = path.split_once("::")?;
    if !nabic_core::TIERS.contains(&root) {
        return None;
    }
    let found = nabic_core::TIERS
        .iter()
        .filter(|t| **t != root)
        .map(|t| format!("{t}::{rest}"))
        .find(|c| known.contains(c))?;
    Some((root, found))
}

/// The written segment of `path` (the leaf at `leaf` in `tree`) where it
/// stops naming anything; `leaf` when the segments cannot be matched up.
fn unresolved_at(tree: &UseTree, leaf: Span, path: &str, known: &HashSet<String>) -> Span {
    let Some(spans) = leaf_segments(tree, leaf, &mut Vec::new()) else {
        return leaf;
    };
    // Root resolution rewrites the head only, so the tails line up.
    let segs: Vec<&str> = path.split("::").collect();
    for (j, &s) in spans.iter().enumerate() {
        let Some(i) = (segs.len() + j).checked_sub(spans.len()) else {
            continue;
        };
        if !known.contains(&segs[..=i].join("::")) {
            return s;
        }
    }
    spans.last().copied().unwrap_or(leaf)
}

/// The spans of every written segment down to the leaf at `leaf`.
fn leaf_segments(tree: &UseTree, leaf: Span, above: &mut Vec<Span>) -> Option<Vec<Span>> {
    let len = above.len();
    above.extend(&tree.spans);
    let found = match &tree.kind {
        UseTreeKind::Group(ts) => ts.iter().find_map(|t| leaf_segments(t, leaf, above)),
        _ => (tree.prefix.span == leaf).then(|| above.clone()),
    };
    above.truncate(len);
    found
}

const VALUE: u8 = 1;
const TYPE: u8 = 2;

/// Each item's namespaces (`VALUE`, `TYPE`) and span, by full path; a
/// tuple or unit struct or variant is a value too.
fn namespaces(items: &[&nabic_ast::Item]) -> HashMap<String, (u8, Span)> {
    let mut out = HashMap::default();
    for item in items {
        match *item {
            Item::Fn(f) => drop(out.insert(f.name.clone(), (VALUE, f.span))),
            Item::Const(c) => drop(out.insert(c.name.clone(), (VALUE, c.span))),
            Item::Static(s) => drop(out.insert(s.name.clone(), (VALUE, s.span))),
            Item::Struct(s) => {
                let ns = match s.kind {
                    StructKind::Named(_) => TYPE,
                    StructKind::Tuple(_) | StructKind::Unit => TYPE | VALUE,
                };
                out.insert(s.name.clone(), (ns, s.span));
            }
            Item::Union(u) => drop(out.insert(u.name.clone(), (TYPE, u.span))),
            Item::Trait(t) => drop(out.insert(t.name.clone(), (TYPE, t.span))),
            Item::TypeAlias(a) => drop(out.insert(a.name.clone(), (TYPE, a.span))),
            Item::Enum(e) => {
                out.insert(e.name.clone(), (TYPE, e.span));
                for v in &e.variants {
                    let ns = match v.kind {
                        EnumVariantKind::Named(_) => TYPE,
                        _ => TYPE | VALUE,
                    };
                    out.insert(format!("{}::{}", e.name, v.name), (ns, v.span));
                }
            }
            Item::Extern(b) => {
                for x in &b.items {
                    let (name, at) = match x {
                        ExternItem::Fn(f) => (f.name.clone(), f.span),
                        ExternItem::Static(s) => (s.name.clone(), s.span),
                    };
                    out.insert(name, (VALUE, at));
                }
            }
            Item::Impl(_) | Item::Mod(_) | Item::Use(_) => {}
        }
    }
    out
}

/// `use <full>` binding `local` where the module defines an item of that
/// name in the same namespace: the item would win unseen, so it's an
/// error. Globs give way.
fn shadowed_import(
    names: &HashMap<String, (u8, Span)>,
    reexported: &HashMap<String, &str>,
    me: &str,
    local: &str,
    full: &str,
    span: Span,
    written: &HashMap<Span, String>,
) -> Option<(NabiDiagnostic, Span)> {
    let &(own, at) = names.get(me)?;
    let mut end = full;
    for _ in 0..=reexported.len() {
        match reexported.get(end) {
            Some(next) => end = next,
            None => break,
        }
    }
    if end == me {
        return None;
    }
    let &(theirs, _) = names.get(end)?;
    if own & theirs == 0 {
        return None;
    }
    let path = written.get(&span).map_or(full, String::as_str);
    let mut segs = path.rsplit("::");
    let leaf = segs.next().unwrap_or(local);
    let rename = match segs
        .next()
        .filter(|p| !matches!(*p, "pkg" | "sup" | "self"))
    {
        Some(parent) => alias(parent, leaf),
        None => format!("{leaf}2"),
    };
    Some((
        NabiDiagnostic::error(
            Code::IMPORT_CONFLICT,
            span,
            format!("`{local}` is both imported from `{full}` and defined in this module"),
        )
        .with_span(span, "imported here")
        .with_secondary(at, "defined here, which would hide the import")
        .with_help(format!(
            "import it under another name, `use {path} as {rename};`, or remove this `use` \
             if the module does not need it"
        )),
        at,
    ))
}

/// A name for `parent::leaf` in `leaf`'s case: `a_f`, `A_MAX`, `APoint`.
fn alias(parent: &str, leaf: &str) -> String {
    if leaf.starts_with(|c: char| c.is_lowercase() || c == '_') {
        format!("{}_{leaf}", parent.to_lowercase())
    } else if leaf.len() > 1
        && leaf
            .chars()
            .all(|c| c.is_uppercase() || c.is_ascii_digit() || c == '_')
    {
        format!("{}_{leaf}", parent.to_uppercase())
    } else {
        let camel: String = parent
            .split('_')
            .flat_map(|w| {
                let mut cs = w.chars();
                cs.next()
                    .map(|c| c.to_ascii_uppercase())
                    .into_iter()
                    .chain(cs)
            })
            .collect();
        format!("{camel}{leaf}")
    }
}

/// Each simple leaf of `tree` by its span, as the source writes its path.
fn written_leaves(tree: &UseTree, prefix: &mut Vec<String>, out: &mut HashMap<Span, String>) {
    let n = prefix.len();
    prefix.extend(tree.prefix.segments.iter().map(|s| s.name.clone()));
    match &tree.kind {
        UseTreeKind::Simple(_) => drop(out.insert(tree.prefix.span, prefix.join("::"))),
        UseTreeKind::Group(ts) => ts.iter().for_each(|t| written_leaves(t, prefix, out)),
        UseTreeKind::Glob | UseTreeKind::GlobRecursive => {}
    }
    prefix.truncate(n);
}

/// `module::name` (`name` at the root).
fn qualified(module: &str, name: &str) -> String {
    if module.is_empty() {
        name.to_owned()
    } else {
        format!("{module}::{name}")
    }
}

const fn vis_word(v: Visibility) -> &'static str {
    match v {
        Visibility::Pub => "`pub`",
        Visibility::Pkg => "`pub(pkg)`",
        Visibility::Sub => "`pub(sub)`",
        Visibility::Private => "private",
    }
}

/// Item keys a glob can reach (variants and re-exports too) -> (visibility,
/// its module, the item it ends at).
pub struct GlobNames(HashMap<String, (Visibility, String, String)>);

impl GlobNames {
    #[must_use]
    pub fn new(items: &[&nabic_ast::Item], reexports: &[ReExport]) -> Self {
        let mut out = HashMap::default();
        let mut variants = Vec::new();
        let module = |k: &str| {
            k.rsplit_once("::")
                .map_or(String::new(), |(m, _)| m.to_owned())
        };
        let mut put = |k: &str, vis| drop(out.insert(k.to_owned(), (vis, module(k), k.to_owned())));
        for item in items {
            match *item {
                Item::Fn(f) => put(&f.name, f.vis),
                Item::Struct(s) => put(&s.name, s.vis),
                Item::Union(u) => put(&u.name, u.vis),
                Item::Trait(t) => put(&t.name, t.vis),
                Item::Const(c) => put(&c.name, c.vis),
                Item::Static(s) => put(&s.name, s.vis),
                Item::TypeAlias(a) => put(&a.name, a.vis),
                Item::Mod(m) => put(&m.name, m.vis),
                Item::Enum(e) => {
                    put(&e.name, e.vis);
                    // A variant is as visible as its enum.
                    for v in &e.variants {
                        let k = format!("{}::{}", e.name, v.name);
                        variants.push((k, e.vis, module(&e.name)));
                    }
                }
                Item::Extern(b) => {
                    for x in &b.items {
                        match x {
                            ExternItem::Fn(f) => put(&f.name, f.vis),
                            ExternItem::Static(s) => put(&s.name, s.vis),
                        }
                    }
                }
                Item::Impl(_) | Item::Use(_) => {}
            }
        }
        for (k, vis, from) in variants {
            out.insert(k.clone(), (vis, from, k));
        }
        for r in reexports {
            out.insert(
                qualified(&r.module_path, &r.short_name),
                (r.vis, r.module_path.clone(), r.full_path.clone()),
            );
        }
        Self(out)
    }

    /// Names the enclosing blocks' `use p::*`s bring into the
    /// headers of an item lifted into `module` (`blocks` innermost first).
    /// Module items win, inner blocks beat outer; a name two globs of one
    /// block disagree on is left to the module.
    #[must_use]
    pub fn block_paths(
        &self,
        blocks: &[Vec<String>],
        module: &str,
        table: &UseTable,
    ) -> HashMap<String, Vec<String>> {
        let earlier = table.get(module).map_or(&[][..], Vec::as_slice);
        let end = |k: &str| {
            let mut k = k.to_owned();
            for _ in 0..8 {
                match self.0.get(&k) {
                    Some((_, _, next)) if *next != k => k.clone_from(next),
                    _ => break,
                }
            }
            k
        };
        let mut out: HashMap<String, Vec<String>> = HashMap::default();
        for globs in blocks {
            let mut here: HashMap<&str, (String, Vec<String>)> = HashMap::default();
            let mut ambiguous = HashSet::default();
            for written in globs {
                let segs: Vec<String> = written.split("::").map(str::to_owned).collect();
                let prefix = resolve_root(&segs, module, earlier).join("::");
                for (k, (vis, from, _)) in &self.0 {
                    let Some((parent, name)) = k.rsplit_once("::") else {
                        continue;
                    };
                    if parent != prefix
                        || !crate::lower::visible_from(module, from, *vis)
                        || out.contains_key(name)
                        || self.0.contains_key(&qualified(module, name))
                    {
                        continue;
                    }
                    let target = end(k);
                    match here.get(name) {
                        Some((t, _)) if *t != target => drop(ambiguous.insert(name)),
                        Some(_) => {}
                        None => {
                            let mut path = segs.clone();
                            path.push(name.to_owned());
                            here.insert(name, (target, path));
                        }
                    }
                }
            }
            for (name, (_, path)) in here {
                if !ambiguous.contains(name) {
                    out.insert(name.to_owned(), path);
                }
            }
        }
        out
    }
}

/// `path` exists once a re-exported module on it is followed
/// (`core::prelude::task::sleep` through `pub use core::task`).
fn through_module(reexported: &HashMap<String, &str>, known: &HashSet<String>, path: &str) -> bool {
    path.match_indices("::").any(|(i, _)| {
        reexported
            .get(&path[..i])
            .is_some_and(|m| known.contains(&format!("{m}{}", &path[i..])))
    })
}

/// `flatten_tree` for a function-local `use`, against the function's module.
pub fn flatten_tree_for_local(tree: &UseTree, module_path: &str, out: &mut Vec<UseEntry>) {
    let mut leaves = Vec::new();
    flatten_tree(tree, None, module_path, &[], &mut leaves);
    out.extend(leaves.into_iter().map(|(e, _)| e));
}

/// One `UseEntry` per leaf; `module_path` resolves `pkg::` / `sup::`
/// (lookup already walks ancestors, so no `self::`). `parent`: the
/// enclosing group's resolved prefix.
fn flatten_tree(
    tree: &UseTree,
    parent: Option<&[String]>,
    module_path: &str,
    earlier: &[UseEntry],
    out: &mut Vec<(UseEntry, Span)>,
) {
    let span = tree.prefix.span;
    let raw_segs: Vec<String> = tree
        .prefix
        .segments
        .iter()
        .map(|s| s.name.clone())
        .collect();
    // Only the path's first segment is a root; group members extend their
    // prefix. A resolved parent may be empty (`pkg::{..}` at the root), so
    // presence decides, not length.
    let prefix_segs: Vec<String> = match parent {
        None => resolve_root(&raw_segs, module_path, earlier),
        Some(p) => p.iter().cloned().chain(raw_segs).collect(),
    };

    match &tree.kind {
        UseTreeKind::Simple(alias) => {
            // `use a::b::c` -> local `c` (or alias) -> full `a::b::c`.
            let Some(leaf) = prefix_segs.last().cloned() else {
                return;
            };
            let full = prefix_segs.join("::");
            let local = alias.clone().unwrap_or(leaf);
            out.push((UseEntry::Single { local, full }, span));
        }
        UseTreeKind::Glob => {
            out.push((
                UseEntry::Glob {
                    prefix: prefix_segs.join("::"),
                },
                span,
            ));
        }
        UseTreeKind::GlobRecursive => {
            out.push((
                UseEntry::GlobDeep {
                    prefix: prefix_segs.join("::"),
                },
                span,
            ));
        }
        UseTreeKind::Group(children) => {
            for child in children {
                flatten_tree(child, Some(&prefix_segs), module_path, earlier, out);
            }
        }
    }
}

/// Convert `pkg::a::b` / `sup::a` into an absolute `module_path`-rooted
/// segment list. Bare-ident roots (`a::b`) are relative to the module.
fn resolve_root(segs: &[String], module_path: &str, earlier: &[UseEntry]) -> Vec<String> {
    if segs.is_empty() {
        return Vec::new();
    }
    let strs: Vec<&str> = segs.iter().map(String::as_str).collect();
    if let Some(rooted) = crate::lower::root_path(&strs, module_path) {
        return rooted;
    }
    // An import of an item inside child module `first` (`use norm::norm;`)
    // shows the module exists, and a path's root is a module: the import
    // is a value of the same name, not the root.
    let child = format!("{}::", qualified(module_path, &segs[0]));
    let through = |e: &UseEntry| match e {
        UseEntry::Single { local, full } if *local == segs[0] && !full.starts_with(&child) => {
            Some(full.clone())
        }
        _ => None,
    };
    // A child module named like a tier.
    if let Some(child) = crate::lower::shadowing(module_path, &segs[0]) {
        return child
            .split("::")
            .map(str::to_owned)
            .chain(segs[1..].iter().cloned())
            .collect();
    }
    // A dependency's name is a top-level name in every module of the
    // package that depends on it.
    if let Some(m) = crate::lower::dependency_in(module_path, &segs[0]) {
        return std::iter::once(m)
            .chain(segs[1..].iter().cloned())
            .collect();
    }
    match segs[0].as_str() {
        // A name an earlier `use` of this module brought in (`use a::b; use b::c;`).
        _ if earlier.iter().any(|e| through(e).is_some()) => {
            let full = earlier.iter().find_map(through).unwrap_or_default();
            full.split("::")
                .map(str::to_owned)
                .chain(segs[1..].iter().cloned())
                .collect()
        }
        // A standard library tier.
        first if nabic_core::TIERS.contains(&first) => segs.to_vec(),
        _ if module_path.is_empty() => segs.to_vec(),
        _ => module_path
            .split("::")
            .map(str::to_owned)
            .chain(segs.iter().cloned())
            .collect(),
    }
}

nabic_core::codec_enum!(UseEntry {
    Single { local, full },
    Glob { prefix },
    GlobDeep { prefix }
});
nabic_core::codec_struct!(ReExport {
    module_path,
    short_name,
    full_path,
    vis
});
