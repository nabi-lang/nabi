//! Operator precedence is the language's (`fixed_prec`).
//! `#[infix]` / `#[prefix]` / `#[postfix]` only bind tokens to trait
//! methods; `PrecTable` records that for HIR.

use nabic_ast::{AttrArg, AttrBody, Attribute, Item, Literal, Program, TraitItem};
use nabic_core::HashMap;
use nabic_diagnostic::{Code, NabiDiagnostic};
use nabic_lexer::TokenKind;

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum Fixity {
    Infix,
    Prefix,
    Postfix,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Assoc {
    Left,
    Right,
    /// Non-associative; chaining must be parenthesized.
    None,
}

/// Binding power of the prefix unary operators and `await`
/// (right-assoc).
pub const PREFIX_PREC: u32 = 13;
/// Binding power of `**` / `.**` (right-assoc): tighter than the
/// prefix operators, so `-x**2` is `-(x**2)`.
pub const POW_PREC: u32 = 13;
/// Binding power of `as` / `is` (left-assoc).
pub const CAST_PREC: u32 = 12;
/// Binding power of `??` (level 1, right-assoc).
pub const COALESCE_PREC: u32 = 1;

/// A built-in operator's precedence, Rust's order; dotted
/// broadcast tokens share their scalar's row. `None` outside the table.
#[must_use]
pub fn fixed_prec(symbol: &str, fixity: Fixity) -> Option<(u32, Assoc)> {
    let base = match symbol {
        "./" => "/",
        s if s.len() > 1 && s.starts_with('.') => &s[1..],
        s => s,
    };
    match fixity {
        Fixity::Prefix => {
            matches!(symbol, "-" | "!" | "~" | "√").then_some((PREFIX_PREC, Assoc::Right))
        }
        // The postfix row (`?`, member access). `?` itself is language
        // syntax; it is here only as a `like` target.
        Fixity::Postfix => (symbol == "?").then_some((14, Assoc::Left)),
        // `.??`: element-wise `??`, its level.
        Fixity::Infix if symbol == ".??" => Some((COALESCE_PREC, Assoc::Right)),
        Fixity::Infix => Some(match base {
            "**" => (POW_PREC, Assoc::Right),
            "∘" => (11, Assoc::Left),
            "*" | "/" | "%" | "%%" | "×" | "⋅" | "÷" | "∩" => (10, Assoc::Left),
            "+" | "-" | "∪" => (9, Assoc::Left),
            "<<" | ">>" => (8, Assoc::Left),
            "&" => (7, Assoc::Left),
            "^" => (6, Assoc::Left),
            "|" => (5, Assoc::Left),
            "==" | "!=" | "<" | ">" | "<=" | ">=" | "≡" | "≠" | "≤" | "≥" | "∈" | "∉" | "⊂"
            | "⊆" => (4, Assoc::None),
            "&&" => (3, Assoc::Left),
            "||" => (2, Assoc::Left),
            _ => return None,
        }),
    }
}

/// Compound assignment tokens: statements, so no binding
/// level; their `#[infix]` binds the `*Assign` method.
#[must_use]
pub fn is_assign_token(symbol: &str) -> bool {
    matches!(
        symbol,
        "+=" | "-=" | "*=" | "**=" | "/=" | "%=" | "%%=" | "&=" | "|=" | "^=" | "<<=" | ">>="
    )
}

/// The level recorded for a compound assignment token (never parsed).
const ASSIGN_LEVEL: (u32, Assoc) = (0, Assoc::Right);

#[derive(Clone, Debug)]
pub struct OpEntry {
    pub symbol: String,
    pub fixity: Fixity,
    /// Trait method the operator dispatches to (`add` for `+`).
    pub method_name: String,
    /// Trait declaring that method, by its full path (`core::ops::Add`,
    /// `m::Dot`); only its impls make a type support the operator.
    pub trait_name: String,
    /// `#[infix("∈", swap)]`: `a ∈ s` calls `s.contains(a)`.
    pub swap: bool,
    /// Binding level: the table's, or `like`'s for an extension token.
    pub level: (u32, Assoc),
    /// The package whose trait registers it (`""`: the one being built).
    pub package: String,
    /// The trait is `pub`: its tokens reach the packages depending on
    /// `package`.
    pub public: bool,
}

/// The fixed tokens whose binary position a registration may bind.
///
/// The language writes `@` and `~` only where no operand ends before them
/// (`@derive`, `n @ (a, b)`, `return@f`, prefix `~x`), so `a @ b` and
/// `a ~ b` are free; they stay fixed tokens and the parser reads them as
/// operators where they are bound.
pub const FIXED_INFIX: [&str; 2] = ["@", "~"];

/// Who registered a token that starts like a fixed one.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Owner {
    pub package: String,
    /// Its trait is `pub`.
    pub public: bool,
}

