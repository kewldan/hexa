//! Tokenizer and a tiny C-like preprocessor (STRC-01/02, STRC-05/06).
//!
//! Handles `//` and `/* */` comments, numeric literals (hex, binary, octal,
//! decimal, floats, `'` and `_` digit separators, C suffixes), strings and
//! character literals with escapes, and `#` directives. The preprocessor
//! implements `#include`/`import` (through a resolver), object-like
//! `#define`, `#undef`, `#ifdef`/`#ifndef`/`#if`/`#elif`/`#else`/`#endif`
//! and passes `#pragma` lines through to the parser.

use std::collections::HashMap;

use crate::ast::Span;
use crate::diag::Diagnostic;

/// Token kinds.
#[derive(Debug, Clone, PartialEq)]
pub enum Tok {
    Ident(String),
    Int(i128),
    Float(f64),
    Str(String),
    /// Character literal (value of the character).
    Char(i128),
    Punct(&'static str),
    /// `#name rest-of-line` directive.
    Directive(String, String),
    Eof,
}

/// Token with its source span.
#[derive(Debug, Clone, PartialEq)]
pub struct Token {
    pub tok: Tok,
    pub span: Span,
}

const PUNCTS: &[&str] = &[
    "<<=", ">>=", "...", "::", "->", "+=", "-=", "*=", "/=", "%=", "&=", "|=", "^=", "==", "!=",
    "<=", ">=", "&&", "||", "^^", "<<", ">>", "++", "--", "+", "-", "*", "/", "%", "&", "|", "^",
    "~", "!", "=", "<", ">", "?", ":", ";", ",", ".", "(", ")", "[", "]", "{", "}", "@", "$",
];

struct Lexer<'a> {
    src: &'a [u8],
    text: &'a str,
    pos: usize,
    line: u32,
    col: u32,
    line_start: bool,
}

