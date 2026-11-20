//! `nabi inspect` (a prebuilt library) and `nabi demangle` (its symbols).

use nabic::{Args, fail, usage_error};
use nabic_core::cli;
use nabic_interface::Demangled;
use std::io::{Read, Write};
use std::path::Path;
use std::process;

/// `inspect [--dump] <file>`.
pub(crate) fn cmd_inspect(a: &Args) {
    let [file] = a.positional.as_slice() else {
        usage_error("usage: nabi inspect [--dump] <file.nabihir | lib<name>.a>");
    };
    let path = Path::new(file);
    let out = if a.dump {
        let nabihir = nabic_interface::nabihir::companions(path).0;
        std::fs::read(&nabihir)
            .map_err(|e| format!("{}: {e}", nabihir.display()))
            .and_then(|b| nabic_interface::nabihir::Nabihir::decode(&b))
            .map(|r| r.text())
    } else {
        nabic_interface::nabihir::inspect(path)
    };
    print!("{}", out.unwrap_or_else(|e| fail(&e)));
}

/// `demangle [-v] [<symbol>...]`: each symbol's name on a line (`-v`: the
/// parts it encodes); with no symbol, stdin to stdout with each Nabi
/// symbol in it replaced by its name, as a filter (`perf report | nabi
/// demangle`).
pub(crate) fn cmd_demangle(a: &Args) {
    if a.positional.is_empty() {
        let mut text = String::new();
        if let Err(e) = std::io::stdin().read_to_string(&mut text) {
            fail(&format!("cannot read stdin: {e}"));
        }
        let out = demangle_text(&text);
        let mut stdout = std::io::stdout().lock();
        // A closed pipe (`| head`) ends the filter quietly.
        let _ = stdout
            .write_all(out.as_bytes())
            .and_then(|()| stdout.flush());
        return;
    }
    let mut bad = false;
    for (i, sym) in a.positional.iter().enumerate() {
        let Some(d) = symbol(sym) else {
            eprintln!("{}: `{sym}` is not a Nabi symbol", cli::invoked_as());
            bad = true;
            continue;
        };
        if a.verbose == 0 {
            println!("{}", d.display_name());
            continue;
        }
        if i > 0 {
            println!();
        }
        let path = d.qualified_path();
        println!("package:  {}", d.pkg);
        println!(
            "path:     {}",
            if path.is_empty() { "(root)" } else { &path }
        );
        println!("name:     {}", d.name);
        match &d.typehash {
            Some(h) => println!("generics: {h} (type tuple hash)"),
            None => println!("generics: (none)"),
        }
    }
    if bad {
        process::exit(1);
    }
}

/// `sym` taken apart; Mach-O's extra leading `_` (`__Y..`) and a
/// `.<suffix>` (a unit's `.copy_Y<hash>_`) are accepted.
fn symbol(sym: &str) -> Option<Demangled> {
    let sym = sym.split_once('.').map_or(sym, |(s, _)| s);
    nabic_interface::demangle(sym)
        .or_else(|| sym.strip_prefix('_').and_then(nabic_interface::demangle))
}

/// `text` with each Nabi symbol in it replaced by its one-line name;
/// everything else as it was.
fn demangle_text(text: &str) -> String {
    let word = |c: char| c.is_ascii_alphanumeric() || c == '_' || c == '.' || c == '$';
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find(word) {
        out.push_str(&rest[..at]);
        rest = &rest[at..];
        let end = rest.find(|c: char| !word(c)).unwrap_or(rest.len());
        let tok = &rest[..end];
        let (head, suffix) = tok
            .split_once('.')
            .map_or((tok, ""), |(h, _)| (h, &tok[h.len()..]));
        match symbol(head).filter(|_| head.starts_with("_Y") || head.starts_with("__Y")) {
            Some(d) => {
                out.push_str(&d.display_name());
                // A unit's copy is the same function.
                if !suffix.starts_with(".copy_Y") {
                    out.push_str(suffix);
                }
            }
            None => out.push_str(tok),
        }
        rest = &rest[end..];
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::demangle_text;

    #[test]
    fn filters_symbols_in_text() {
        let nm = "0000000100006b60 t __Y0b3cd907_core_collections_dict_Dict_0_0grow_1707c745048bf996\n\
                  00000001000022d8 T _Y0b3cd907_core_array_Array_0_0len_e21da56af45cd94f.copy_Yf03649e1_\n\
                  0000000100001000 T _main\n";
        assert_eq!(
            demangle_text(nm),
            "0000000100006b60 t core::collections::dict::Dict::grow\n\
             00000001000022d8 T core::array::Array::len\n\
             0000000100001000 T _main\n"
        );
        let perf = "  12.50%  out  out  [.] _Y3f8a1c2b_geom_distance_+0x14 (x _Ynot)";
        assert_eq!(
            demangle_text(perf),
            "  12.50%  out  out  [.] geom::distance+0x14 (x _Ynot)"
        );
        assert_eq!(demangle_text("é _Y3f8a1c2b__main_\n"), "é main\n");
    }
}
