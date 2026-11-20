//! Diagnostics and values as the user sees them.

#[allow(clippy::wildcard_imports)]
use crate::*;

/// User text inside the generated session source, and where it shows.
pub(crate) struct Region {
    /// Byte offset and length in the session source.
    pub(crate) off: usize,
    pub(crate) len: usize,
    /// The file it shows in (registered id, path, text) and its offset there.
    pub(crate) file: u32,
    pub(crate) path: String,
    pub(crate) text: Arc<str>,
    pub(crate) at: usize,
    /// An earlier cell's item, as messages name it (`` `impl P` from cell
    /// 2 ``).
    pub(crate) owner: Option<String>,
}

/// How runtime locations name cell `n`'s text.
pub(crate) fn cell_path(n: CellNo) -> String {
    if n.is_none() {
        REPL_FILE.to_owned()
    } else {
        format!("<repl cell {n}>")
    }
}

/// 1-based line of byte `off` in `src`, and how many chars precede it
/// on that line.
pub(crate) fn line_col(src: &str, off: usize) -> (u32, u32) {
    let before = &src[..off];
    let start = before.rfind('\n').map_or(0, |i| i + 1);
    (
        u32::try_from(before.matches('\n').count() + 1).unwrap_or(0),
        u32::try_from(before[start..].chars().count()).unwrap_or(0),
    )
}

/// `d` as the user sees it: spans in the user's text move to their file
/// (`regions`); spans in the generated wrapper are dropped. Also the
/// owner of the region its primary span is in.
pub(crate) fn localize(
    d: &NabiDiagnostic,
    session: u32,
    path: &str,
    regions: &[Region],
) -> (NabiDiagnostic, Option<String>) {
    let mut d = d.clone();
    // A span touching a region's edge (the `(` around an expression cell)
    // snaps into it.
    let region = |s: Span| -> Option<&Region> {
        let start = s.start as usize;
        regions
            .iter()
            .find(|r| (r.off..r.off + r.len).contains(&start))
            .or_else(|| {
                regions
                    .iter()
                    .find(|r| start + 1 >= r.off && start <= r.off + r.len)
            })
    };
    // `None`: in the wrapper.
    let map = |s: Span| -> Option<Span> {
        if s.file != session {
            return Some(s);
        }
        let (start, end) = (s.start as usize, (s.end as usize).max(s.start as usize));
        let r = region(s)?;
        let shown = |b: usize| r.at + b.saturating_sub(r.off).min(r.len);
        let (start, end) = (shown(start), shown(end));
        let (line, col) = line_col(&r.text, start);
        Some(Span {
            start: u32::try_from(start).ok()?,
            end: u32::try_from(end).ok()?,
            line,
            col: col + 1,
            file: r.file,
        })
    };
    if d.file.as_deref() == Some(path) {
        d.file = None;
    }
    let owner = d
        .primary
        .as_ref()
        .filter(|p| p.span.file == session)
        .and_then(|p| region(p.span)?.owner.clone());
    if let Some(p) = d.primary.take() {
        d.primary = map(p.span).map(|span| Box::new(Label { span, ..*p }));
    }
    d.secondary = std::mem::take(&mut d.secondary)
        .into_iter()
        .filter_map(|l| {
            Some(Label {
                span: map(l.span)?,
                ..l
            })
        })
        .collect();
    d.message = d
        .message
        .replace(&format!("`let {VALUE}`"), "the expression");
    (d, owner)
}

/// How a cell's value shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Fmt {
    /// `display(v)` under the kernel: its `RichDisplay` bundle goes out as
    /// `display_data`, nothing more is shown.
    Rich,
    Display,
    Debug,
    /// Its type name (an `any` or sum value's: the held type's).
    Type,
}

impl Fmt {
    /// The text the host gets for value `n`.
    pub(crate) fn text(self, n: &str) -> String {
        match self {
            Self::Rich => format!("{{ display({n}); \"\" }}"),
            Self::Display => format!("f\"{{{n}}}\""),
            Self::Debug => format!("f\"{{{n}:?}}\""),
            Self::Type => format!("{n}.type_name_of_val()"),
        }
    }
}

/// Whether the `errors` of a build showing the value with `fmt` say
/// only that the value's type lacks that trait; then how to show it
/// instead. `file` is the id of `src`, the session source.
pub(crate) fn unformattable(
    errors: &[NabiDiagnostic],
    src: &str,
    file: u32,
    fmt: Fmt,
) -> Option<Fmt> {
    let only = errors.iter().all(|d| {
        matches!(
            d.tag,
            Tag::NoDisplay | Tag::NoDisplayHasDebug | Tag::NoDebug
        ) && in_show(d, src, file)
    });
    // The type has no `RichDisplay`: the bound of `display` is all that
    // failed in the store.
    if fmt == Fmt::Rich && !errors.is_empty() && errors.iter().all(|d| in_show(d, src, file)) {
        return Some(Fmt::Display);
    }
    if errors.is_empty() || !only {
        return None;
    }
    let debug = errors.iter().any(|d| d.tag == Tag::NoDisplayHasDebug);
    match fmt {
        Fmt::Rich | Fmt::Display if debug => Some(Fmt::Debug),
        Fmt::Rich | Fmt::Display | Fmt::Debug => Some(Fmt::Type),
        Fmt::Type => None,
    }
}

/// Whether `d` is about the store of the shown value in `src`, the
/// session source (file id `file`).
pub(crate) fn in_show(d: &NabiDiagnostic, src: &str, file: u32) -> bool {
    let Some(store) = src.rfind(&format!("\n{SHOWN} = ")) else {
        return false;
    };
    d.primary
        .as_ref()
        .is_some_and(|p| p.span.file == file && p.span.start as usize > store)
}

/// What a failed cell's host reply `why` (`nabir/src/repl.rs`) tells
/// the user.
pub(crate) fn failure(why: &str) -> String {
    const SAME: &str = "the session is unchanged";
    if let Some(code) = why.strip_prefix("exit ") {
        return format!(
            "the cell called exit({code}); {SAME} (`exit` or `:quit` ends the session)"
        );
    }
    if let Some(t) = why.strip_prefix("throw\n") {
        let (at, t) = t.split_once('\n').unwrap_or(("", t));
        let (raised, value) = t.split_once('\n').unwrap_or(("", t));
        let mut m = format!("the cell threw `{value}`; {SAME}");
        if !at.is_empty() {
            let _ = write!(m, "\n  --> {at}");
        }
        if !raised.is_empty() {
            let _ = write!(m, "\n  raised at {raised}");
        }
        return m;
    }
    match why {
        "interrupted" => format!("interrupted (Ctrl-C); {SAME}"),
        "panic" => format!("the cell panicked; {SAME}"),
        _ => format!("the cell failed; {SAME}"),
    }
}