/// Registered extension operator tokens: what the lexer must recognize
/// and the level the parser gives them.
#[derive(Clone, Debug, Default)]
pub struct ExtOps {
    pub levels: HashMap<(String, Fixity), (u32, Assoc)>,
    /// Tokens that start like a fixed one (`<-`, `**`), by the package
    /// registering them: they lex in that package's files and, from a
    /// `pub` trait, in its dependents', so `a<-1` elsewhere stays
    /// `a < -1`.
    pub owners: std::collections::BTreeMap<String, Owner>,
    /// The dependencies of each package (`""`: the one being built);
    /// a package missing here may see every other package's tokens.
    pub deps: HashMap<String, Vec<String>>,
}

impl ExtOps {
    /// Distinct symbols the lexer cuts, longest first (its longest
    /// match); binary `@` is the fixed token.
    #[must_use]
    pub fn symbols(&self) -> Vec<String> {
        let mut v: Vec<String> = self
            .levels
            .keys()
            .filter(|k| !FIXED_INFIX.contains(&k.0.as_str()))
            .map(|k| k.0.clone())
            .collect();
        v.sort_by_key(|s| std::cmp::Reverse(s.len()));
        v.dedup();
        v
    }

    /// Whether the files of `package` see `owner`'s tokens.
    fn sees(&self, package: &str, owner: &Owner) -> bool {
        owner.package == package
            || owner.public
                && self
                    .deps
                    .get(package)
                    .is_none_or(|d| d.contains(&owner.package))
    }

    /// What files of `package` lex: without the tokens starting like a
    /// fixed one that it does not see (`sees`).
    #[must_use]
    pub fn in_package(&self, package: &str) -> std::borrow::Cow<'_, Self> {
        if self.owners.values().all(|o| self.sees(package, o)) {
            return std::borrow::Cow::Borrowed(self);
        }
        let hidden = |s: &String| self.owners.get(s).is_some_and(|o| !self.sees(package, o));
        std::borrow::Cow::Owned(Self {
            levels: self
                .levels
                .iter()
                .filter(|((s, _), _)| !hidden(s))
                .map(|(k, v)| (k.clone(), *v))
                .collect(),
            owners: self
                .owners
                .iter()
                .filter(|(s, _)| !hidden(s))
                .map(|(s, o)| (s.clone(), o.clone()))
                .collect(),
            deps: self.deps.clone(),
        })
    }

    /// Level of `symbol` at `fixity`: the fixed table, else a registration.
    #[must_use]
    pub fn level(&self, symbol: &str, fixity: Fixity) -> Option<(u32, Assoc)> {
        fixed_prec(symbol, fixity)
            .or_else(|| self.levels.get(&(symbol.to_owned(), fixity)).copied())
    }
}

/// Operator bindings: `(symbol, fixity) -> trait method`.
#[derive(Clone, Debug, Default)]
pub struct PrecTable {
    by_key: HashMap<(String, Fixity), OpEntry>,
}

impl PrecTable {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert(&mut self, entry: OpEntry) {
        self.by_key
            .insert((entry.symbol.clone(), entry.fixity), entry);
    }