impl Lexer<'_> {
    fn peek(&self, n: usize) -> u8 {
        self.src.get(self.pos + n).copied().unwrap_or(0)
    }

    fn bump(&mut self) -> u8 {
        let c = self.peek(0);
        self.pos += 1;
        if c == b'\n' {
            self.line += 1;
            self.col = 1;
            self.line_start = true;
        } else if c & 0xC0 != 0x80 {
            // Count characters, not UTF-8 continuation bytes.
            self.col += 1;
        }
        c
    }

    fn skip_trivia(&mut self) -> Result<(), Diagnostic> {
        loop {
            let c = self.peek(0);
            if c == b'\n' || c == b' ' || c == b'\t' || c == b'\r' || c == 0x0C {
                self.bump();
            } else if c == b'/' && self.peek(1) == b'/' {
                while self.pos < self.src.len() && self.peek(0) != b'\n' {
                    self.bump();
                }
            } else if c == b'/' && self.peek(1) == b'*' {
                let (l, cl) = (self.line, self.col);
                self.bump();
                self.bump();
                loop {
                    if self.pos >= self.src.len() {
                        return Err(Diagnostic::error(
                            Span::new(l, cl, self.line, self.col),
                            "unterminated block comment",
                        ));
                    }
                    if self.peek(0) == b'*' && self.peek(1) == b'/' {
                        self.bump();
                        self.bump();
                        break;
                    }
                    self.bump();
                }
            } else if c == b'\\' && (self.peek(1) == b'\n' || self.peek(1) == b'\r') {
                // Line continuation.
                self.bump();
                if self.peek(0) == b'\r' {
                    self.bump();
                }
                if self.peek(0) == b'\n' {
                    self.bump();
                }
                self.line_start = false;
            } else {
                return Ok(());
            }
        }
    }

    fn escape(&mut self, start: Span) -> Result<char, Diagnostic> {
        let c = self.bump();
        Ok(match c {
            b'n' => '\n',
            b't' => '\t',
            b'r' => '\r',
            b'0' => '\0',
            b'a' => '\x07',
            b'b' => '\x08',
            b'f' => '\x0C',
            b'v' => '\x0B',
            b'e' => '\x1B',
            b'\\' => '\\',
            b'\'' => '\'',
            b'"' => '"',
            b'x' => {
                let mut v = 0u32;
                let mut n = 0;
                while n < 2 && self.peek(0).is_ascii_hexdigit() {
                    v = v * 16 + (self.bump() as char).to_digit(16).unwrap_or(0);
                    n += 1;
                }
                char::from_u32(v).unwrap_or('\0')
            }
            b'u' => {
                let mut v = 0u32;
                if self.peek(0) == b'{' {
                    self.bump();
                    while self.peek(0).is_ascii_hexdigit() {
                        v = v * 16 + (self.bump() as char).to_digit(16).unwrap_or(0);
                    }
                    if self.peek(0) == b'}' {
                        self.bump();
                    }
                } else {
                    for _ in 0..4 {
                        if self.peek(0).is_ascii_hexdigit() {
                            v = v * 16 + (self.bump() as char).to_digit(16).unwrap_or(0);
                        }
                    }
                }
                char::from_u32(v).unwrap_or('\u{FFFD}')
            }
            0 => return Err(Diagnostic::error(start, "unterminated escape")),
            other => other as char,
        })
    }

    fn number(&mut self) -> Result<Tok, String> {
        let start = self.pos;
        let radix = if self.peek(0) == b'0' {
            match self.peek(1) {
                b'x' | b'X' => 16,
                b'b' | b'B' if self.peek(2) == b'0' || self.peek(2) == b'1' => 2,
                b'o' | b'O' => 8,
                _ => 10,
            }
        } else {
            10
        };
        if radix != 10 {
            self.bump();
            self.bump();
            let mut digits = String::new();
            while self.peek(0).is_ascii_alphanumeric() || matches!(self.peek(0), b'_' | b'\'') {
                let c = self.bump();
                if c != b'_' && c != b'\'' {
                    digits.push(c as char);
                }
            }
            let digits = digits.trim_end_matches(['u', 'U', 'l', 'L']);
            let v = u128::from_str_radix(digits, radix)
                .map_err(|_| format!("invalid number literal `{}`", &self.text[start..self.pos]))?;
            return Ok(Tok::Int(v as i128));
        }
        let mut s = String::new();
        let mut is_float = false;
        loop {
            let c = self.peek(0);
            if c.is_ascii_digit() || c == b'_' || c == b'\'' {
                self.bump();
                if c.is_ascii_digit() {
                    s.push(c as char);
                }
            } else if c == b'.' && self.peek(1).is_ascii_digit() && !is_float {
                is_float = true;
                s.push('.');
                self.bump();
            } else if (c == b'e' || c == b'E')
                && (self.peek(1).is_ascii_digit()
                    || ((self.peek(1) == b'-' || self.peek(1) == b'+')
                        && self.peek(2).is_ascii_digit()))
            {
                is_float = true;
                s.push('e');
                self.bump();
                if matches!(self.peek(0), b'-' | b'+') {
                    s.push(self.bump() as char);
                }
            } else {
                break;
            }
        }
        // Suffixes: u, l, ul, ull, f, d.
        let mut float_suffix = false;
        while matches!(
            self.peek(0),
            b'u' | b'U' | b'l' | b'L' | b'f' | b'F' | b'd' | b'D'
        ) {
            if matches!(self.peek(0), b'f' | b'F' | b'd' | b'D') {
                float_suffix = true;
            }
            self.bump();
        }
        if self.peek(0).is_ascii_alphanumeric() {
            return Err(format!(
                "invalid number literal `{}`",
                &self.text[start..=self.pos]
            ));
        }
        if is_float || float_suffix {
            s.parse::<f64>()
                .map(Tok::Float)
                .map_err(|_| format!("invalid float literal `{s}`"))
        } else if s.len() > 1 && s.starts_with('0') && s.bytes().all(|b| (b'0'..=b'7').contains(&b))
        {
            // C-style octal.
            u128::from_str_radix(&s[1..], 8)
                .map(|v| Tok::Int(v as i128))
                .map_err(|_| format!("invalid octal literal `{s}`"))
        } else {
            s.parse::<u128>()
                .map(|v| Tok::Int(v as i128))
                .map_err(|_| format!("number literal `{s}` is too large"))
        }
    }

    fn next(&mut self) -> Result<Token, Diagnostic> {
        self.skip_trivia()?;
        let (line, col) = (self.line, self.col);
        let at_line_start = self.line_start;
        self.line_start = false;
        let mk = |s: &Self, tok| Token {
            tok,
            span: Span::new(line, col, s.line, s.col),
        };
        let c = self.peek(0);
        if self.pos >= self.src.len() {
            return Ok(mk(self, Tok::Eof));
        }
        if c == b'#' && at_line_start {
            self.bump();
            while matches!(self.peek(0), b' ' | b'\t') {
                self.bump();
            }
            let ns = self.pos;
            while self.peek(0).is_ascii_alphanumeric() || self.peek(0) == b'_' {
                self.bump();
            }
            let name = self.text[ns..self.pos].to_string();
            let mut rest = String::new();
            // Rest of line, honouring continuations and stripping comments.
            while self.pos < self.src.len() && self.peek(0) != b'\n' {
                if self.peek(0) == b'\\' && matches!(self.peek(1), b'\n' | b'\r') {
                    self.bump();
                    while matches!(self.peek(0), b'\r' | b'\n') {
                        let c = self.bump();
                        if c == b'\n' {
                            break;
                        }
                    }
                    rest.push(' ');
                    continue;
                }
                if self.peek(0) == b'/' && self.peek(1) == b'/' {
                    while self.pos < self.src.len() && self.peek(0) != b'\n' {
                        self.bump();
                    }
                    break;
                }
                if self.peek(0) == b'/' && self.peek(1) == b'*' {
                    self.skip_trivia()?;
                    rest.push(' ');
                    continue;
                }
                let start = self.pos;
                self.bump();
                while self.pos < self.src.len() && self.src[self.pos] & 0xC0 == 0x80 {
                    self.pos += 1;
                }
                rest.push_str(&self.text[start..self.pos]);
            }
            return Ok(mk(self, Tok::Directive(name, rest.trim().to_string())));
        }
        if c.is_ascii_alphabetic() || c == b'_' || c >= 0x80 {
            let s = self.pos;
            while self.peek(0).is_ascii_alphanumeric()
                || self.peek(0) == b'_'
                || self.peek(0) >= 0x80
            {
                self.bump();
            }
            return Ok(mk(self, Tok::Ident(self.text[s..self.pos].to_string())));
        }
        if c.is_ascii_digit() || (c == b'.' && self.peek(1).is_ascii_digit()) {
            if c == b'.' {
                // `.5`
                self.bump();
                let s = self.pos;
                while self.peek(0).is_ascii_digit() {
                    self.bump();
                }
                let v: f64 = format!("0.{}", &self.text[s..self.pos])
                    .parse()
                    .unwrap_or(0.0);
                return Ok(mk(self, Tok::Float(v)));
            }
            return match self.number() {
                Ok(t) => Ok(mk(self, t)),
                Err(m) => Err(Diagnostic::error(
                    Span::new(line, col, self.line, self.col),
                    m,
                )),
            };
        }
        if c == b'"' {
            self.bump();
            let mut s = String::new();
            loop {
                let here = Span::new(line, col, self.line, self.col);
                match self.peek(0) {
                    0 if self.pos >= self.src.len() => {
                        return Err(Diagnostic::error(here, "unterminated string literal"));
                    }
                    b'\n' => return Err(Diagnostic::error(here, "unterminated string literal")),
                    b'"' => {
                        self.bump();
                        break;
                    }
                    b'\\' => {
                        self.bump();
                        s.push(self.escape(here)?);
                    }
                    _ => {
                        let st = self.pos;
                        self.bump();
                        while self.pos < self.src.len() && self.src[self.pos] & 0xC0 == 0x80 {
                            self.pos += 1;
                        }
                        s.push_str(&self.text[st..self.pos]);
                    }
                }
            }
            return Ok(mk(self, Tok::Str(s)));
        }
        if c == b'\'' {
            self.bump();
            let here = Span::new(line, col, self.line, self.col);
            let mut value: i128 = 0;
            let mut n = 0;
            while self.peek(0) != b'\'' {
                if self.pos >= self.src.len() || self.peek(0) == b'\n' {
                    return Err(Diagnostic::error(here, "unterminated character literal"));
                }
                let ch = if self.peek(0) == b'\\' {
                    self.bump();
                    self.escape(here)?
                } else {
                    let st = self.pos;
                    self.bump();
                    while self.pos < self.src.len() && self.src[self.pos] & 0xC0 == 0x80 {
                        self.pos += 1;
                    }
                    self.text[st..self.pos].chars().next().unwrap_or('\0')
                };
                // Multi-character constants ('PK') pack big-endian like C.
                value = (value << 8) | i128::from(u32::from(ch));
                n += 1;
            }
            self.bump();
            if n == 0 {
                return Err(Diagnostic::error(here, "empty character literal"));
            }
            return Ok(mk(self, Tok::Char(value)));
        }
        let rest = &self.src[self.pos..];
        for p in PUNCTS {
            if rest.starts_with(p.as_bytes()) {
                for _ in 0..p.len() {
                    self.bump();
                }
                return Ok(mk(self, Tok::Punct(p)));
            }
        }
        self.bump();
        Err(Diagnostic::error(
            Span::new(line, col, self.line, self.col),
            format!("unexpected character `{}`", c as char),
        ))
    }
}

