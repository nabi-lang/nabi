//! Runtime functions run at compile time, computed as `nabir` does.

#[allow(clippy::wildcard_imports)]
use crate::*;

/// Byte size of a primitive, `()` or an array of them; only the pointer
/// width matters.
pub(crate) fn plain_size(ty: &HirTy) -> Option<u32> {
    let word = pointer_width() / 8;
    Some(match ty {
        HirTy::Unit => 0,
        HirTy::Prim(PrimTy::Str) => 2 * word,
        HirTy::Prim(p) => p.bit_width(pointer_width()) / 8,
        HirTy::Array(e, n) => plain_size(e)?.checked_mul(*n)?,
        _ => return None,
    })
}

/// Runtime functions evaluable at compile time: the pure ones core uses,
/// sharing the runtime's own code where nontrivial (`#[path]`), so a
/// constant equals the run-time result. Not libm's inexact ones
/// (the target's may round differently) nor the `int` kernels.
pub(crate) const EVALUABLE: &[&str] = &[
    "nabi_gc_pinned",
    "nabi_hash_keys",
    "nabi_throw_site",
    "nabi_throw_site_take",
    "nabi_str_len",
    "nabi_str_char_count",
    "nabi_str_char_count_parked",
    "nabi_str_byte",
    "nabi_str_cmp",
    "nabi_str_cmp_parked",
    "nabi_str_bytes_buf",
    "nabi_str_eq",
    "nabi_str_eq_parked",
    "nabi_str_eq_at",
    "nabi_str_eq_at_parked",
    "nabi_str_find_near",
    "nabi_str_find_parked",
    "nabi_str_rfind_near",
    "nabi_str_rfind_parked",
    "nabi_str_concat",
    "nabi_str_slice",
    "nabi_str_replace",
    "nabi_str_from_char",
    "nabi_str_from_buf",
    "nabi_str_case",
    "nabi_str_casefold",
    "nabi_str_normalize",
    "nabi_str_is_normalized",
    "nabi_str_grapheme_end",
    "nabi_str_grapheme_start",
    "nabi_str_grapheme_count",
    "nabi_str_grapheme_count_parked",
    "nabi_str_word_end",
    "nabi_str_trim",
    "nabi_str_parse_int",
    "nabi_str_parse_float",
    "nabi_str_parse_narrow",
    "nabi_buf_put_str",
    "nabi_buf_put_str_parked",
    "nabi_buf_put_char",
    "nabi_buf_copy",
    "nabi_buf_copy_parked",
    "nabi_buf_utf8_error",
    "nabi_buf_utf8_error_parked",
    "nabi_buf_slice_str",
    "nabi_char_props",
    "nabi_char_to_case",
    "nabi_fmt_new",
    "nabi_fmt_finish",
    "nabi_fmt_len",
    "nabi_fmt_str",
    "nabi_fmt_char",
    "nabi_fmt_escaped",
    "nabi_fmt_int",
    "nabi_fmt_digits",
    "nabi_fmt_integral",
    "nabi_fmt_f64",
    "nabi_fmt_pad",
    "nabi_fmt_pack",
    "nabi_fmt_spec_parse",
    "nabi_f64_floor",
    "nabi_f64_ceil",
    "nabi_f64_round",
    "nabi_f64_trunc",
    "nabi_f64_abs",
    "nabi_f64_copysign",
    "nabi_f64_min",
    "nabi_f64_max",
    "nabi_f64_mul_add",
    "nabi_f32_floor",
    "nabi_f32_ceil",
    "nabi_f32_round",
    "nabi_f32_trunc",
    "nabi_f32_abs",
    "nabi_f32_copysign",
    "nabi_f32_min",
    "nabi_f32_max",
    "nabi_f32_mul_add",
    "nabi_f16_from_f32",
    "nabi_f16_to_f32",
    "nabi_f16_from_f64",
    "nabi_bf16_from_f64",
    "nabi_bf16_from_i64",
    "nabi_bf16_from_u64",
];

/// Whether extern `name` runs at compile time (`run_extern`, or a
/// panic, which fails the evaluation).
pub fn evaluable_extern(name: &str) -> bool {
    panics(name) || EVALUABLE.contains(&name)
}

/// A runtime function that panics: a failed `try!` is one.
pub(crate) fn panics(name: &str) -> bool {
    name.starts_with("nabi_panic") || name == "nabi_try_failed"
}

