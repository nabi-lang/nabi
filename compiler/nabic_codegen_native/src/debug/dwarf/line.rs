//! The `.debug_line` program.

#[allow(clippy::wildcard_imports)]
use crate::debug::dwarf::*;

/// `.debug_line`: one sequence per function, rows `unit`-byte aligned
/// (the instruction size, 1 on `x86_64`).
pub(crate) fn line_program(
    fns: &[&FnRange],
    files: &[PathBuf],
    dir_of: &dyn Fn(&Path) -> String,
    fid: &dyn Fn(u32) -> u32,
    (unit, aw, be): (u32, usize, bool),
) -> (Vec<u8>, Vec<Reloc>) {
    let mut b = Vec::new();
    let mut relocs = Vec::new();
    b.extend_from_slice(&[0; 4]);
    b.extend_from_slice(&int(be, 4, 2));
    let hl = b.len();
    b.extend_from_slice(&[0; 4]);
    let header_start = b.len();
    b.extend_from_slice(&[unit as u8, 1, 1]); // min inst length, max ops, default_is_stmt
    b.push((-5i8) as u8);
    b.push(14);
    b.push(13);
    b.extend_from_slice(&[0, 1, 1, 1, 1, 0, 0, 0, 1, 0, 0, 1]);
    let mut dirs: Vec<String> = Vec::new();
    let mut entries = Vec::new();
    for f in files {
        let d = dir_of(f);
        let k = dirs.iter().position(|x| *x == d).unwrap_or_else(|| {
            dirs.push(d.clone());
            dirs.len() - 1
        });
        entries.push((
            f.file_name()
                .map_or_else(String::new, |n| n.to_string_lossy().into_owned()),
            k + 1,
        ));
    }
    for d in &dirs {
        cstr(&mut b, d);
    }
    b.push(0);
    for (n, d) in &entries {
        cstr(&mut b, n);
        uleb(&mut b, *d as u64);
        b.extend_from_slice(&[0, 0]);
    }
    b.push(0);
    let header_len = (b.len() - header_start) as u32;
    b[hl..hl + 4].copy_from_slice(&int(be, u64::from(header_len), 4));
    for f in fns {
        let (d, _) = f.debug.as_ref().expect("debug");
        // DW_LNE_set_address
        b.extend_from_slice(&[0, 1 + aw as u8, 2]);
        relocs.push(Reloc {
            off: b.len() as u32,
            kind: RelKind::Abs64,
            sym: f.sym,
        });
        b.extend_from_slice(&int(be, u64::from(f.start), aw));
        // DW_LNS_set_file
        b.push(4);
        uleb(&mut b, u64::from(fid(d.file)));
        let (mut addr, mut line) = (0u32, 1i64);
        let mut rows = vec![(0u32, d.line, 1u32, 0u32)];
        rows.extend(f.lines.iter().copied());
        // Optimized: rows of inlined bodies name their file; a row is a
        // statement where the line changes; the first after the
        // prologue ends it.
        let (mut file, mut stmt, mut last) = (fid(d.file), true, (0, 0));
        for (n, (a, l, c, sc)) in rows.into_iter().enumerate() {
            if a > addr {
                b.push(2); // advance_pc
                uleb(&mut b, u64::from((a - addr) / unit));
                addr = a;
            }
            let fl = if d.inlined.is_empty() {
                file
            } else {
                fid(d.file_of(sc))
            };
            if fl != file {
                b.push(4); // set_file
                uleb(&mut b, u64::from(fl));
                file = fl;
            }
            if i64::from(l) != line {
                b.push(3); // advance_line
                sleb(&mut b, i64::from(l) - line);
                line = i64::from(l);
            }
            b.push(5); // set_column
            uleb(&mut b, u64::from(c));
            if d.opt {
                if (fl, l) == last || n == 0 {
                    // The function's row, or the same line again.
                    if stmt && n != 0 {
                        b.push(6); // negate_stmt
                        stmt = false;
                    }
                } else if !stmt {
                    b.push(6);
                    stmt = true;
                }
                if n == 1 {
                    b.push(10); // set_prologue_end
                }
                last = (fl, l);
            }
            b.push(1); // copy
        }
        if f.len > addr {
            b.push(2);
            uleb(&mut b, u64::from((f.len - addr) / unit));
        }
        b.extend_from_slice(&[0, 1, 1]); // end_sequence
    }
    let total = b.len() as u32 - 4;
    b[..4].copy_from_slice(&int(be, u64::from(total), 4));
    (b, relocs)
}