/// Tokenizes `src`. Lexical errors are reported and the offending character
/// skipped so that later stages still see the rest of the file.
pub fn tokenize(src: &str) -> (Vec<Token>, Vec<Diagnostic>) {
    let mut lx = Lexer {
        src: src.as_bytes(),
        text: src,
        pos: 0,
        line: 1,
        col: 1,
        line_start: true,
    };
    let mut out = Vec::new();
    let mut diags = Vec::new();
    loop {
        match lx.next() {
            Ok(t) => {
                let eof = t.tok == Tok::Eof;
                out.push(t);
                if eof {
                    break;
                }
            }
            Err(d) => {
                diags.push(d);
                if lx.pos >= lx.src.len() {
                    out.push(Token {
                        tok: Tok::Eof,
                        span: Span::new(lx.line, lx.col, lx.line, lx.col),
                    });
                    break;
                }
            }
        }
    }
    (out, diags)
}

/// Resolves `#include "x"` / `import a.b;` to source text.
pub type Resolver<'a> = &'a dyn Fn(&str) -> Option<String>;

/// Paths that name `ImHex` standard-library headers; their contents are built
/// into the evaluator so a missing resolver entry is not an error.
pub(crate) fn is_builtin_include(path: &str) -> bool {
    let p = path
        .trim_start_matches(['<', '"'])
        .trim_end_matches(['>', '"']);
    [
        "std/", "std.", "type/", "type.", "hex/", "hex.", "std", "type",
    ]
    .iter()
    .any(|pre| p.starts_with(pre))
}

