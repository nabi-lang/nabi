//! `#if` expressions.

#[allow(clippy::wildcard_imports)]
use crate::*;

impl Pp<'_> {
    /// In `#if`: `defined X` and `__has_include(..)` before expansion.
    pub(crate) fn if_operator(&mut self, t: Tok) -> Result<Option<Tok>, Error> {
        let v = if t.sym == kw::DEFINED {
            let mut x = self.raw()?;
            let paren = self.is(x, "(");
            if paren {
                x = self.raw()?;
            }
            if x.kind != Kind::Ident {
                return Err(self.err(t.loc, "macro name must be an identifier"));
            }
            if paren {
                let c = self.raw()?;
                if !self.is(c, ")") {
                    return Err(self.err(c.loc, "missing ')' after 'defined'"));
                }
            }
            i64::from(self.macros.contains_key(&x.sym))
        } else if t.sym == kw::HAS_INCLUDE || t.sym == kw::HAS_INCLUDE_NEXT {
            let args = self.paren_args(t)?;
            let args = if args.first().is_some_and(|a| a.kind == Kind::Ident) {
                self.expand_isolated(args)?
            } else {
                args
            };
            let text = if args.len() == 1 && args[0].kind == Kind::Str {
                self.spell(args[0]).to_owned()
            } else {
                args.iter().map(|a| self.spell(*a)).collect::<String>()
            };
            if text.len() < 2 {
                return Err(self.err(t.loc, "expected \"FILENAME\" or <FILENAME>"));
            }
            let angled = text.starts_with('<');
            let name = text[1..text.len() - 1].to_owned();
            i64::from(
                self.find(&name, angled, t.sym == kw::HAS_INCLUDE_NEXT)
                    .is_some(),
            )
        } else {
            return Ok(None);
        };
        let mut r = tok(Kind::Num, self.names.intern(&v.to_string()), t.loc);
        r.flags = t.flags & SPACE;
        Ok(Some(r))
    }

    pub(crate) fn eval_if(&mut self, l: &[Tok], loc: Loc) -> Result<bool, Error> {
        if l.is_empty() {
            return Err(self.err(loc, "#if with no expression"));
        }
        self.in_if = true;
        let toks = self.expand_isolated(l.to_vec());
        self.in_if = false;
        let toks = toks?;
        let mut e = IfExpr {
            pp: self,
            toks: &toks,
            pos: 0,
            loc,
        };
        let v = e.cond()?;
        if let Some(t) = toks.get(e.pos) {
            let t = *t;
            return Err(self.err(
                t.loc,
                "token is not a valid binary operator in a preprocessor subexpression",
            ));
        }
        Ok(v.0 != 0)
    }
}

/// `#if` arithmetic: `intmax_t` / `uintmax_t` values.
pub(crate) struct IfExpr<'a, 'p, 't> {
    pp: &'p Pp<'t>,
    toks: &'a [Tok],
    pos: usize,
    loc: Loc,
}

pub(crate) type Val = (i64, bool);

