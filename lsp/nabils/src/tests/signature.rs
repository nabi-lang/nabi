//! Signature help.

#[allow(clippy::wildcard_imports)]
use crate::tests::*;

/// The signature label and active parameter of reply `id`.
pub(crate) fn active(msgs: &[Json], id: u32) -> (String, u32) {
    let r = reply(msgs, id);
    let Json::Arr(sigs) = r.get("signatures") else {
        panic!("signature help {id}: {r}")
    };
    (
        sigs[0].get("label").as_str().unwrap().to_owned(),
        r.get("activeParameter").as_u32().unwrap(),
    )
}

#[test]
fn signature_help_active_param() {
    let src = "\
/// Adds three.
fn add3(a: i32, b: i32, c: i32) -> i32 { a + b + c }
let s = add3(1, [2, 3].len() as i32,
";
    let msgs = std_session("lsp-sig-fn", src, |uri| {
        vec![
            request(1, "textDocument/signatureHelp", at(uri, 2, 13)),
            request(2, "textDocument/signatureHelp", at(uri, 2, 21)),
            request(3, "textDocument/signatureHelp", at(uri, 2, 37)),
            request(4, "textDocument/signatureHelp", at(uri, 2, 4)),
        ]
    });
    let (label, n) = active(&msgs, 1);
    assert_eq!(label, "fn add3(a: i32, b: i32, c: i32) -> i32");
    assert_eq!(n, 0);
    // Inside `[2, 3]` the parameter is still `b`.
    assert_eq!(active(&msgs, 2).1, 1);
    assert_eq!(active(&msgs, 3).1, 2);
    let r = reply(&msgs, 1);
    let Json::Arr(sigs) = r.get("signatures") else {
        panic!("{r}")
    };
    let Json::Arr(params) = sigs[0].get("parameters") else {
        panic!("{r}")
    };
    assert_eq!(params.len(), 3);
    assert_eq!(
        params[1].get("label"),
        &Json::Arr(vec![Json::num(16), Json::num(22)])
    );
    let doc = sigs[0].get("documentation").get("value").as_str();
    assert!(doc.is_some_and(|d| d.contains("Adds three")), "{r}");
    assert_eq!(reply(&msgs, 4), Json::Null);
}

/// A method by its receiver's type: `self` is not an argument.
#[test]
fn signature_help_method() {
    let src = format!("{LIST}xs.insert(0, \n");
    let msgs = std_session("lsp-sig-method", &src, |uri| {
        vec![
            request(1, "textDocument/signatureHelp", at(uri, 3, 10)),
            request(2, "textDocument/signatureHelp", at(uri, 3, 13)),
        ]
    });
    let (label, n) = active(&msgs, 1);
    assert!(
        label.contains("fn insert(self, idx: usize, x: T)"),
        "{label}"
    );
    assert_eq!(n, 1);
    assert_eq!(active(&msgs, 2).1, 2);
}