struct Pre<'r> {
    defines: HashMap<String, Vec<Token>>,
    resolver: Option<Resolver<'r>>,
    out: Vec<Token>,
    diags: Vec<Diagnostic>,
    depth: u32,
    included: Vec<String>,
}

impl Pre<'_> {
    fn include(&mut self, path: &str, span: Span) {
        let name = path
            .trim()
            .trim_start_matches(['<', '"'])
            .trim_end_matches(['>', '"'])
            .to_string();
        if self.included.contains(&name) {
            return; // implicit include guard / #pragma once
        }
        if self.depth > 32 {
            self.diags
                .push(Diagnostic::error(span, "includes nested too deeply"));
            return;
        }
        let src = self.resolver.and_then(|r| r(&name));
        match src {
            Some(src) => {
                self.included.push(name);
                let (toks, diags) = tokenize(&src);
                self.diags.extend(diags);
                self.depth += 1;
                self.run(toks, false);
                self.depth -= 1;
            }
            None if is_builtin_include(&name) => {}
            None => self.diags.push(Diagnostic::error(
                span,
                format!("cannot resolve include `{name}`"),
            )),
        }
    }

    fn eval_if(&self, cond: &str) -> bool {
        let cond = cond.trim();
        if let Some(rest) = cond.strip_prefix('!') {
            return !self.eval_if(rest);
        }
        if let Some(inner) = cond.strip_prefix("defined").map(|r| {
            r.trim()
                .trim_start_matches('(')
                .trim_end_matches(')')
                .trim()
        }) {
            return self.defines.contains_key(inner);
        }
        if let Ok(v) = cond.parse::<i64>() {
            return v != 0;
        }
        match self.defines.get(cond) {
            Some(toks) => !matches!(toks.first().map(|t| &t.tok), Some(Tok::Int(0))),
            // Unknown conditions are treated as true so that code is checked.
            None => cond != "0",
        }
    }

    fn run(&mut self, toks: Vec<Token>, top: bool) {
        // Stack of (currently active, some branch already taken, parent active).
        let mut cond: Vec<(bool, bool)> = Vec::new();
        let active = |c: &Vec<(bool, bool)>| c.iter().all(|x| x.0);
        let mut it = toks.into_iter().peekable();
        while let Some(t) = it.next() {
            match &t.tok {
                Tok::Eof => {
                    if top {
                        self.out.push(t);
                    }
                    break;
                }
                Tok::Directive(name, rest) => match name.as_str() {
                    "ifdef" | "ifndef" => {
                        let d = self.defines.contains_key(rest.trim());
                        let v = if name == "ifdef" { d } else { !d };
                        cond.push((v, v));
                    }
                    "if" => {
                        let v = self.eval_if(rest);
                        cond.push((v, v));
                    }
                    "elif" => {
                        let v = self.eval_if(rest);
                        if let Some(top) = cond.last_mut() {
                            let new = !top.1 && v;
                            *top = (new, top.1 || new);
                        }
                    }
                    "else" => {
                        if let Some(top) = cond.last_mut() {
                            *top = (!top.1, true);
                        }
                    }
                    "endif" => {
                        cond.pop();
                    }
                    _ if !active(&cond) => {}
                    "define" => {
                        let rest = rest.trim();
                        let end = rest
                            .find(|c: char| !(c.is_alphanumeric() || c == '_'))
                            .unwrap_or(rest.len());
                        let (name, body) = rest.split_at(end);
                        if body.starts_with('(') {
                            // Function-like macros are not supported; ignore.
                            continue;
                        }
                        let (mut toks, _) = tokenize(body);
                        toks.retain(|t| t.tok != Tok::Eof);
                        for t in &mut toks {
                            t.span = Span::default();
                        }
                        self.defines.insert(name.to_string(), toks);
                    }
                    "undef" => {
                        self.defines.remove(rest.trim());
                    }
                    "include" => self.include(rest, t.span),
                    "pragma" if rest.trim() == "once" => {}
                    "error" => self
                        .diags
                        .push(Diagnostic::error(t.span, format!("#error {rest}"))),
                    _ => self.out.push(t),
                },
                _ if !active(&cond) => {}
                Tok::Ident(id) if id == "import" => {
                    // `import std.mem;` / `import a.b as c;`
                    let mut path = String::new();
                    let span = t.span;
                    for n in it.by_ref() {
                        match n.tok {
                            Tok::Punct(";") | Tok::Eof => break,
                            Tok::Ident(s) => {
                                if s == "as" {
                                    continue;
                                }
                                path.push_str(&s);
                            }
                            Tok::Punct(".") => path.push('/'),
                            Tok::Punct("*") => path.push('*'),
                            _ => {}
                        }
                    }
                    if !is_builtin_include(&path) {
                        let with_ext = format!("{path}.hexpat");
                        if self.resolver.and_then(|r| r(&with_ext)).is_some() {
                            self.include(&with_ext, span);
                        } else {
                            self.include(&path, span);
                        }
                    }
                }
                Tok::Ident(id) if self.defines.contains_key(id) => {
                    let mut seen = vec![id.clone()];
                    self.expand(id, t.span, &mut seen);
                }
                _ => self.out.push(t),
            }
        }
    }

    fn expand(&mut self, name: &str, span: Span, seen: &mut Vec<String>) {
        let body = self.defines.get(name).cloned().unwrap_or_default();
        for mut t in body {
            t.span = span;
            if let Tok::Ident(id) = &t.tok
                && self.defines.contains_key(id)
                && !seen.contains(id)
            {
                seen.push(id.clone());
                let id = id.clone();
                self.expand(&id, span, seen);
                seen.pop();
                continue;
            }
            self.out.push(t);
        }
    }
}