/// What a runtime function did at compile time.
pub(crate) enum Ext {
    /// Arguments it cannot take here (a cut inside a char, ..).
    No,
    /// It panicked with this message.
    Panic(String),
}

impl From<std::num::TryFromIntError> for Ext {
    fn from(_: std::num::TryFromIntError) -> Self {
        Self::No
    }
}

/// Normalization scratch.
pub(crate) struct Items(Vec<u64>);

impl norm::Buf for Items {
    fn push(&mut self, x: u64) {
        self.0.push(x);
    }
    fn items(&mut self) -> &mut [u64] {
        &mut self.0
    }
    fn clear(&mut self) {
        self.0.clear();
    }
}

/// A text being written (`fmt_text::Text`).
pub(crate) struct Out(Vec<u8>);

impl std::fmt::Write for Out {
    fn write_str(&mut self, s: &str) -> std::fmt::Result {
        self.0.extend_from_slice(s.as_bytes());
        Ok(())
    }
}

impl fmt_text::Text for Out {
    fn len(&self) -> usize {
        self.0.len()
    }
    fn put(&mut self, b: &[u8]) {
        self.0.extend_from_slice(b);
    }
    fn bytes_mut(&mut self) -> &mut [u8] {
        &mut self.0
    }
    fn truncate(&mut self, len: usize) {
        self.0.truncate(len);
    }
}

pub(crate) fn int_of(v: &Value) -> Result<i128, Ext> {
    match v {
        Value::Int(i, _) => Ok(*i),
        Value::Char(c) => Ok(i128::from(*c)),
        _ => Err(Ext::No),
    }
}

pub(crate) fn usize_of(v: &Value) -> Result<usize, Ext> {
    Ok(usize::try_from(int_of(v)?)?)
}

/// A primitive's bits as `u64` (the packed spec words, `i64` values).
#[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
pub(crate) fn bits_of(v: &Value) -> Result<u64, Ext> {
    Ok(int_of(v)? as u64)
}

pub(crate) fn str_of(v: &Value) -> Result<&str, Ext> {
    match v {
        Value::Str(s) => Ok(s),
        _ => Err(Ext::No),
    }
}

pub(crate) const fn float_of(v: &Value) -> Result<f64, Ext> {
    match v {
        Value::Float(x, _) => Ok(*x),
        _ => Err(Ext::No),
    }
}

pub(crate) const fn buf_of(v: &Value) -> Result<&Rc<RefCell<Vec<Value>>>, Ext> {
    match v {
        Value::Buf(b) => Ok(b),
        _ => Err(Ext::No),
    }
}

/// `buf[off..off + len]` of a byte buffer.
pub(crate) fn bytes_of(v: &Value, off: usize, len: usize) -> Result<Vec<u8>, Ext> {
    let items = buf_of(v)?.borrow();
    let end = off.checked_add(len).ok_or(Ext::No)?;
    items
        .get(off..end)
        .ok_or(Ext::No)?
        .iter()
        .map(|b| Ok(u8::try_from(int_of(b)?)?))
        .collect()
}

/// Store `bits` at `buf[i]`, in the element's own type.
#[allow(clippy::cast_possible_wrap)]
pub(crate) fn put_bits(buf: &Value, i: usize, bits: u64) -> Result<(), Ext> {
    let mut items = buf_of(buf)?.borrow_mut();
    let slot = items.get_mut(i).ok_or(Ext::No)?;
    *slot = match slot {
        Value::Int(_, p) => Value::Int(wrap(i128::from(bits), *p), *p),
        Value::Float(_, PrimTy::F32) => {
            Value::Float(f64::from(f32::from_bits(bits as u32)), PrimTy::F32)
        }
        Value::Float(_, p) => Value::Float(f64::from_bits(bits), *p),
        _ => return Err(Ext::No),
    };
    Ok(())
}

pub(crate) fn put_bytes(buf: &Value, off: usize, bytes: &[u8]) -> Result<(), Ext> {
    for (i, b) in bytes.iter().enumerate() {
        put_bits(buf, off + i, u64::from(*b))?;
    }
    Ok(())
}

/// A text (`str` bytes) back as a value; not UTF-8 is not a `str` this
/// evaluator holds.
pub(crate) fn text_value(b: Vec<u8>) -> Result<Value, Ext> {
    String::from_utf8(b)
        .map(|s| Value::Str(s.into()))
        .map_err(|_| Ext::No)
}