    #[must_use]
    pub fn lookup(&self, symbol: &str, fixity: Fixity) -> Option<&OpEntry> {
        self.by_key.get(&(symbol.to_owned(), fixity))
    }

    pub fn entries(&self) -> impl Iterator<Item = &OpEntry> {
        self.by_key.values()
    }

    /// Extension tokens: registered symbols outside the
    /// fixed table, with the level they took from `like`.
    #[must_use]
    pub fn ext_ops(&self) -> ExtOps {
        let ext = || {
            self.by_key.values().filter(|e| {
                fixed_prec(&e.symbol, e.fixity).is_none() && !is_assign_token(&e.symbol)
            })
        };
        ExtOps {
            levels: ext()
                .map(|e| ((e.symbol.clone(), e.fixity), e.level))
                .collect(),
            owners: ext()
                .filter(|e| {
                    !FIXED_INFIX.contains(&e.symbol.as_str())
                        && e.symbol
                            .bytes()
                            .next()
                            .is_some_and(nabic_lexer::lexer::fixed_lead)
                })
                .map(|e| {
                    let owner = Owner {
                        package: e.package.clone(),
                        public: e.public,
                    };
                    (e.symbol.clone(), owner)
                })
                .collect(),
            deps: HashMap::default(),
        }
    }

    /// Insert `entry`; an error if the token is already bound to a
    /// different trait method.
    pub fn merge(&mut self, entry: OpEntry) -> Option<NabiDiagnostic> {
        if let Some(e) = self.lookup(&entry.symbol, entry.fixity) {
            if e.trait_name != entry.trait_name || e.method_name != entry.method_name {
                return Some(NabiDiagnostic::error(
                    Code::DUPLICATE_REGISTRATION,
                    nabic_diagnostic::Span::DUMMY,
                    format!(
                        "operator `{}` is bound twice (`{}::{}` and `{}::{}`)",
                        entry.symbol,
                        e.trait_name,
                        e.method_name,
                        entry.trait_name,
                        entry.method_name
                    ),
                ));
            }
        }
        self.insert(entry);
        None
    }
}

/// Collect `#[infix]` / `#[prefix]` / `#[postfix]` on trait methods.
///
/// Errors: precedence arguments, `like` on a table token, a
/// non-table token without `like`, aliases on different levels,
/// one token bound to two methods.
pub fn collect_prec_table(
    program: &Program,
    module: &[String],
) -> (PrecTable, Vec<NabiDiagnostic>) {
    let mut table = PrecTable::new();
    let mut diags = Vec::new();
    collect_items(&program.items, module, &mut table, &mut diags);
    (table, diags)
}

fn collect_items(
    items: &[Item],
    module: &[String],
    table: &mut PrecTable,
    diags: &mut Vec<NabiDiagnostic>,
) {
    for item in items {
        if let Item::Mod(m) = item {
            if let Some(inner) = &m.items {
                let mut sub = module.to_vec();
                sub.push(m.name.clone());
                collect_items(inner, &sub, table, diags);
            }
            continue;
        }
        let stray = match item {
            Item::Fn(f) => f.attrs.iter().collect(),
            Item::Impl(i) => i
                .items
                .iter()
                .filter_map(|it| match it {
                    nabic_ast::ImplItem::Method(m) => Some(&m.attrs),
                    _ => None,
                })
                .flatten()
                .collect(),
            _ => Vec::new(),
        };
        for attr in stray {
            if let [p] = attr.path.as_slice() {
                if matches!(p.as_str(), "infix" | "prefix" | "postfix") {
                    diags.push(meta_err(
                        attr,
                        format!("`#[{p}]` can be attached only to a trait method signature"),
                    ));
                }
            }
        }
        let Item::Trait(t) = item else { continue };
        let trait_name = module
            .iter()
            .map(String::as_str)
            .chain(std::iter::once(t.name.as_str()))
            .collect::<Vec<_>>()
            .join("::");
        for ti in &t.items {
            let TraitItem::Method(m) = ti else { continue };
            let mut level: Option<(u32, Assoc)> = None;
            for attr in &m.attrs {
                let Some((fixity, symbol, lv, swap)) = collect_attr(attr, diags) else {
                    continue;
                };
                if !matches!(m.throws, nabic_ast::ThrowsKind::None) {
                    diags.push(throwing_op(attr, &symbol, fixity, m));
                }
                if level.is_some_and(|l| l != lv) {
                    diags.push(meta_err(
                        attr,
                        format!(
                            "operator aliases of `{}` must share one precedence level",
                            m.name
                        ),
                    ));
                    continue;
                }
                level = Some(lv);
                let entry = OpEntry {
                    symbol,
                    fixity,
                    swap,
                    level: lv,
                    method_name: m.name.clone(),
                    trait_name: trait_name.clone(),
                    package: String::new(),
                    public: t.vis == nabic_core::Visibility::Pub,
                };
                if let Some(e) = table.merge(entry) {
                    diags.push(e.with_span(attr.span, ""));
                }
            }
        }
    }
}

