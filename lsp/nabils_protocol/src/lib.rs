//! The language server protocol's plumbing: JSON-RPC 2.0 messages with
//! `Content-Length` framing, LSP positions (UTF-16 columns) and
//! `file://` URIs.

use nabic_diagnostic::Json;
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};

/// One framed message; `None` at end of input. Unparsable bodies are
/// skipped.
pub fn read_message(input: &mut impl BufRead) -> Option<Json> {
    loop {
        let mut len = None;
        loop {
            let mut line = String::new();
            if input.read_line(&mut line).ok()? == 0 {
                return None;
            }
            let line = line.trim_end();
            if line.is_empty() {
                break;
            }
            if let Some((k, v)) = line.split_once(':') {
                if k.eq_ignore_ascii_case("content-length") {
                    len = v.trim().parse::<usize>().ok();
                }
            }
        }
        let mut body = vec![0; len?];
        input.read_exact(&mut body).ok()?;
        if let Ok(msg) = Json::parse(&String::from_utf8_lossy(&body)) {
            return Some(msg);
        }
    }
}

/// Write `msg` framed. A closed client is no error: the next read ends.
pub fn write_message(out: &mut impl Write, msg: &Json) {
    let body = msg.to_string();
    let _ = write!(out, "Content-Length: {}\r\n\r\n{body}", body.len());
    let _ = out.flush();
}

/// Byte offset of an LSP position (UTF-16 columns).
pub fn offset(text: &str, line: u32, col: u32) -> usize {
    let start = if line == 0 {
        0
    } else {
        text.match_indices('\n')
            .nth(line as usize - 1)
            .map_or(text.len(), |(i, _)| i + 1)
    };
    let mut units = 0;
    for (i, c) in text[start..].char_indices() {
        if units >= col as usize || c == '\n' {
            return start + i;
        }
        units += c.len_utf16();
    }
    text.len()
}

/// 0-based line and UTF-16 character of a byte offset.
pub fn line_col(text: &str, off: usize) -> (u32, u32) {
    let off = off.min(text.len());
    let before = &text[..off];
    let line = before.matches('\n').count() as u32;
    let line_start = before.rfind('\n').map_or(0, |i| i + 1);
    let col: u32 = before[line_start..]
        .chars()
        .map(|c| c.len_utf16() as u32)
        .sum();
    (line, col)
}

/// The LSP position (0-based line, UTF-16 character) of a byte offset.
pub fn position(text: &str, off: usize) -> Json {
    let mut off = off.min(text.len());
    while !text.is_char_boundary(off) {
        off -= 1;
    }
    let before = &text[..off];
    let line = before.matches('\n').count();
    let line_start = before.rfind('\n').map_or(0, |i| i + 1);
    let col: usize = before[line_start..].chars().map(char::len_utf16).sum();
    Json::obj([
        ("line", Json::num(line as u32)),
        ("character", Json::num(col as u32)),
    ])
}

/// The LSP range of bytes `start..end`.
pub fn range(text: &str, start: usize, end: usize) -> Json {
    Json::obj([
        ("start", position(text, start)),
        ("end", position(text, end.max(start))),
    ])
}

/// The path of a `file://` URI on this host.
pub fn uri_path(uri: &str) -> PathBuf {
    PathBuf::from(uri_to_path(uri, cfg!(windows)))
}

/// The `file://` URI of a path on this host.
pub fn path_uri(path: &Path) -> String {
    path_to_uri(&path.display().to_string(), cfg!(windows))
}

/// A `file://` URI's path; on Windows `file:///c%3A/a/b` is `C:\a\b` (the
/// drive as nabic spells it) and `file://host/share/x` is `\\host\share\x`.
pub fn uri_to_path(uri: &str, windows: bool) -> String {
    let Some(rest) = uri.strip_prefix("file://") else {
        return uri.to_owned();
    };
    let bytes = rest.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let hex = bytes
            .get(i + 1..i + 3)
            .and_then(|h| std::str::from_utf8(h).ok())
            .and_then(|h| u8::from_str_radix(h, 16).ok());
        match (bytes[i], hex) {
            (b'%', Some(b)) => {
                out.push(b);
                i += 3;
            }
            (b, _) => {
                out.push(b);
                i += 1;
            }
        }
    }
    let path = String::from_utf8_lossy(&out).into_owned();
    if !windows {
        return path;
    }
    let path = match path.as_bytes() {
        [b'/', d, b':', ..] if d.is_ascii_alphabetic() => {
            format!("{}{}", d.to_ascii_uppercase() as char, &path[2..])
        }
        [b'/', ..] => path,
        // An authority: a UNC share.
        _ => format!("//{path}"),
    };
    path.replace('/', "\\")
}

/// The `file://` URI of a path (Windows: `C:\a b\x` is
/// `file:///C:/a%20b/x`).
pub fn path_to_uri(path: &str, windows: bool) -> String {
    let path = if windows {
        let p = path.replace('\\', "/");
        match p.strip_prefix("//") {
            Some(unc) => unc.to_owned(),
            None => format!("/{p}"),
        }
    } else {
        path.to_owned()
    };
    let mut out = String::from("file://");
    for (i, b) in path.bytes().enumerate() {
        let drive_colon = windows && i == 2 && b == b':';
        if b.is_ascii_alphanumeric() || b"/-_.~".contains(&b) || drive_colon {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}
