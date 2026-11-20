//! The debugger formatters the toolchain ships (library/lldb, library/gdb,
//! library/natvis, over library/debug/layout.py) against the library's
//! collections.

use crate::common::workspace;
use crate::native::debug::{debugger, lldb};
#[allow(clippy::wildcard_imports)]
use crate::native::*;

const FORMATTEE: &str = "use core::collections::{SortedDict, SortedSet};
use core::array::Array;

fn main() {
    let xs: List<i64> = [1, 2, 3];
    let m: Dict<str, i64> = Dict::new();
    m.insert(\"a\", 1);
    m.insert(\"bb\", 2);
    m.insert(\"c\", 3);
    m.remove(\"a\");
    let st: Set<i64> = Set::new();
    st.insert(4);
    let sd: SortedDict<i64, i64> = SortedDict::new();
    for i in 0..40 {
        sd.insert(39 - i, i);
    }
    let ss: SortedSet<i64> = SortedSet::new();
    ss.insert(9);
    let ar: Array<f64, 2> = [[1.5, 2.5], [3.5, 4.5]];
    let nested: List<List<i64>> = [[7], []];
    show(xs, \"q\\\"x\", m, st, sd, ss, ar, xs.slice(1..3), nested, none);
}

#[inline(never)]
fn show(xs: List<i64>, s: str, m: Dict<str, i64>, st: Set<i64>, sd: SortedDict<i64, i64>, ss: SortedSet<i64>, ar: Array<f64, 2>, sl: Slice<i64>, nested: List<List<i64>>, o: i64 | none) {
    println(f\"{xs.len()} {s} {m.len()} {st.len()} {sd.len()} {ss.len()} {ar.shape():?} {sl.len()} {nested.len()} {o == none}\");
}
";

/// Under gdb and lldb (where they run the program), each collection
/// shows its size and elements, `str` its text, a `none` just `none`.
#[test]
fn formatters_show_collections() {
    let env = setup();
    let dir = scratch("formatters");
    let src = dir.join("main.nabi");
    fs::write(&src, FORMATTEE).unwrap();
    let exe = dir.join("out");
    ok(&build(
        &env,
        &src,
        &["-O0", "-g", "-o", exe.to_str().unwrap()],
    ));
    let at = "main.nabi:26";
    let init = Command::new(NABIC)
        .args(["--print", "gdbinit"])
        .output()
        .unwrap();
    ok(&init);
    let mut c = Command::new("gdb");
    c.args(["-batch", "-nx", "-iex", text(&init.stdout).trim()]);
    let cmds = [&format!("break {at}")[..], "run", "info args"];
    if let Some(out) =
        debugger(c, &exe, &cmds, "-ex", "kill").filter(|o| o.contains("Breakpoint 1,"))
    {
        for want in [
            "xs = size=3 = {1, 2, 3}",
            "s = \"q\\\"x\"",
            "m = size=2 = {[\"bb\"] = 2, [\"c\"] = 3}",
            "st = size=1 = {4}",
            "sd = size=40 = {[0] = 39, [1] = 38,",
            "[39] = 0}",
            "ss = size=1 = {9}",
            "ar = shape=[2, 2] = {[0, 0] = 1.5, [0, 1] = 2.5, [1, 0] = 3.5, [1, 1] = 4.5}",
            "sl = size=2 = {2, 3}",
            "nested = size=2 = {size=1 = {7}, size=0}",
            "o = i64 | none::none",
        ] {
            assert!(out.contains(want), "gdb: no `{want}`:\n{out}");
        }
    }
    let Some(out) = lldb(
        &exe,
        &[
            &format!("b {at}")[..],
            "run",
            "frame variable xs s m st sd[39] ss ar sl nested o",
        ],
    ) else {
        return;
    };
    for want in [
        "xs = size=3 {\n  [0] = 1\n  [1] = 2\n  [2] = 3\n}",
        "(str) s = \"q\\\"x\"",
        "m = size=2 {\n  [\"bb\"] = 2\n  [\"c\"] = 3\n}",
        "st = size=1 {\n  [0] = 4\n}",
        "sd[39] = 0",
        "ss = size=1 {\n  [0] = 9\n}",
        "ar = shape=[2, 2] {\n  [0, 0] = 1.5\n",
        "sl = size=2 {\n  [0] = 2\n  [1] = 3\n}",
        "nested = size=2 {\n  [0] = size=1 {\n    [0] = 7\n  }\n  [1] = size=0\n}",
        "(i64 | none) o = none\n",
    ] {
        assert!(out.contains(want), "lldb: no `{want}`:\n{out}");
    }
}

/// The formatters' view of the library: the types they match exist, the
/// B-tree node size is the library's, and the layout walks (run by
/// python3 where there is one) give the library's orders.
#[test]
fn formatters_match_the_library() {
    let lib = workspace().join("library");
    let read = |p: &str| fs::read_to_string(lib.join(p)).unwrap();
    let layout = read("debug/layout.py");
    let natvis = read("natvis/nabi.natvis");
    let sorted = read("core/src/collections/sorted.nabi");
    assert!(
        sorted.contains("const MIN: usize = 16;"),
        "the B-tree changed"
    );
    assert!(layout.contains("\nFULL = 31\n"));
    assert!(natvis.contains("root * 31 + i") && natvis.contains("root * 32"));
    for (path, file) in [
        (
            "core::collections::list::List",
            "core/src/collections/list.nabi",
        ),
        (
            "core::collections::dict::Dict",
            "core/src/collections/dict.nabi",
        ),
        (
            "core::collections::set::Set",
            "core/src/collections/set.nabi",
        ),
        (
            "core::collections::sorted::SortedDict",
            "core/src/collections/sorted.nabi",
        ),
        (
            "core::collections::sorted::SortedSet",
            "core/src/collections/sorted.nabi",
        ),
        (
            "core::collections::slice::Slice",
            "core/src/collections/slice.nabi",
        ),
        ("core::array::Array", "core/src/array.nabi"),
    ] {
        let name = path.rsplit("::").next().unwrap();
        assert!(
            read(file).contains(&format!("pub struct {name}<")),
            "no `{path}` in {file}"
        );
        assert!(layout.contains(path), "layout.py does not match `{path}`");
        assert!(
            natvis.contains(&format!("{path}&lt;")),
            "nabi.natvis does not match `{path}`"
        );
    }
    let check = "import importlib.util, sys\n\
        s = importlib.util.spec_from_file_location('l', sys.argv[1])\n\
        L = importlib.util.module_from_spec(s); s.loader.exec_module(L)\n\
        m = {1000: 1, 1008: 2, 1016: 2, 3000: 1, 3008: 2}\n\
        lf = {2000: 0, 2001: 1, 2002: 1}\n\
        assert L.sorted_slots(0, 5, 3000, 1000, 2000, m.get, lf.get, 8) == [31, 32, 0, 62, 63]\n\
        hs = {100: 5, 108: 0, 116: 7}\n\
        assert L.dict_live(3, 100, hs.get) == [0, 2]\n\
        assert L.array_order([2, 2], [1, 2], 1) == [((0, 0), 1), ((0, 1), 3), ((1, 0), 2), ((1, 1), 4)]\n\
        assert L.text(b'a\"\\n', 3) == '\"a\\\\\"\\\\n\"'\n\
        assert L.kind('core::collections::dict::Dict<str, i64, RandomState>') == 'dict'\n\
        assert not L.sane(5, 4) and L.sane(0, 0)\n";
    let Ok(out) = Command::new("python3")
        .args(["-I", "-B", "-c", check])
        .arg(lib.join("debug/layout.py"))
        .output()
    else {
        return;
    };
    ok(&out);
}
