//! Splitting and classifying cell text.

#[allow(clippy::wildcard_imports)]
use crate::*;

/// The name a cell's text goes by in locations.
pub(crate) const REPL_FILE: &str = "<repl>";

/// Cells that end the session unless the session defines the name.
pub(crate) const EXIT: [&str; 4] = ["exit", "quit", "exit()", "quit()"];

/// The cells of a multi-line text: lines until the input is complete
/// (`incomplete`), as the terminal REPL reads them.
pub fn split_cells(code: &str) -> Vec<String> {
    let mut cells = Vec::new();
    let mut buf = String::new();
    for line in code.lines() {
        buf.push_str(line);
        buf.push('\n');
        if !incomplete(&buf) {
            let cell = std::mem::take(&mut buf);
            if !blank(&cell) {
                cells.push(cell.trim().to_owned());
            }
        }
    }
    if !blank(&buf) {
        cells.push(buf.trim().to_owned());
    }
    cells
}

/// An item's text before its body or `;`, on one line, without its
/// attributes and comments: `fn f(x: i32) -> i32`, `const N: i32`.
pub(crate) fn header(item: &str) -> String {
    let rest = past_attrs(item);
    let mut depth = 0i32;
    let mut end = rest.len();
    let bytes = rest.as_bytes();
    for (i, &c) in bytes.iter().enumerate() {
        match c {
            b'(' | b'[' => depth += 1,
            b')' | b']' => depth -= 1,
            b'{' | b';' if depth == 0 => {
                end = i;
                break;
            }
            // `=` alone, not `==`, `=>`, `<=`, `>=` or `!=`.
            b'=' if depth == 0
                && !matches!(bytes.get(i + 1), Some(b'=' | b'>'))
                && !matches!(
                    i.checked_sub(1).map(|j| bytes[j]),
                    Some(b'=' | b'<' | b'>' | b'!')
                ) =>
            {
                end = i;
                break;
            }
            _ => {}
        }
    }
    rest[..end].split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Whether a `.nabi` file under `dir` defines an item `name`.
pub(crate) fn defined_in(dir: &std::path::Path, name: &str) -> bool {
    let files = nabic_interface::source_files(dir);
    files
        .iter()
        .filter(|f| f.extension().is_some_and(|x| x == "nabi"))
        .filter_map(|f| std::fs::read_to_string(f).ok())
        .any(|text| {
            let (tokens, _) = nabic_lexer::Lexer::new(&text, "").lex_all();
            tokens.windows(2).any(|w| {
                let item = match &w[0].kind {
                    T::Fn | T::Struct | T::Enum | T::Trait => true,
                    T::Ident(k) => matches!(k.as_str(), "const" | "static" | "type" | "union"),
                    _ => false,
                };
                item && matches!(&w[1].kind, T::Ident(n) if n == name)
            })
        })
}

/// Whether `text` has the identifier `name`.
pub(crate) fn names(text: &str, name: &str) -> bool {
    let (tokens, _) = nabic_lexer::Lexer::new(text, "").lex_all();
    tokens
        .iter()
        .any(|t| matches!(&t.kind, nabic_lexer::TokenKind::Ident(n) if n == name))
}

/// The items of an item cell: (replace key, start, end). A cell that is
/// not only items is one, as written.
pub(crate) fn items_of(cell: &str) -> Vec<(Option<String>, usize, usize)> {
    let (tokens, _) = nabic_lexer::Lexer::new(cell, REPL_FILE).lex_all();
    let tops = nabic_parser::parse_program(&tokens, REPL_FILE).0.tops;
    let key = |text: &str| match classify(text) {
        Cell::Item(None) if impl_trait(text) => Some(impl_header(text)),
        Cell::Item(k) => k,
        _ => None,
    };
    if tops.len() < 2 || !tops.iter().all(|t| t.1) {
        return vec![(key(cell), 0, cell.len())];
    }
    tops.iter()
        .enumerate()
        .map(|(i, &(start, _))| {
            let start = start as usize;
            let end = tops.get(i + 1).map_or(cell.len(), |t| t.0 as usize);
            let end = start + cell[start..end].trim_end().len();
            (key(&cell[start..end]), start, end)
        })
        .collect()
}

/// `impl ..` up to its body, spaces collapsed: `impl Display for P`.
pub(crate) fn impl_header(text: &str) -> String {
    let head = text.split('{').next().unwrap_or(text);
    head.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// `text` is a trait's impl, `impl Tr for T`.
pub(crate) fn impl_trait(text: &str) -> bool {
    let head = impl_header(text);
    head.starts_with("impl") && head.split(' ').any(|w| w == "for")
}

/// The methods an inherent `impl T { .. }` defines, if `text` is one, with
/// its header.
pub(crate) fn inherent(text: &str) -> Option<(String, Vec<String>)> {
    let head = impl_header(text);
    if !head.starts_with("impl") || impl_trait(text) || !matches!(classify(text), Cell::Item(None))
    {
        return None;
    }
    let (tokens, _) = nabic_lexer::Lexer::new(text, REPL_FILE).lex_all();
    let mut depth = 0;
    let mut methods = Vec::new();
    for w in tokens.windows(2) {
        match &w[0].kind {
            T::LBrace => depth += 1,
            T::RBrace => depth -= 1,
            T::Fn if depth == 1 => {
                if let T::Ident(n) = &w[1].kind {
                    methods.push(n.clone());
                }
            }
            _ => {}
        }
    }
    Some((head, methods))
}

/// Whether `new` replaces `old`: the same key, or inherent impls of one
/// type where the new one defines one of the old one's methods.
pub(crate) fn replaces(new: &Item, old: &Item) -> bool {
    if new.key.is_some() {
        return new.key == old.key;
    }
    match (inherent(&new.text), inherent(&old.text)) {
        (Some((a, ms)), Some((b, old_ms))) => a == b && ms.iter().any(|m| old_ms.contains(m)),
        _ => false,
    }
}

/// Whether a value of `ty` can hold an `any` value (`seen` cuts
/// recursive types).
pub(crate) fn holds_any(hir: &HirModule, ty: &HirTy, seen: &mut Vec<HirTy>) -> bool {
    if seen.contains(ty) {
        return false;
    }
    seen.push(ty.clone());
    let mut any = |ts: &[HirTy]| ts.iter().any(|t| holds_any(hir, t, seen));
    match ty {
        HirTy::Any | HirTy::AnyTrait(_) => true,
        HirTy::Sum(ts) | HirTy::Tuple(ts) => any(ts),
        HirTy::Array(t, _) | HirTy::GcBuf(t) => any(std::slice::from_ref(t)),
        HirTy::Throws { ok, err } => any(&[(**ok).clone(), (**err).clone()]),
        HirTy::Struct(id) => hir.structs.iter().filter(|s| s.id == *id).any(|s| {
            let fields: Vec<HirTy> = s.fields.iter().map(|f| f.ty.clone()).collect();
            any(&fields)
        }),
        HirTy::Enum(id) => hir.enums.iter().filter(|e| e.id == *id).any(|e| {
            let payloads: Vec<HirTy> = e
                .variants
                .iter()
                .filter_map(|v| v.payload_ty.clone())
                .collect();
            any(&payloads)
        }),
        _ => false,
    }
}

/// The functions that may hold the session's top level: `main`, then the
/// coroutine bodies it starts (a top-level `await` makes `main` run its
/// body on the executor).
pub(crate) fn top_fns(hir: &HirModule) -> Vec<usize> {
    let Some(main) = hir.functions.iter().position(|f| f.name == "main") else {
        return Vec::new();
    };
    let mut out = vec![main];
    if let Some(body) = &hir.functions[main].body {
        nabic_hir::each_block(body, &mut |e| {
            if let HirExprKind::CoroNew { body_fn, .. } = &e.kind {
                out.extend(hir.functions.iter().position(|f| f.id == *body_fn));
            }
        });
    }
    out
}

/// Whether `s` is the cell's block: the block expression at offset `at`
/// of the session source.
pub(crate) const fn is_cell(s: &HirStmt, at: u32) -> bool {
    matches!(s, HirStmt::Expr(e) if e.span.start == at && matches!(e.kind, HirExprKind::Block(_)))
}

/// In `f`, the statements before the cell's block (`is_cell`) in the
/// block holding it, and the cell's block.
pub(crate) fn cell_scope(f: &HirFunction, at: u32) -> Option<(&[HirStmt], &HirBlock)> {
    fn split(scope: &HirBlock, at: u32) -> Option<(&[HirStmt], &HirBlock)> {
        let i = scope.stmts.iter().position(|s| is_cell(s, at))?;
        match &scope.stmts[i] {
            HirStmt::Expr(HirExpr {
                kind: HirExprKind::Block(b),
                ..
            }) => Some((&scope.stmts[..i], b)),
            _ => None,
        }
    }
    let body = f.body.as_ref()?;
    let mut found = split(body, at);
    nabic_hir::each_block(body, &mut |e| {
        if let (None, HirExprKind::Block(b)) = (&found, &e.kind) {
            found = split(b, at);
        }
    });
    found
}

/// The type of the last top-level binding `name` of the session (`main`
/// or its coroutine body), as written.
pub(crate) fn binding_ty(r: &CompileResult, name: &str) -> Option<String> {
    let hir: &HirModule = &r.modules.first()?.hir;
    let local = top_fns(hir).into_iter().find_map(|f| {
        hir.functions[f]
            .locals
            .iter()
            .rev()
            .find(|l| l.name.as_deref() == Some(name))
    })?;
    Some(match &local.ty {
        HirTy::Unit => "()".to_owned(),
        t => ty_name(hir, t),
    })
}

/// `text` past its leading attributes and line comments, which belong to
/// the item after them.
pub(crate) fn past_attrs(text: &str) -> &str {
    let mut rest = text.trim_start();
    loop {
        if rest.starts_with("#[") {
            let Some(end) = matching(rest, 1) else { break };
            rest = rest[end + 1..].trim_start();
        } else if rest.starts_with("//") {
            rest = rest.split_once('\n').map_or("", |(_, r)| r).trim_start();
        } else {
            return rest;
        }
    }
    rest
}

pub(crate) fn classify(cell: &str) -> Cell {
    let rest = past_attrs(cell);
    let words: Vec<&str> = rest
        .split(|c: char| !(c.is_alphanumeric() || c == '_'))
        .filter(|w| !w.is_empty())
        .take(4)
        .collect();
    // A block's first word is inside it: `{ let q = 2; q * 3 }` is a value.
    let first = if rest.starts_with('{') {
        ""
    } else {
        words.first().copied().unwrap_or("")
    };
    let item_kw = [
        "fn", "struct", "enum", "union", "trait", "impl", "use", "mod", "type", "const", "static",
        "extern", "pub", "async", "unsafe",
    ];
    if item_kw.contains(&first) && !(first == "unsafe" && rest[6..].trim_start().starts_with('{')) {
        // `fn f` / `struct S` / `pub fn f` name what a redefinition replaces;
        // an impl's methods do not name it.
        let named = words
            .windows(2)
            .find(|w| {
                [
                    "fn", "struct", "enum", "union", "trait", "type", "const", "static", "impl",
                ]
                .contains(&w[0])
            })
            .filter(|w| w[0] != "impl");
        return Cell::Item(named.map(|w| format!("{} {}", w[0], w[1])));
    }
    if first == "let" {
        return Cell::Let;
    }
    if cell.ends_with(';') {
        Cell::Stmt
    } else {
        Cell::Expr
    }
}

/// `cell` split before its last top-level element.
pub(crate) fn split_last(cell: &str) -> (&str, &str) {
    let (tokens, _) = nabic_lexer::Lexer::new(cell, REPL_FILE).lex_all();
    let tops = nabic_parser::parse_program(&tokens, REPL_FILE).0.tops;
    cell.split_at(tops.last().map_or(0, |t| t.0 as usize))
}

/// Byte index of the bracket closing the one at `open`.
pub(crate) fn matching(s: &str, open: usize) -> Option<usize> {
    let mut depth = 0i32;
    for (i, c) in s.char_indices().skip(open) {
        match c {
            '[' | '(' | '{' => depth += 1,
            ']' | ')' | '}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
    }
    None
}

/// The input needs more lines (`...` continuation): an open
/// bracket, string or block comment, a last token that cannot end a
/// statement, or an item header still waiting for its body.
pub fn incomplete(src: &str) -> bool {
    // A command is one line (`:load .`).
    if src.trim_start().starts_with(':') {
        return false;
    }
    let (tokens, errors) = nabic_lexer::Lexer::new(src, REPL_FILE).lex_all();
    if errors
        .iter()
        .any(|e| e.message.starts_with("unterminated") && !e.message.contains("char"))
    {
        return true;
    }
    // A doc comment waits for its item.
    let last = src.lines().rev().map(str::trim).find(|l| !l.is_empty());
    if last.is_some_and(|l| l.starts_with("///") && !l.starts_with("////")) {
        return true;
    }
    let tokens = tokens.split_last().map_or(&[][..], |(_, t)| t);
    let mut depth = 0i32;
    // The last top-level statement starts after a `;` or a closing `}`.
    let mut start = 0;
    for (i, t) in tokens.iter().enumerate() {
        match t.kind {
            T::LParen | T::LBracket | T::LBrace | T::HashLBracket | T::HashBangLBracket => {
                depth += 1;
            }
            T::RParen | T::RBracket | T::RBrace => {
                depth -= 1;
                if depth == 0 && t.kind == T::RBrace {
                    start = i + 1;
                }
            }
            T::Semi if depth == 0 => start = i + 1,
            _ => {}
        }
    }
    if depth > 0 {
        return true;
    }
    tokens.last().is_some_and(|t| continues(&t.kind)) || item_header(&tokens[start..])
}

/// The brackets still open at the end of `src`.
pub fn open_brackets(src: &str) -> usize {
    let (tokens, _) = nabic_lexer::Lexer::new(src, REPL_FILE).lex_all();
    tokens.iter().fold(0, |depth, t| match t.kind {
        T::LParen | T::LBracket | T::LBrace | T::HashLBracket | T::HashBangLBracket => depth + 1,
        T::RParen | T::RBracket | T::RBrace => depth.saturating_sub(1),
        _ => depth,
    })
}

/// `cell` with the `;` its last top-level `let` or assignment may lack.
pub(crate) fn close_stmt(cell: &str) -> String {
    let last = split_last(cell).1;
    let (tokens, _) = nabic_lexer::Lexer::new(last, REPL_FILE).lex_all();
    // A place starts an assignment; `using g = ..`, `if let p = ..` and
    // `static S: T = ..` hold an `=` without being one.
    let place = match tokens.first().map(|t| &t.kind) {
        Some(T::Ident(w)) => !matches!(w.as_str(), "static" | "const" | "type" | "extern"),
        Some(T::SelfLower | T::Star | T::LParen) => true,
        _ => false,
    };
    let mut depth = 0i32;
    let mut assigns = false;
    let scan = if place { &tokens[..] } else { &[] };
    for t in scan {
        match t.kind {
            T::LParen | T::LBracket | T::LBrace | T::HashLBracket | T::HashBangLBracket => {
                depth += 1;
            }
            T::RParen | T::RBracket | T::RBrace => depth -= 1,
            T::Eq
            | T::PlusEq
            | T::MinusEq
            | T::StarEq
            | T::SlashEq
            | T::PercentEq
            | T::PercentPercentEq
            | T::AmpEq
            | T::PipeEq
            | T::CaretEq
            | T::ShlEq
            | T::ShrEq
                if depth == 0 =>
            {
                assigns = true;
            }
            _ => {}
        }
    }
    let open = tokens
        .iter()
        .rev()
        .nth(1)
        .is_some_and(|t| t.kind != T::Semi);
    // `use`, `const`, `static` and `type` end in `;` too, even after a `}`.
    let mut rest = tokens.iter().map(|t| &t.kind).peekable();
    if rest.next_if(|k| **k == T::Pub).is_some() && rest.next_if(|k| **k == T::LParen).is_some() {
        rest.find(|k| **k == T::RParen);
    }
    let item = match rest.next() {
        Some(T::Use) => true,
        Some(T::Ident(w)) => matches!(w.as_str(), "const" | "static" | "type"),
        _ => false,
    };
    if open && (assigns || item || matches!(classify(last), Cell::Let)) {
        // On a line of its own, outside the text diagnostics quote.
        format!("{cell}\n;")
    } else {
        cell.to_owned()
    }
}

/// `src` is only whitespace and comments.
pub(crate) fn blank(src: &str) -> bool {
    nabic_lexer::Lexer::new(src, REPL_FILE).lex_all().0.len() <= 1
}

/// `kind` needs a token after it: a binary operator, `.`, `,`, `=`,
/// `->`, `|`, or a keyword that leads into more.
pub(crate) fn continues(kind: &nabic_lexer::TokenKind) -> bool {
    match kind {
        T::Ident(w) => matches!(w.as_str(), "in" | "mut" | "where"),
        // Not `>` / `>>`: they also close generics (`x as List<i32>`).
        T::Plus
        | T::Minus
        | T::Star
        | T::Slash
        | T::Percent
        | T::PercentPercent
        | T::EqEq
        | T::NotEq
        | T::Lt
        | T::LtEq
        | T::GtEq
        | T::AmpAmp
        | T::PipePipe
        | T::PipeGt
        | T::Bang
        | T::Amp
        | T::Pipe
        | T::Caret
        | T::Tilde
        | T::Shl
        | T::MathOp(_)
        | T::ExtOp(_)
        | T::DotPlus
        | T::DotMinus
        | T::DotStar
        | T::DotSlash
        | T::DotPercent
        | T::DotPercentPercent
        | T::DotEqEq
        | T::DotNotEq
        | T::DotLt
        | T::DotGt
        | T::DotLtEq
        | T::DotGtEq
        | T::DotAmp
        | T::DotPipe
        | T::DotAmpAmp
        | T::DotPipePipe
        | T::DotCaret
        | T::DotShl
        | T::DotShr
        | T::DotQuestionQuestion
        | T::DotEq
        | T::DotPlusEq
        | T::DotMinusEq
        | T::DotStarEq
        | T::DotStarStarEq
        | T::DotSlashEq
        | T::DotPercentEq
        | T::DotPercentPercentEq
        | T::DotAmpEq
        | T::DotPipeEq
        | T::DotCaretEq
        | T::DotShlEq
        | T::DotShrEq
        | T::Eq
        | T::PlusEq
        | T::MinusEq
        | T::StarEq
        | T::SlashEq
        | T::PercentEq
        | T::PercentPercentEq
        | T::AmpEq
        | T::PipeEq
        | T::CaretEq
        | T::ShlEq
        | T::ShrEq
        | T::DotDotEq
        | T::Colon
        | T::ColonColon
        | T::Dot
        | T::QuestionDot
        | T::QuestionQuestion
        | T::Arrow
        | T::FatArrow
        | T::Comma
        | T::At
        | T::Let
        | T::If
        | T::Else
        | T::Match
        | T::While
        | T::Loop
        | T::For
        | T::As
        | T::Is
        | T::Ref
        | T::Try
        | T::Throw
        | T::Using => true,
        _ => false,
    }
}

/// `stmt` (balanced, no `;` and no block) is only attributes or an
/// item's header: `fn g(x: i32)` waits for `-> i32 {` on the next line.
pub(crate) fn item_header(stmt: &[nabic_lexer::Token]) -> bool {
    let kind = |i: usize| stmt.get(i).map(|t| &t.kind);
    let mut i = 0;
    loop {
        match kind(i) {
            None => return i > 0,
            Some(T::HashLBracket | T::HashBangLBracket) => i = past_group(stmt, i),
            Some(T::Pub) if kind(i + 1) == Some(&T::LParen) => i = past_group(stmt, i + 1),
            Some(T::Pub | T::Async | T::Unsafe) => i += 1,
            Some(T::Ident(w)) if w == "extern" => {
                i += if matches!(kind(i + 1), Some(T::StrLit(_))) {
                    2
                } else {
                    1
                };
            }
            Some(T::Ident(w)) if w == "const" && kind(i + 1) == Some(&T::Fn) => i += 1,
            Some(T::Fn | T::Struct | T::Enum | T::Trait | T::Impl) => return true,
            Some(T::Ident(w)) => return w == "union" && matches!(kind(i + 1), Some(T::Ident(_))),
            Some(_) => return false,
        }
    }
}

/// Index past the bracket closing the one at `open`.
pub(crate) fn past_group(tokens: &[nabic_lexer::Token], open: usize) -> usize {
    let mut depth = 0i32;
    for (i, t) in tokens.iter().enumerate().skip(open) {
        match t.kind {
            T::LParen | T::LBracket | T::HashLBracket | T::HashBangLBracket => depth += 1,
            T::RParen | T::RBracket => depth -= 1,
            _ => {}
        }
        if depth == 0 {
            return i + 1;
        }
    }
    tokens.len()
}
