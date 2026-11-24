//! `f"..."` from the lexer's f-string tokens: text
//! runs and holes (`ExprKind::FString`). Hole code is ordinary tokens;
//! HIR reads the spec and formats by the hole's type.

use crate::parser::Parser;
use nabic_ast::{Expr, ExprKind, FStringPart, SpecPiece};
use nabic_diagnostic::{Code, NabiDiagnostic};
use nabic_lexer::TokenKind;

impl Parser<'_> {
    /// From `FStrStart` (current token) through `FStrEnd`.
    pub(crate) fn parse_fstring(&mut self) -> ExprKind {
        self.bump();
        let mut parts = Vec::new();
        loop {
            match self.peek().clone() {
                TokenKind::FStrText(t) => {
                    self.bump();
                    parts.push(FStringPart::Lit(t));
                }
                TokenKind::HoleStart => {
                    if let Some((expr, spec, echo)) = self.parse_hole() {
                        // `{x=}`: the text, then the Debug form (the spec's
                        // form when one is given).
                        let spec = match echo {
                            Some(text) => {
                                parts.push(FStringPart::Lit(text));
                                if spec.is_empty() {
                                    vec![SpecPiece::Text("?".to_owned())]
                                } else {
                                    spec
                                }
                            }
                            None => spec,
                        };
                        parts.push(FStringPart::Hole { expr, spec });
                    }
                }
                TokenKind::FStrEnd(_) => {
                    self.bump();
                    break;
                }
                // The lexer closes every f-string, even at EOF.
                _ => break,
            }
        }
        ExprKind::FString(parts)
    }

    /// `HoleStart expr echo? (HoleSpec (SpecText | hole)*)? HoleEnd`, the
    /// echo (`=`, lexed as the hole's text) last; on an error, skips to
    /// the hole's end.
    fn parse_hole(&mut self) -> Option<(Expr, Vec<SpecPiece>, Option<String>)> {
        let span = self.peek_span();
        self.bump();
        if self.eat(&TokenKind::HoleEnd) {
            // Nabi's hole syntax in the text, not a Rust format argument.
            #[allow(clippy::literal_string_with_formatting_args)]
            let help = "put the value in the hole: `f\"{x}\"` (values are not passed after the \
                        string)";
            self.error_help(Code::FSTRING_SYNTAX, span, "empty `{}` in f-string", help);
            return None;
        }
        let expr = self.with_struct_expr(true, Self::parse_expr);
        let mut echo = None;
        if let TokenKind::FStrText(t) = self.peek().clone() {
            self.bump();
            echo = Some(t);
        }
        let mut spec = Vec::new();
        if expr.is_some() && self.eat(&TokenKind::HoleSpec) {
            loop {
                match self.peek().clone() {
                    TokenKind::SpecText(t) => {
                        self.bump();
                        spec.push(SpecPiece::Text(t));
                    }
                    TokenKind::HoleStart => {
                        let at = self.peek_span();
                        match self.parse_hole() {
                            Some((e, s, None)) if s.is_empty() => spec.push(SpecPiece::Expr(e)),
                            Some(_) => self.error(
                                Code::FSTRING_SYNTAX,
                                at,
                                "a format spec can't have its own spec",
                            ),
                            None => {}
                        }
                    }
                    _ => break,
                }
            }
        }
        if self.eat(&TokenKind::HoleEnd) {
            return Some((expr?, spec, echo));
        }
        if let (Some(e), Some(conv)) = (&expr, self.python_conversion()) {
            let at = self.peek_span();
            let x = self.text_in(e.span);
            let help = if conv == "s" {
                format!("write `{{{x}}}`: a hole already formats with `Display`")
            } else {
                format!("write `{{{x}:?}}`: `:?` formats with `Debug`, as `repr` does")
            };
            self.error_help(
                Code::FSTRING_SYNTAX,
                at,
                format!("Nabi has no `!{conv}` conversion"),
                help,
            );
        } else if expr.is_some() {
            let at = self.peek_span();
            let d = NabiDiagnostic::error(
                Code::FSTRING_SYNTAX,
                at,
                "unexpected tokens in f-string hole (expected `}`)",
            )
            .with_file(self.file.clone());
            self.diagnostics.push(match self.bang_help() {
                Some(help) => d.with_help(help).with_fix(at, "", "delete `!`"),
                None => d,
            });
        }
        let mut depth = 0u32;
        loop {
            match self.peek() {
                TokenKind::HoleStart => depth += 1,
                TokenKind::HoleEnd if depth == 0 => {
                    self.bump();
                    return None;
                }
                TokenKind::HoleEnd => depth -= 1,
                TokenKind::Eof => return None,
                _ => {}
            }
            self.bump();
        }
    }
}