impl IfExpr<'_, '_, '_> {
    pub(crate) fn peek(&self) -> Option<&str> {
        self.toks
            .get(self.pos)
            .filter(|t| t.kind == Kind::Punct)
            .map(|t| self.pp.spell(*t))
    }

    pub(crate) fn err(&self, msg: &str) -> Error {
        let loc = self.toks.get(self.pos).map_or(self.loc, |t| t.loc);
        self.pp.err(loc, msg)
    }

    pub(crate) fn cond(&mut self) -> Result<Val, Error> {
        let c = self.binary(0)?;
        if self.peek() == Some("?") {
            self.pos += 1;
            let a = self.cond()?;
            if self.peek() != Some(":") {
                return Err(self.err("expected ':' in preprocessor expression"));
            }
            self.pos += 1;
            let b = self.cond()?;
            let u = a.1 || b.1;
            return Ok(if c.0 != 0 { (a.0, u) } else { (b.0, u) });
        }
        Ok(c)
    }

    pub(crate) fn binary(&mut self, min: u8) -> Result<Val, Error> {
        let mut l = self.unary()?;
        loop {
            let Some(op) = self.peek() else {
                return Ok(l);
            };
            let prec = match op {
                "*" | "/" | "%" => 10,
                "+" | "-" => 9,
                "<<" | ">>" => 8,
                "<" | ">" | "<=" | ">=" => 7,
                "==" | "!=" => 6,
                "&" => 5,
                "^" => 4,
                "|" => 3,
                "&&" => 2,
                "||" => 1,
                _ => return Ok(l),
            };
            if prec <= min {
                return Ok(l);
            }
            let op = op.to_owned();
            self.pos += 1;
            let r = self.binary(prec)?;
            let u = l.1 || r.1;
            let (a, b) = (l.0, r.0);
            let cmp = |x: bool| (i64::from(x), false);
            l = match op.as_str() {
                "*" => (a.wrapping_mul(b), u),
                "/" | "%" => {
                    if b == 0 {
                        // Only an error where evaluated: leave it 0.
                        l = (0, u);
                        continue;
                    }
                    let v = match (u, op == "/") {
                        (true, true) => ((a as u64) / (b as u64)) as i64,
                        (true, false) => ((a as u64) % (b as u64)) as i64,
                        (false, true) => a.wrapping_div(b),
                        (false, false) => a.wrapping_rem(b),
                    };
                    (v, u)
                }
                "+" => (a.wrapping_add(b), u),
                "-" => (a.wrapping_sub(b), u),
                "<<" => (a.wrapping_shl(b as u32), l.1),
                ">>" => {
                    if l.1 {
                        (((a as u64) >> (b as u32 & 63)) as i64, true)
                    } else {
                        (a.wrapping_shr(b as u32), false)
                    }
                }
                "<" => cmp(if u { (a as u64) < (b as u64) } else { a < b }),
                ">" => cmp(if u { (a as u64) > (b as u64) } else { a > b }),
                "<=" => cmp(if u { (a as u64) <= (b as u64) } else { a <= b }),
                ">=" => cmp(if u { (a as u64) >= (b as u64) } else { a >= b }),
                "==" => cmp(a == b),
                "!=" => cmp(a != b),
                "&" => (a & b, u),
                "^" => (a ^ b, u),
                "|" => (a | b, u),
                "&&" => cmp(a != 0 && b != 0),
                _ => cmp(a != 0 || b != 0),
            };
        }
    }

    pub(crate) fn unary(&mut self) -> Result<Val, Error> {
        let Some(&t) = self.toks.get(self.pos) else {
            return Err(self.err("expected value in expression"));
        };
        self.pos += 1;
        match t.kind {
            Kind::Num => {
                let s = self.pp.spell(t);
                let Some((v, u)) = parse_int(s) else {
                    return Err(self.pp.err(
                        t.loc,
                        &format!("invalid integer `{s}` in preprocessor expression"),
                    ));
                };
                Ok((v as i64, u || v > i64::MAX as u64))
            }
            Kind::Char => {
                let s = self.pp.spell(t);
                char_value(s, self.pp.target.char_signed)
                    .map(|v| (v, false))
                    .ok_or_else(|| self.pp.err(t.loc, "invalid character literal"))
            }
            // Identifiers left after expansion are 0.
            Kind::Ident => Ok((0, false)),
            Kind::Punct => match self.pp.spell(t) {
                "(" => {
                    let v = self.cond()?;
                    while self.peek() == Some(",") {
                        self.pos += 1;
                        self.cond()?;
                    }
                    if self.peek() != Some(")") {
                        return Err(self.err("expected ')' in preprocessor expression"));
                    }
                    self.pos += 1;
                    Ok(v)
                }
                "-" => self.unary().map(|v| (v.0.wrapping_neg(), v.1)),
                "+" => self.unary(),
                "~" => self.unary().map(|v| (!v.0, v.1)),
                "!" => self.unary().map(|v| (i64::from(v.0 == 0), false)),
                _ => Err(self
                    .pp
                    .err(t.loc, "invalid token at start of a preprocessor expression")),
            },
            _ => Err(self
                .pp
                .err(t.loc, "invalid token at start of a preprocessor expression")),
        }
    }
}
