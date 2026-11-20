//! Unused imports and dead code.

#[allow(clippy::wildcard_imports)]
use crate::lint::*;

impl Cx<'_> {
    pub(crate) fn unused_imports(&mut self, items: &[Item]) {
        // `use a::inner; use inner::f;` - a path's first segment may be an
        // earlier import.
        for item in items {
            if let Item::Use(u) = item {
                if let Some(first) = u.tree.prefix.segments.first() {
                    self.file_names.insert(first.name.clone());
                }
            }
        }
        for item in items {
            match item {
                Item::Use(u) if u.vis == Visibility::Private => {
                    let mut leaves = Vec::new();
                    use_leaves(&u.tree, &mut Vec::new(), &mut leaves);
                    let alone = leaves.len() == 1;
                    for (name, path, span) in leaves {
                        if !self.file_names.contains(&name) && !self.traits.contains(&name) {
                            let span = if span.is_dummy() { u.span } else { span };
                            let fix = alone.then(|| self.use_line(u.span)).flatten();
                            self.hit_fix(
                                "unused_imports",
                                span,
                                format!("unused import `{path}`"),
                                fix,
                            );
                        }
                    }
                }
                Item::Mod(m) => self.unused_imports(m.items.as_deref().unwrap_or(&[])),
                _ => {}
            }
        }
    }
}

impl Cx<'_> {
    /// The line of the `use` at `at`, deleted: only when the line holds
    /// that one `use` and nothing else, with no attribute above it.
    fn use_line(&self, at: Span) -> Option<(Span, String, String)> {
        let (_, text) = self
            .texts
            .get(usize::try_from(at.file).ok()?.checked_sub(1)?)?;
        let start = usize::try_from(at.start).ok()?;
        let line = text[..start].rfind('\n').map_or(0, |i| i + 1);
        if !text[line..start].trim().is_empty() || !text[start..].starts_with("use ") {
            return None;
        }
        let semi = start + text[start..].find(';')?;
        let end = text[semi..].find('\n').map_or(text.len(), |i| semi + i + 1);
        if !text[semi + 1..end].trim().is_empty() || text[start..semi].contains('\n') {
            return None;
        }
        let above = text[..line].trim_end().rsplit('\n').next().unwrap_or("");
        if above.trim_start().starts_with("#[") {
            return None;
        }
        // No blank line left at the top or doubled.
        let mut end = end;
        if text[end..].starts_with('\n') && (line == 0 || text[..line].ends_with("\n\n")) {
            end += 1;
        }
        let span = Span {
            start: u32::try_from(line).ok()?,
            end: u32::try_from(end).ok()?,
            ..at
        };
        Some((span, String::new(), "delete the `use`".to_owned()))
    }
}

impl Cx<'_> {
    /// `dead_code`: non-`pub` free functions and inherent methods whose
    /// name no user file mentions.
    pub(crate) fn dead_code(&mut self, user: &[&SourceFile]) {
        fn walk(cx: &mut Cx<'_>, items: &[Item]) {
            for item in items {
                match item {
                    Item::Fn(f) => cx.dead_fn(f),
                    Item::Impl(im) if im.trait_path.is_none() => {
                        for ii in &im.items {
                            if let ImplItem::Method(m) = ii {
                                cx.dead_fn(m);
                            }
                        }
                    }
                    Item::Mod(m) => walk(cx, m.items.as_deref().unwrap_or(&[])),
                    _ => {}
                }
            }
        }
        for f in user {
            walk(self, &f.items);
        }
    }

    pub(crate) fn dead_fn(&mut self, f: &FnDef) {
        const ROOTS: &[&str] = &[
            "test",
            "bench",
            "export_name",
            "no_mangle",
            "lang",
            "intrinsic",
            "infix",
            "prefix",
            "postfix",
            "literal",
            "keyword",
        ];
        // A fn in a block by its own name.
        let name = nabic_core::local::leaf(&f.name);
        if f.vis != Visibility::Pub
            && name != "main"
            && !name.starts_with('_')
            && !has_attr(&f.attrs, ROOTS)
            && !self.all_names.contains(&*name)
        {
            let at = word_after(self.texts, f.span, "fn", &name).unwrap_or(f.span);
            self.hit("dead_code", at, format!("function `{name}` is never used"));
        }
    }
}

impl Cx<'_> {
    pub(crate) fn mark_used(&mut self, name: &str) {
        for s in self.scopes.iter_mut().rev() {
            if let Some(b) = s.bindings.iter_mut().rev().find(|b| b.name == name) {
                b.used = true;
                // `A(v) | B(v)`: the other alternatives' `v` is this one.
                let Some(g) = self.or_peers.get(&key(b.span)).copied() else {
                    return;
                };
                for b in s.bindings.iter_mut().filter(|b| b.name == name) {
                    if self.or_peers.get(&key(b.span)) == Some(&g) {
                        b.used = true;
                    }
                }
                return;
            }
        }
    }

    /// `name` written (assigned, or captured by `ref`): a use when it is a
    /// `ref` parameter.
    pub(crate) fn mark_written(&mut self, name: &str) {
        for s in self.scopes.iter_mut().rev() {
            if let Some(b) = s.bindings.iter_mut().rev().find(|b| b.name == name) {
                b.used |= b.by_ref;
                return;
            }
        }
    }
}

/// `(bound name, written path, span)` for each non-glob leaf of a `use`.
pub(crate) fn use_leaves(
    t: &UseTree,
    prefix: &mut Vec<String>,
    out: &mut Vec<(String, String, Span)>,
) {
    let n = prefix.len();
    prefix.extend(t.prefix.segments.iter().map(|s| s.name.clone()));
    match &t.kind {
        UseTreeKind::Simple(alias) => {
            let last = match prefix.last().map(String::as_str) {
                Some("self") => prefix.len().checked_sub(2).map(|i| prefix[i].clone()),
                l => l.map(str::to_owned),
            };
            if let Some(name) = alias.clone().or(last) {
                out.push((name, prefix.join("::"), t.prefix.span));
            }
        }
        UseTreeKind::Group(children) => {
            for c in children {
                use_leaves(c, prefix, out);
            }
        }
        UseTreeKind::Glob | UseTreeKind::GlobRecursive => {}
    }
    prefix.truncate(n);
}
