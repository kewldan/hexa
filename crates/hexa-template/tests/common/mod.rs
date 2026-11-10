#![allow(dead_code)]

use hexa_core::piece::Piece;
use hexa_core::{Cancel, Tree};
use hexa_template::{
    Dialect, EvalOptions, Node, ParseOptions, Program, Severity, TemplateResult, evaluate,
    parse_with,
};

pub fn tree(bytes: &[u8]) -> Tree {
    if bytes.is_empty() {
        return Tree::new();
    }
    Tree::from_piece(Piece::bytes(bytes.to_vec()))
}

pub fn program(src: &str, dialect: Dialect) -> Program {
    let (p, d) = parse_with(
        src,
        &ParseOptions {
            dialect,
            ..ParseOptions::default()
        },
    );
    let errors: Vec<_> = d.iter().filter(|d| d.severity == Severity::Error).collect();
    assert!(errors.is_empty(), "parse errors: {errors:#?}");
    p
}

pub fn run_dialect(src: &str, bytes: &[u8], dialect: Dialect) -> TemplateResult {
    let p = program(src, dialect);
    evaluate(&p, &tree(bytes), &EvalOptions::default(), &Cancel::new()).expect("evaluate")
}

pub fn run(src: &str, bytes: &[u8]) -> TemplateResult {
    run_dialect(src, bytes, Dialect::Hexpat)
}

pub fn run_ok(src: &str, bytes: &[u8]) -> TemplateResult {
    let r = run(src, bytes);
    assert!(r.is_ok(), "evaluation errors: {:#?}", r.errors);
    r
}

pub fn get<'a>(r: &'a TemplateResult, path: &str) -> &'a Node {
    r.find(path)
        .unwrap_or_else(|| panic!("no node `{path}`; have {:?}", all_paths(r)))
}

pub fn int(r: &TemplateResult, path: &str) -> i128 {
    get(r, path)
        .raw
        .as_ref()
        .and_then(hexa_template::Value::as_int)
        .unwrap_or_else(|| panic!("`{path}` has no integer value"))
}

pub fn text(r: &TemplateResult, path: &str) -> String {
    match &get(r, path).raw {
        Some(hexa_template::Value::Str(s)) => s.clone(),
        other => panic!("`{path}` is not a string: {other:?}"),
    }
}

pub fn all_paths(r: &TemplateResult) -> Vec<String> {
    r.nodes.iter().map(|n| r.path(n.id)).collect()
}