/// A registered token is `Pattern_Syntax` code points only
/// (no identifier character, no space) and no fixed token, binary `@`
/// aside. `Err`: why not, and what to write instead.
fn check_new_token(symbol: &str, fixity: Fixity) -> Result<(), (String, Option<String>)> {
    if symbol.is_empty() {
        return Err(("it is empty".to_owned(), None));
    }
    let instead = match fixity {
        Fixity::Prefix => "a function (`fourier(x)`)",
        Fixity::Postfix => "a method (`a.t()`)",
        Fixity::Infix => "a method (`a.solve(b)`)",
    };
    let other = format!(
        "use {instead}, or a symbol such as `†` or `⊛` (arrows, math operators, `†‡`: \
         Unicode `Pattern_Syntax`)"
    );
    if let Some(c) = symbol
        .chars()
        .find(|&c| nabic_lexer::unicode::is_ident_continue(c))
    {
        let why = if matches!(c, '\u{2032}'..='\u{2034}' | '\u{2057}') {
            "primes continue a name (`f′`)".to_owned()
        } else if matches!(c, '∑' | '∏' | '∫') {
            "`∑ ∏ ∫` are names (`∑(xs)`)".to_owned()
        } else if c.is_alphanumeric() {
            format!("letters and digits make names (`{c}x` is one name)")
        } else {
            "it may be part of a name".to_owned()
        };
        return Err((
            format!(
                "`{c}` (U+{:04X}) is an identifier character, not an operator one: {why}",
                c as u32
            ),
            Some(other),
        ));
    }
    if let Some(c) = symbol.chars().find(|&c| !is_pattern_syntax(c)) {
        return Err((
            format!(
                "`{}` (U+{:04X}) is not a Unicode `Pattern_Syntax` code point",
                c.escape_debug(),
                c as u32
            ),
            Some(other),
        ));
    }
    if FIXED_INFIX.contains(&symbol) && fixity == Fixity::Infix {
        return Ok(());
    }
    if symbol == "@" {
        return Err((
            "`@` is meta syntax before an operand (`@derive`) and after a pattern name \
             (`n @ (a, b)`): only binary `a @ b` can be bound"
                .to_owned(),
            Some("write `#[infix(\"@\", like = \"*\")]`, or bind another token".to_owned()),
        ));
    }
    if is_fixed_token(symbol) {
        return Err(("it is a fixed token of the language".to_owned(), None));
    }
    Ok(())
}

/// `symbol` lexes as one fixed operator or punctuation token.
fn is_fixed_token(symbol: &str) -> bool {
    if !symbol
        .chars()
        .all(|c| is_pattern_syntax(c) && !nabic_lexer::unicode::is_ident_continue(c))
    {
        return false;
    }
    let (tokens, diags) = nabic_lexer::Lexer::new(symbol, "").lex_all();
    diags.is_empty()
        && matches!(tokens.as_slice(), [t, end] if end.kind == TokenKind::Eof
            && !matches!(t.kind, TokenKind::ExtOp(_)))
}