pub(crate) const fn usize_value(n: usize) -> Value {
    Value::Int(n as i128, PrimTy::Usize)
}

pub(crate) fn i32_value(n: i32) -> Value {
    Value::Int(i128::from(n), PrimTy::I32)
}

/// Extern `name` on `args`, computed as `nabir` does.
#[allow(clippy::cast_possible_truncation, clippy::cast_possible_wrap)]
pub(crate) fn run_extern(name: &str, args: &[Value]) -> Result<Value, Ext> {
    // The f-string writers: the text so far, the packed spec words.
    let text = |t: &Value, write: &dyn Fn(&mut Out) -> Result<(), Ext>| {
        let mut out = Out(str_of(t)?.as_bytes().to_vec());
        write(&mut out)?;
        text_value(out.0)
    };
    let spec =
        |flags: &Value, counts: &Value| Ok::<_, Ext>(unpack(bits_of(flags)?, bits_of(counts)?));
    let f32_op = |x: &Value, op: fn(f32) -> f32| -> Result<Value, Ext> {
        Ok(Value::Float(
            f64::from(op(float_of(x)? as f32)),
            PrimTy::F32,
        ))
    };
    let f32_op2 = |x: &Value, y: &Value, op: fn(f32, f32) -> f32| -> Result<Value, Ext> {
        let r = op(float_of(x)? as f32, float_of(y)? as f32);
        Ok(Value::Float(f64::from(r), PrimTy::F32))
    };
    let f64_op = |x: &Value, op: fn(f64) -> f64| -> Result<Value, Ext> {
        Ok(Value::Float(op(float_of(x)?), PrimTy::F64))
    };
    let f64_op2 = |x: &Value, y: &Value, op: fn(f64, f64) -> f64| -> Result<Value, Ext> {
        Ok(Value::Float(op(float_of(x)?, float_of(y)?), PrimTy::F64))
    };
    Ok(match (name, args) {
        ("nabi_str_len", [s]) => usize_value(str_of(s)?.len()),
        ("nabi_str_char_count" | "nabi_str_char_count_parked", [s]) => {
            usize_value(str_of(s)?.chars().count())
        }
        ("nabi_str_byte", [s, i]) => {
            let b = str_of(s)?.as_bytes().get(usize_of(i)?).copied();
            Value::Int(i128::from(b.unwrap_or(0)), PrimTy::U8)
        }
        // The reserved all-zero key: a `RandomState` built at compile
        // time takes the process's keys at run time.
        // Nothing is pinned at compile time.
        ("nabi_gc_pinned", [_]) => Value::Int(0, PrimTy::I32),
        ("nabi_hash_keys", [out]) => {
            put_bits(out, 0, 0)?;
            put_bits(out, 1, 0)?;
            Value::Unit
        }
        // A copy of the bytes: the view is read-only.
        ("nabi_str_bytes_buf", [s]) => Value::Buf(Rc::new(RefCell::new(
            str_of(s)?
                .bytes()
                .map(|b| Value::Int(i128::from(b), PrimTy::U8))
                .collect(),
        ))),
        ("nabi_str_eq" | "nabi_str_eq_parked", [a, b]) => {
            i32_value(i32::from(str_of(a)? == str_of(b)?))
        }
        ("nabi_str_cmp" | "nabi_str_cmp_parked", [a, b]) => {
            i32_value(str_of(a)?.as_bytes().cmp(str_of(b)?.as_bytes()) as i32)
        }
        ("nabi_str_eq_at" | "nabi_str_eq_at_parked", [s, at, pat]) => {
            let hit = str_ops::eq_at(
                str_of(s)?.as_bytes(),
                usize_of(at)?,
                str_of(pat)?.as_bytes(),
            );
            i32_value(i32::from(hit))
        }
        (
            "nabi_str_find_near"
            | "nabi_str_find_parked"
            | "nabi_str_rfind_near"
            | "nabi_str_rfind_parked",
            [s, pat, near],
        ) => {
            let (h, n) = (str_of(s)?.as_bytes(), str_of(pat)?.as_bytes());
            let near = usize_of(near)?;
            let at = match name {
                "nabi_str_find_near" => str_ops::find_near(h, n, near),
                "nabi_str_find_parked" => str_ops::find_far(h, n, near),
                "nabi_str_rfind_near" => str_ops::rfind_near(h, n, near),
                _ => str_ops::rfind_far(h, n, near),
            };
            Value::Int(at as i128, PrimTy::Isize)
        }
        // No thread runs at compile time: nothing to note.
        ("nabi_throw_site", [_]) => Value::Unit,
        ("nabi_throw_site_take", []) => Value::Str(String::new().into()),
        ("nabi_str_concat", [a, b]) => Value::Str(format!("{}{}", str_of(a)?, str_of(b)?).into()),
        // The caller's position comes last (`#[track_caller]`).
        ("nabi_str_slice", [s, a, b, ..]) => {
            let (s, a, b) = (str_of(s)?, usize_of(a)?, usize_of(b)?);
            str_ops::slice_check(s.as_bytes(), a, b).map_err(|m| Ext::Panic(m.to_string()))?;
            Value::Str(s.get(a..b).ok_or(Ext::No)?.into())
        }
        ("nabi_str_replace", [s, from, to]) => {
            let mut out = Vec::new();
            let (h, f, t) = (str_of(s)?, str_of(from)?, str_of(to)?);
            str_ops::replace(h.as_bytes(), f.as_bytes(), t.as_bytes(), |b| {
                out.extend_from_slice(b);
            });
            text_value(out)?
        }
        ("nabi_str_from_char", [c]) => {
            let c =
                char::from_u32(u32::try_from(int_of(c)?)?).unwrap_or(char::REPLACEMENT_CHARACTER);
            Value::Str(c.to_string().into())
        }
        ("nabi_str_from_buf", [buf, off, len]) => {
            text_value(bytes_of(buf, usize_of(off)?, usize_of(len)?)?)?
        }
        ("nabi_str_case", [s, mode]) => {
            let mut out = String::new();
            unicode::case(str_of(s)?, i32::try_from(int_of(mode)?)?, |c| out.push(c));
            Value::Str(out.into())
        }
        ("nabi_str_casefold", [s]) => {
            let mut out = String::new();
            unicode::casefold(str_of(s)?, |c| out.push(c));
            Value::Str(out.into())
        }
        ("nabi_str_normalize", [s, form]) => {
            let form = norm::Form::from_i32(i32::try_from(int_of(form)?)?).ok_or(Ext::No)?;
            let mut out = String::new();
            norm::normalize(str_of(s)?, form, &mut Items(Vec::new()), |p| {
                out.push_str(p);
            });
            Value::Str(out.into())
        }
        ("nabi_str_is_normalized", [s, form]) => {
            let form = norm::Form::from_i32(i32::try_from(int_of(form)?)?).ok_or(Ext::No)?;
            let yes = norm::is_normalized(str_of(s)?, form, &mut Items(Vec::new()));
            i32_value(i32::from(yes))
        }
        ("nabi_str_grapheme_end" | "nabi_str_word_end", [s, pos]) => {
            let (b, pos) = (str_of(s)?.as_bytes(), usize_of(pos)?);
            usize_value(if name == "nabi_str_word_end" {
                segment::word_end(b, pos)
            } else {
                segment::grapheme_end(b, pos)
            })
        }
        ("nabi_str_grapheme_start", [s, end]) => usize_value(segment::grapheme_start(
            str_of(s)?.as_bytes(),
            usize_of(end)?,
        )),
        ("nabi_str_grapheme_count" | "nabi_str_grapheme_count_parked", [s]) => {
            usize_value(segment::grapheme_count(str_of(s)?.as_bytes()))
        }
        ("nabi_str_trim", [s, which]) => {
            let t = unicode::trim(str_of(s)?.as_bytes(), i32::try_from(int_of(which)?)?);
            text_value(t.to_vec())?
        }
        ("nabi_str_parse_int", [s, radix, signed, bits, out]) => {
            let r = str_ops::parse_int(
                str_of(s)?.as_bytes(),
                u32::try_from(int_of(radix)?)?,
                int_of(signed)? != 0,
                u32::try_from(int_of(bits)?)?,
            );
            match r {
                Ok(v) => {
                    put_bits(out, 0, v)?;
                    i32_value(0)
                }
                Err(st) => i32_value(st),
            }
        }
        ("nabi_str_parse_float", [s, out]) => match str_ops::parse_float(str_of(s)?) {
            Ok(v) => {
                put_bits(out, 0, v.to_bits())?;
                i32_value(0)
            }
            Err(st) => i32_value(st),
        },
        ("nabi_str_parse_narrow", [s, kind, out]) => {
            match str_ops::parse_narrow(str_of(s)?, u32::try_from(int_of(kind)?)?) {
                Ok(v) => {
                    put_bits(out, 0, u64::from(v))?;
                    i32_value(0)
                }
                Err(st) => i32_value(st),
            }
        }
        ("nabi_buf_put_str" | "nabi_buf_put_str_parked", [buf, off, s]) => {
            put_bytes(buf, usize_of(off)?, str_of(s)?.as_bytes())?;
            Value::Unit
        }
        ("nabi_buf_put_char", [buf, off, c]) => {
            let c =
                char::from_u32(u32::try_from(int_of(c)?)?).unwrap_or(char::REPLACEMENT_CHARACTER);
            let mut tmp = [0; 4];
            let b = c.encode_utf8(&mut tmp).as_bytes();
            put_bytes(buf, usize_of(off)?, b)?;
            usize_value(b.len())
        }
        ("nabi_buf_copy" | "nabi_buf_copy_parked", [dst, doff, src, soff, len]) => {
            let b = bytes_of(src, usize_of(soff)?, usize_of(len)?)?;
            put_bytes(dst, usize_of(doff)?, &b)?;
            Value::Unit
        }
        ("nabi_buf_utf8_error" | "nabi_buf_utf8_error_parked", [buf, off, len]) => {
            let b = bytes_of(buf, usize_of(off)?, usize_of(len)?)?;
            Value::Int(str_ops::utf8_error(&b) as i128, PrimTy::Isize)
        }
        ("nabi_buf_slice_str", [buf, len, a, b, ..]) => {
            let bytes = bytes_of(buf, 0, usize_of(len)?)?;
            let (a, b) = (usize_of(a)?, usize_of(b)?);
            str_ops::slice_check(&bytes, a, b).map_err(|m| Ext::Panic(m.to_string()))?;
            text_value(bytes[a..b].to_vec())?
        }
        ("nabi_char_props", [c]) => Value::Int(
            i128::from(unicode::props(u32::try_from(int_of(c)?)?)),
            PrimTy::U32,
        ),
        ("nabi_char_to_case", [c, mode]) => {
            let c = unicode::to_case(u32::try_from(int_of(c)?)?, i32::try_from(int_of(mode)?)?);
            Value::Int(i128::from(c), PrimTy::U32)
        }
        // A formatter's text is its `str` so far.
        ("nabi_fmt_new", [_]) => Value::Str("".into()),
        ("nabi_fmt_finish", [t]) => Value::Str(str_of(t)?.into()),
        ("nabi_fmt_len", [t]) => usize_value(str_of(t)?.len()),
        ("nabi_fmt_str", [t, s]) => Value::Str(format!("{}{}", str_of(t)?, str_of(s)?).into()),
        ("nabi_fmt_char", [t, c]) => {
            let c =
                char::from_u32(u32::try_from(int_of(c)?)?).unwrap_or(char::REPLACEMENT_CHARACTER);
            Value::Str(format!("{}{c}", str_of(t)?).into())
        }
        ("nabi_fmt_escaped", [t, s, quote]) => text(t, &|w| {
            fmt_text::escape(w, str_of(s)?, u32::try_from(int_of(quote)?)?);
            Ok(())
        })?,
        ("nabi_fmt_int", [t, flags, counts, v, signed, bits]) => text(t, &|w| {
            let v = i64::try_from(int_of(v)?)?;
            let signed = int_of(signed)? != 0;
            fmt_text::int(
                w,
                &spec(flags, counts)?,
                v,
                signed,
                u32::try_from(int_of(bits)?)?,
            );
            Ok(())
        })?,
        ("nabi_fmt_digits", [t, flags, counts, neg, d]) => text(t, &|w| {
            let neg = int_of(neg)? != 0;
            fmt_text::digits(w, &spec(flags, counts)?, neg, str_of(d)?.as_bytes());
            Ok(())
        })?,
        ("nabi_fmt_integral", [t, flags, counts, nonneg, prefix, buf]) => text(t, &|w| {
            let (prefix, buf) = (str_of(prefix)?.as_bytes(), str_of(buf)?.as_bytes());
            fmt_text::integral(w, &spec(flags, counts)?, int_of(nonneg)? != 0, prefix, buf);
            Ok(())
        })?,
        ("nabi_fmt_f64", [t, flags, counts, v, mode]) => text(t, &|w| {
            let mode = u32::try_from(int_of(mode)?)?;
            fmt_text::float_number(w, &spec(flags, counts)?, float_of(v)?, mode)
                .map_err(|m| Ext::Panic(m.to_owned()))
        })?,
        ("nabi_fmt_pad", [t, flags, counts, start, mid, numeric]) => text(t, &|w| {
            let (start, mid) = (usize_of(start)?, usize_of(mid)?);
            let numeric = int_of(numeric)? != 0;
            fmt_text::pad_span(w, &spec(flags, counts)?, start, mid, numeric);
            Ok(())
        })?,
        ("nabi_fmt_pack", [s, text_only, out]) => {
            let src = str_of(s)?;
            let (flags, counts) = fmt_text::pack_spec(src, int_of(text_only)? != 0)
                .map_err(|why| bad_spec(src, why))?;
            put_bits(out, 0, flags)?;
            put_bits(out, 1, counts)?;
            Value::Unit
        }
        ("nabi_fmt_spec_parse", [s, out]) => {
            let src = str_of(s)?;
            let words = fmt_text::spec_words(src).map_err(|why| bad_spec(src, why))?;
            for (i, w) in words.into_iter().enumerate() {
                put_bits(out, i, w as u64)?;
            }
            Value::Unit
        }
        // Exact in every libm: Rust's own give the same bits.
        ("nabi_f64_floor", [x]) => f64_op(x, f64::floor)?,
        ("nabi_f64_ceil", [x]) => f64_op(x, f64::ceil)?,
        ("nabi_f64_round", [x]) => f64_op(x, f64::round)?,
        ("nabi_f64_trunc", [x]) => f64_op(x, f64::trunc)?,
        ("nabi_f64_abs", [x]) => f64_op(x, f64::abs)?,
        ("nabi_f64_copysign", [x, y]) => f64_op2(x, y, f64::copysign)?,
        ("nabi_f64_min", [x, y]) => f64_op2(x, y, float::min_f64)?,
        ("nabi_f64_max", [x, y]) => f64_op2(x, y, float::max_f64)?,
        ("nabi_f64_mul_add", [x, y, z]) => Value::Float(
            float_of(x)?.mul_add(float_of(y)?, float_of(z)?),
            PrimTy::F64,
        ),
        ("nabi_f32_floor", [x]) => f32_op(x, f32::floor)?,
        ("nabi_f32_ceil", [x]) => f32_op(x, f32::ceil)?,
        ("nabi_f32_round", [x]) => f32_op(x, f32::round)?,
        ("nabi_f32_trunc", [x]) => f32_op(x, f32::trunc)?,
        ("nabi_f32_abs", [x]) => f32_op(x, f32::abs)?,
        ("nabi_f32_copysign", [x, y]) => f32_op2(x, y, f32::copysign)?,
        ("nabi_f32_min", [x, y]) => f32_op2(x, y, float::min_f32)?,
        ("nabi_f32_max", [x, y]) => f32_op2(x, y, float::max_f32)?,
        ("nabi_f32_mul_add", [x, y, z]) => {
            let r = (float_of(x)? as f32).mul_add(float_of(y)? as f32, float_of(z)? as f32);
            Value::Float(f64::from(r), PrimTy::F32)
        }
        ("nabi_f16_from_f32", [x]) => u16_value(half::from_f32(half::F16, float_of(x)? as f32)),
        ("nabi_f16_to_f32", [h]) => Value::Float(
            half::to_f64(half::F16, u16::try_from(int_of(h)?)?),
            PrimTy::F32,
        ),
        ("nabi_f16_from_f64", [x]) => u16_value(half::from_f64(half::F16, float_of(x)?)),
        ("nabi_bf16_from_f64", [x]) => u16_value(half::from_f64(half::BF16, float_of(x)?)),
        ("nabi_bf16_from_i64", [x]) => {
            let x = int_of(x)?;
            u16_value(half::from_int(
                half::BF16,
                x < 0,
                u64::try_from(x.unsigned_abs())?,
            ))
        }
        ("nabi_bf16_from_u64", [x]) => u16_value(half::from_int(
            half::BF16,
            false,
            u64::try_from(int_of(x)?)?,
        )),
        _ => return Err(Ext::No),
    })
}

fn u16_value(h: u16) -> Value {
    Value::Int(i128::from(h), PrimTy::U16)
}