/// Tokenizes and preprocesses `src`.
pub fn preprocess(src: &str, resolver: Option<Resolver<'_>>) -> (Vec<Token>, Vec<Diagnostic>) {
    let (toks, diags) = tokenize(src);
    let mut pre = Pre {
        defines: HashMap::new(),
        resolver,
        out: Vec::new(),
        diags,
        depth: 0,
        included: Vec::new(),
    };
    pre.run(toks, true);
    if pre.out.last().map(|t| &t.tok) != Some(&Tok::Eof) {
        pre.out.push(Token {
            tok: Tok::Eof,
            span: Span::default(),
        });
    }
    (pre.out, pre.diags)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn toks(s: &str) -> Vec<Tok> {
        let (t, d) = tokenize(s);
        assert!(d.is_empty(), "{d:?}");
        t.into_iter().map(|t| t.tok).collect()
    }

    #[test]
    fn numbers() {
        assert_eq!(
            toks("0x1F 0b101 0o17 017 1'000 1_0 5u 7ULL 1.5 2e3 3f"),
            vec![
                Tok::Int(31),
                Tok::Int(5),
                Tok::Int(15),
                Tok::Int(15),
                Tok::Int(1000),
                Tok::Int(10),
                Tok::Int(5),
                Tok::Int(7),
                Tok::Float(1.5),
                Tok::Float(2000.0),
                Tok::Float(3.0),
                Tok::Eof
            ]
        );
    }

    #[test]
    fn strings_chars_and_puncts() {
        assert_eq!(
            toks(r#""a\n\x41" 'A' 'PK' a::b ... >>= [["#),
            vec![
                Tok::Str("a\nA".into()),
                Tok::Char(65),
                Tok::Char(0x504B),
                Tok::Ident("a".into()),
                Tok::Punct("::"),
                Tok::Ident("b".into()),
                Tok::Punct("..."),
                Tok::Punct(">>="),
                Tok::Punct("["),
                Tok::Punct("["),
                Tok::Eof
            ]
        );
    }

    #[test]
    fn comments_and_spans() {
        let (t, _) = tokenize("// c\n/* x\n y */ abc");
        assert_eq!(t[0].tok, Tok::Ident("abc".into()));
        assert_eq!((t[0].span.line, t[0].span.col), (3, 7));
        let (_, d) = tokenize("/* open");
        assert_eq!(d.len(), 1);
        let (_, d) = tokenize("\"open");
        assert_eq!(d.len(), 1);
    }

    #[test]
    fn preprocessor() {
        let src = "#define N 4\n#ifndef X\nu8 a[N];\n#else\nbad\n#endif\n#pragma endian big\n";
        let (t, d) = preprocess(src, None);
        assert!(d.is_empty(), "{d:?}");
        let k: Vec<_> = t.into_iter().map(|t| t.tok).collect();
        assert_eq!(k[3], Tok::Int(4));
        assert!(matches!(&k[6], Tok::Directive(n, r) if n == "pragma" && r == "endian big"));
        assert!(!k.contains(&Tok::Ident("bad".into())));
    }

    #[test]
    fn includes_through_resolver() {
        let resolver = |p: &str| (p == "inc.h").then(|| "u8 x;".to_string());
        let (t, d) = preprocess(
            "#include \"inc.h\"\n#include <std/mem.pat>\nimport std.io;",
            Some(&resolver),
        );
        assert!(d.is_empty(), "{d:?}");
        assert_eq!(t.len(), 4);
        let (_, d) = preprocess("#include \"missing.h\"", Some(&resolver));
        assert_eq!(d.len(), 1);
    }
}