/// A fixed token registered at a position the language gives it no
/// level in (postfix `%`): what it is, and what to write.
fn fixed_elsewhere(attr: &Attribute, symbol: &str, fixity: Fixity) -> Option<NabiDiagnostic> {
    if fixed_prec(symbol, fixity).is_some()
        || FIXED_INFIX.contains(&symbol) && fixity == Fixity::Infix
        || !is_fixed_token(symbol)
    {
        return None;
    }
    if symbol == "@" {
        let (msg, help) = check_new_token(symbol, fixity).err()?;
        return Some(
            meta_err(attr, format!("`@` can't be a new operator token: {msg}")).with_help(help?),
        );
    }
    if symbol == "!" && fixity == Fixity::Postfix {
        // `n!` would read like the macro call `name!(..)`.
        let help = "call a function or method instead: `factorial(n)` / `n.factorial()`";
        return Some(
            meta_err(
                attr,
                "`!` can't be a postfix operator: after a name it starts a macro call \
                 (`name!(..)`), and before an operand it is `not` (`!a`)"
                    .to_owned(),
            )
            .with_help(help),
        );
    }
    let forms: Vec<String> = [
        (Fixity::Infix, format!("`a {symbol} b`")),
        (Fixity::Prefix, format!("`{symbol}a`")),
        (Fixity::Postfix, format!("`a{symbol}`")),
    ]
    .into_iter()
    .filter(|(f, _)| fixed_prec(symbol, *f).is_some())
    .map(|(_, s)| s)
    .collect();
    // `a -> b` already reads as `a → b`, as `a ≤ b` reads as `a <= b`.
    if symbol == "->" && fixity == Fixity::Infix {
        return Some(
            meta_err(
                attr,
                "`->` is the fixed spelling of `→` between two values: the parser reads \
                 `a -> b` as `a → b`"
                    .to_owned(),
            )
            .with_help(
                "register `→` instead (`#[infix(\"→\", like = \"|\")]`): `a → b` and `a -> b` \
                 both call it",
            ),
        );
    }
    let has = if forms.is_empty() {
        "it is syntax, not an operator".to_owned()
    } else {
        format!("the language reads only {}", forms.join(" and "))
    };
    let help = if symbol == "%" && fixity == Fixity::Postfix {
        "for a percentage, register a number suffix: `#[literal(number \"pct\")]` makes \
         `95pct` call it"
            .to_owned()
    } else {
        "bind a new token instead, with `like = \"..\"` for its level (`⊕`, `∼`, `‰`)".to_owned()
    };
    let at = match fixity {
        Fixity::Infix => "binary",
        Fixity::Prefix => "prefix",
        Fixity::Postfix => "postfix",
    };
    Some(
        meta_err(
            attr,
            format!("`{symbol}` is a fixed token with no {at} form: {has}"),
        )
        .with_help(help),
    )
}

/// `#[infix]` on a method that throws: operators never throw.
fn throwing_op(
    attr: &Attribute,
    symbol: &str,
    fixity: Fixity,
    m: &nabic_ast::FnDef,
) -> NabiDiagnostic {
    let name = &m.name;
    let use_it = match fixity {
        Fixity::Infix => format!("a {symbol} b"),
        Fixity::Prefix => format!("{symbol}a"),
        Fixity::Postfix => format!("a{symbol}"),
    };
    let call = match fixity {
        Fixity::Infix => format!("a.{name}(b)"),
        Fixity::Prefix | Fixity::Postfix => format!("a.{name}()"),
    };
    meta_err(
        attr,
        format!("operator `{symbol}` is bound to `{name}`, which throws: operators do not throw"),
    )
    .with_help(format!(
        "drop `throws` and return the error as a value (`-> Result<T, E>` or `-> T | E`), \
         so `try ({use_it}).get()` or a `match` handles it; or keep `throws` on a named \
         method without the attribute and write `try {call}`"
    ))
}

