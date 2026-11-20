//! "a" or "an" before a word spliced into a message, by how it is read.

/// "a" or "an" for `word`: "an `i64`", "a `u8`", "an enum", "a `Mutex`".
#[must_use]
pub fn a_or_an(word: &str) -> &'static str {
    let w: Vec<char> = word
        .chars()
        .skip_while(|&c| c == '`')
        .take_while(|c| c.is_alphanumeric() || *c == '_')
        .collect();
    let letter = |c: char| "AEFHILMNORSX".contains(c.to_ascii_uppercase());
    let an = match w.as_slice() {
        // Read letter by letter: `S`, `i64`, `f32`, `u8`, `FFI`.
        [c] => letter(*c),
        [c, d, ..] if d.is_ascii_digit() => letter(*c),
        [c, rest @ ..] if c.is_ascii_uppercase() && rest.iter().all(char::is_ascii_uppercase) => {
            letter(*c)
        }
        // `usize`, `union`, `unit` start with a "you"; `unsafe` does not.
        ['u' | 'U', 'n', c, ..] => !matches!(c, 'i' | 'I'),
        ['u' | 'U', ..] | [] => false,
        [c, ..] => "AEIO".contains(c.to_ascii_uppercase()),
    };
    if an { "an" } else { "a" }
}

/// `ty` in backticks with its article: "a `File`", "an `S`", "an `i64`".
#[must_use]
pub fn article(ty: &str) -> String {
    format!("{} `{ty}`", a_or_an(ty))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_words_and_letters() {
        for (w, a) in [
            ("i64", "an"),
            ("f64", "an"),
            ("u8", "a"),
            ("usize", "a"),
            ("isize", "an"),
            ("str", "a"),
            ("S", "an"),
            ("T", "a"),
            ("S | none", "an"),
            ("Option", "an"),
            ("enum", "an"),
            ("impl", "an"),
            ("union", "a"),
            ("unsafe fn()", "an"),
            ("File", "a"),
            ("FFI", "an"),
            ("[i32; 3]", "a"),
        ] {
            assert_eq!(a_or_an(w), a, "{w}");
        }
        assert_eq!(article("i64"), "an `i64`");
    }
}