/// `÷ ≡ ≠ ≤ ≥` lex as `/ == != <= >=`, and a dot before a Unicode
/// operator is a separate `.`: a token bound under either spelling would
/// never be seen.
fn unicode_fixed(attr: &Attribute, symbol: &str) -> Option<NabiDiagnostic> {
    let name = attr.name();
    let synonym = |c: &str| match c {
        "÷" => Some(("/", "Div")),
        "≡" => Some(("==", "PartialEq")),
        "≠" => Some(("!=", "PartialEq")),
        "≤" => Some(("<=", "PartialOrd")),
        "≥" => Some((">=", "PartialOrd")),
        _ => None,
    };
    if let Some(base) = symbol.strip_prefix('.').filter(|b| !b.is_ascii()) {
        fixed_prec(base, Fixity::Infix)?;
        let help = synonym(base).map_or_else(
            || {
                format!(
                    "only the ASCII operators have a broadcast form; write `#[{name}(\"{base}\")]`, \
                     or bind a new token with `like = \"..\"`"
                )
            },
            |(a, _)| format!("the broadcast form of `{a}` is `.{a}`"),
        );
        return Some(
            meta_err(
                attr,
                format!(
                    "`{symbol}` is not an operator token: the lexer reads `.` and `{base}` apart"
                ),
            )
            .with_help(help),
        );
    }
    let (ascii, tr) = synonym(symbol)?;
    Some(
        meta_err(
            attr,
            format!(
                "`{symbol}` is the fixed operator `{ascii}`: the lexer reads it as `{ascii}`, so \
                 `a {symbol} b` already calls what `a {ascii} b` calls"
            ),
        )
        .with_help(format!(
            "implement `ops::{tr}` for your type: `a {ascii} b` and `a {symbol} b` both call it"
        )),
    )
}

/// Unicode `Pattern_Syntax` (a stable property).
const fn is_pattern_syntax(c: char) -> bool {
    matches!(c,
        '!'..='/' | ':'..='@' | '['..='^' | '`' | '{'..='~'
        | '\u{A1}'..='\u{A7}' | '\u{A9}' | '\u{AB}' | '\u{AC}' | '\u{AE}' | '\u{B0}' | '\u{B1}'
        | '\u{B6}' | '\u{BB}' | '\u{BF}' | '\u{D7}' | '\u{F7}'
        | '\u{2010}'..='\u{2027}' | '\u{2030}'..='\u{203E}' | '\u{2041}'..='\u{2053}'
        | '\u{2055}'..='\u{205E}' | '\u{2190}'..='\u{245F}' | '\u{2500}'..='\u{2775}'
        | '\u{2794}'..='\u{2BFF}' | '\u{2E00}'..='\u{2E7F}' | '\u{3001}'..='\u{3003}'
        | '\u{3008}'..='\u{3020}' | '\u{3030}' | '\u{FD3E}' | '\u{FD3F}' | '\u{FE45}' | '\u{FE46}')
}

/// The text of a string argument: `"\\"` is one backslash, as in
/// any string literal; `r"\"` too.
fn str_value(attr: &Attribute, lit: &Literal, diags: &mut Vec<NabiDiagnostic>) -> Option<String> {
    match lit {
        Literal::Str(s) => {
            match nabic_lexer::escape::unescape(s, nabic_lexer::escape::EscapeMode::Str) {
                Ok(v) => Some(v),
                Err((_, msg)) => {
                    diags.push(meta_err(attr, msg));
                    None
                }
            }
        }
        Literal::RawStr(s) => Some(s.clone()),
        _ => None,
    }
}

fn meta_err(attr: &Attribute, msg: String) -> NabiDiagnostic {
    NabiDiagnostic::error(Code::INVALID_OPERATOR_DECL, attr.span, msg)
}

fn collect_attr(
    attr: &Attribute,
    diags: &mut Vec<NabiDiagnostic>,
) -> Option<(Fixity, String, (u32, Assoc), bool)> {
    let fixity = match attr.name() {
        "infix" => Fixity::Infix,
        "prefix" => Fixity::Prefix,
        "postfix" => Fixity::Postfix,
        _ => return None,
    };
    let name = attr.name();
    let AttrBody::Args(args) = &attr.body else {
        diags.push(meta_err(
            attr,
            format!("`#[{name}]` needs an operator token"),
        ));
        return None;
    };
    let mut symbol: Option<String> = None;
    let mut like: Option<String> = None;
    let mut swap = false;
    for a in args {
        match a {
            AttrArg::Lit(l @ (Literal::Str(_) | Literal::RawStr(_))) if symbol.is_none() => {
                symbol = Some(str_value(attr, l, diags)?);
            }
            AttrArg::Word(w) if w == "swap" && fixity == Fixity::Infix && !swap => swap = true,
            AttrArg::KeyValue(k, l @ (Literal::Str(_) | Literal::RawStr(_)))
                if k == "like" && like.is_none() =>
            {
                like = Some(str_value(attr, l, diags)?);
            }
            _ => {
                diags.push(meta_err(
                    attr,
                    format!(
                        "`#[{name}]` takes only an operator token, `like = \"..\"` and `swap`: \
                         precedence and associativity are fixed by the language"
                    ),
                ));
                return None;
            }
        }
    }
    let Some(symbol) = symbol else {
        diags.push(meta_err(
            attr,
            format!("`#[{name}]` needs an operator token"),
        ));
        return None;
    };
    if symbol == "?" {
        diags.push(meta_err(
            attr,
            "`?` is language syntax (error propagation), not an operator token".to_owned(),
        ));
        return None;
    }
    if symbol == "|>" {
        diags.push(
            meta_err(
                attr,
                "`|>` is the language's pipe, not an operator token: `x |> f(a)` calls \
                 `f(x, a)` with no trait"
                    .to_owned(),
            )
            .with_help("drop the attribute and call the function with `|>`, or bind another token"),
        );
        return None;
    }
    if let Some(d) = unicode_fixed(attr, &symbol) {
        diags.push(d);
        return None;
    }
    if is_assign_token(&symbol) {
        if fixity != Fixity::Infix || like.is_some() || swap {
            diags.push(meta_err(
                attr,
                format!("`{symbol}` is a compound assignment: only `#[infix(\"{symbol}\")]`"),
            ));
            return None;
        }
        return Some((fixity, symbol, ASSIGN_LEVEL, false));
    }
    if let Some(d) = fixed_elsewhere(attr, &symbol, fixity) {
        diags.push(d);
        return None;
    }
    let level = match (fixed_prec(&symbol, fixity), like) {
        (Some(lv), None) => lv,
        (Some(_), Some(_)) => {
            diags.push(meta_err(
                attr,
                format!("`{symbol}` has a fixed precedence; `like` is only for new tokens"),
            ));
            return None;
        }
        (None, Some(l)) => {
            if let Err((why, help)) = check_new_token(&symbol, fixity) {
                let d = meta_err(
                    attr,
                    format!("`{symbol}` can't be a new operator token: {why}"),
                );
                diags.push(match help {
                    Some(h) => d.with_help(h),
                    None => d,
                });
                return None;
            }
            let Some(lv) = fixed_prec(&l, fixity) else {
                diags.push(meta_err(
                    attr,
                    format!("`like = \"{l}\"` must name a built-in {name} operator"),
                ));
                return None;
            };
            lv
        }
        (None, None) => {
            diags.push(meta_err(
                attr,
                format!(
                    "`{symbol}` is not a built-in operator token; for `#[{name}]`, a new \
                     token needs `like = \"<existing token>\"`"
                ),
            ));
            return None;
        }
    };
    Some((fixity, symbol, level, swap))
}
