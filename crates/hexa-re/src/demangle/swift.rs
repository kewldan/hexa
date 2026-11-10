//! Basic Swift 5 symbol demangler (RE-08).
//!
//! No pure-Rust Swift demangler exists (the `swift-demangler` crate builds
//! Apple's C++ sources), so this module implements the subset of the
//! Swift 5 mangling grammar that covers most symbols seen in binaries:
//!
//! * prefixes `$s`, `_$s`, `$S`, `_$S`, `$e`;
//! * identifiers including word substitutions (`0` ... `A`-`Z`);
//! * substitutions (`A` + index), standard types (`Si`, `SS`, `Sb`,
//!   repeated `S2i`, `So`/`SC` modules, `Sg` optionals), `s` (Swift module);
//! * nominal types (`V`, `C`, `O`, `P`, `a`), bound generics (`G`), tuples
//!   (`t`, `_`, `y`), function types (`c`, `K` throws, `Ya` async), `z`
//!   inout, generic parameter `x`, builtin types (`Bi<n>_`, `Bp`, `Bw`,
//!   `Bo`, `Bb`, `BO`, `Bf<n>_`), extensions (`E`);
//! * entities: functions (`F`) with argument labels, `Z` static,
//!   variables (`v`) with accessors (`g`, `s`, `M`, `m`, `r`, `w`, `W`,
//!   `p`), initialisers (`fC`, `fc`), deinitialisers (`fD`, `fd`);
//! * wrappers: `N`, `Ma`, `Mn`, `Mf`, `Mm`, `MF`, `Mp`, `MV`, `MXM`, `WV`,
//!   `Tq`; trailing `.suffix` text.
//!
//! Generic signatures, protocol conformances / witnesses, closures,
//! operators, punycode identifiers and old (`_T`) manglings return `None`.

#[derive(Debug, Clone)]
enum Ty {
    Nominal {
        ctx: Ctx,
        name: String,
        kind: u8,
    },
    Generic {
        base: Box<Ty>,
        args: Vec<Ty>,
    },
    Tuple(Vec<(Option<String>, Ty)>),
    Func {
        params: Box<Ty>,
        result: Box<Ty>,
        throws: bool,
        is_async: bool,
    },
    Inout(Box<Ty>),
    Builtin(String),
    Param(String),
}

#[derive(Debug, Clone)]
enum Ctx {
    Module(String),
    Type(Box<Ty>),
    Extension { module: String, ty: Box<Ty> },
}

#[derive(Debug, Clone)]
enum Node {
    Ident(String),
    Module(String),
    Type(Ty),
    Extension(Ctx),
    EmptyList,
    First,
    Throws,
    Async,
    Entity { text: String, is_static: bool },
}

struct Demangler<'a> {
    s: &'a [u8],
    pos: usize,
    stack: Vec<Node>,
    subst: Vec<Node>,
    words: Vec<String>,
}

fn swift(name: &str) -> Ty {
    Ty::Nominal {
        ctx: Ctx::Module("Swift".into()),
        name: name.into(),
        kind: b'V',
    }
}

fn standard_type(c: u8) -> Option<&'static str> {
    Some(match c {
        b'A' => "AutoreleasingUnsafeMutablePointer",
        b'a' => "Array",
        b'b' => "Bool",
        b'D' => "Dictionary",
        b'd' => "Double",
        b'f' => "Float",
        b'h' => "Set",
        b'I' => "DefaultIndices",
        b'i' => "Int",
        b'J' => "Character",
        b'N' => "ClosedRange",
        b'n' => "Range",
        b'O' => "ObjectIdentifier",
        b'P' => "UnsafePointer",
        b'p' => "UnsafeMutablePointer",
        b'R' => "UnsafeBufferPointer",
        b'r' => "UnsafeMutableBufferPointer",
        b'S' => "String",
        b's' => "Substring",
        b'u' => "UInt",
        b'V' => "UnsafeRawPointer",
        b'v' => "UnsafeMutableRawPointer",
        b'W' => "UnsafeRawBufferPointer",
        b'w' => "UnsafeMutableRawBufferPointer",
        b'q' => "Optional",
        b'B' => "BinaryFloatingPoint",
        b'E' => "Encodable",
        b'e' => "Decodable",
        b'F' => "FloatingPoint",
        b'G' => "RandomNumberGenerator",
        b'H' => "Hashable",
        b'j' => "Numeric",
        b'K' => "BidirectionalCollection",
        b'k' => "RandomAccessCollection",
        b'L' => "Comparable",
        b'l' => "Collection",
        b'M' => "MutableCollection",
        b'm' => "RangeReplaceableCollection",
        b'Q' => "Equatable",
        b'T' => "Sequence",
        b't' => "IteratorProtocol",
        b'U' => "UnsignedInteger",
        b'X' => "RangeExpression",
        b'x' => "Strideable",
        b'Y' => "RawRepresentable",
        b'y' => "StringProtocol",
        b'Z' => "SignedInteger",
        b'z' => "BinaryInteger",
        _ => return None,
    })
}

fn ctx_str(c: &Ctx) -> String {
    match c {
        Ctx::Module(m) => m.clone(),
        Ctx::Type(t) => ty_str(t),
        Ctx::Extension { module, ty } => format!("(extension in {module}):{}", ty_str(ty)),
    }
}

fn is_swift(t: &Ty, name: &str) -> bool {
    matches!(t, Ty::Nominal { ctx: Ctx::Module(m), name: n, .. } if m == "Swift" && n == name)
}

fn elements(t: &Ty) -> Vec<(Option<String>, Ty)> {
    match t {
        Ty::Tuple(e) => e.clone(),
        other => vec![(None, other.clone())],
    }
}

fn tuple_str(elems: &[(Option<String>, Ty)]) -> String {
    let parts: Vec<String> = elems
        .iter()
        .map(|(l, t)| match l {
            Some(l) => format!("{l}: {}", ty_str(t)),
            None => ty_str(t),
        })
        .collect();
    format!("({})", parts.join(", "))
}

fn ty_str(t: &Ty) -> String {
    match t {
        Ty::Nominal { ctx, name, .. } => format!("{}.{name}", ctx_str(ctx)),
        Ty::Generic { base, args } => {
            if is_swift(base, "Array") && args.len() == 1 {
                format!("[{}]", ty_str(&args[0]))
            } else if is_swift(base, "Dictionary") && args.len() == 2 {
                format!("[{} : {}]", ty_str(&args[0]), ty_str(&args[1]))
            } else if is_swift(base, "Optional") && args.len() == 1 {
                let inner = ty_str(&args[0]);
                if matches!(args[0], Ty::Func { .. }) {
                    format!("({inner})?")
                } else {
                    format!("{inner}?")
                }
            } else {
                let a: Vec<String> = args.iter().map(ty_str).collect();
                format!("{}<{}>", ty_str(base), a.join(", "))
            }
        }
        Ty::Tuple(e) => tuple_str(e),
        Ty::Func {
            params,
            result,
            throws,
            is_async,
        } => format!(
            "{}{}{} -> {}",
            tuple_str(&elements(params)),
            if *is_async { " async" } else { "" },
            if *throws { " throws" } else { "" },
            ty_str(result)
        ),
        Ty::Inout(t) => format!("inout {}", ty_str(t)),
        Ty::Builtin(b) => format!("Builtin.{b}"),
        Ty::Param(p) => p.clone(),
    }
}

impl Demangler<'_> {
    fn peek(&self) -> Option<u8> {
        self.s.get(self.pos).copied()
    }

    fn next(&mut self) -> Option<u8> {
        let c = self.peek()?;
        self.pos += 1;
        Some(c)
    }

    fn next_if(&mut self, c: u8) -> bool {
        if self.peek() == Some(c) {
            self.pos += 1;
            true
        } else {
            false
        }
    }

    fn natural(&mut self) -> Option<usize> {
        let start = self.pos;
        while self.peek().is_some_and(|c| c.is_ascii_digit()) {
            self.pos += 1;
        }
        if start == self.pos {
            return None;
        }
        std::str::from_utf8(&self.s[start..self.pos])
            .ok()?
            .parse()
            .ok()
    }

    fn pop(&mut self) -> Option<Node> {
        self.stack.pop()
    }

    fn pop_type(&mut self) -> Option<Ty> {
        match self.stack.last()? {
            Node::Type(_) => match self.stack.pop()? {
                Node::Type(t) => Some(t),
                _ => None,
            },
            _ => None,
        }
    }

    fn pop_ident(&mut self) -> Option<String> {
        match self.stack.last()? {
            Node::Ident(_) => match self.stack.pop()? {
                Node::Ident(s) => Some(s),
                _ => None,
            },
            _ => None,
        }
    }

    fn pop_context(&mut self) -> Option<Ctx> {
        Some(match self.pop()? {
            Node::Ident(m) | Node::Module(m) => Ctx::Module(m),
            Node::Type(t) => Ctx::Type(Box::new(t)),
            Node::Extension(c) => c,
            _ => return None,
        })
    }

    /// Pops a parameter or result list: `y` is `()`, otherwise a type.
    fn pop_list_type(&mut self) -> Option<Ty> {
        if matches!(self.stack.last(), Some(Node::EmptyList)) {
            self.stack.pop();
            return Some(Ty::Tuple(Vec::new()));
        }
        self.pop_type()
    }

    fn identifier(&mut self) -> Option<()> {
        let mut has_words = false;
        if self.peek() == Some(b'0') {
            self.pos += 1;
            if self.peek() == Some(b'0') {
                return None; // punycode
            }
            has_words = true;
        }
        let mut ident = String::new();
        loop {
            while has_words && self.peek().is_some_and(|c| c.is_ascii_alphabetic()) {
                let c = self.next()?;
                let idx = if c.is_ascii_lowercase() {
                    usize::from(c - b'a')
                } else {
                    has_words = false;
                    usize::from(c - b'A')
                };
                ident.push_str(self.words.get(idx)?);
            }
            if self.next_if(b'0') {
                break;
            }
            let n = self.natural()?;
            if n == 0 || self.pos + n > self.s.len() {
                return None;
            }
            let slice = std::str::from_utf8(&self.s[self.pos..self.pos + n]).ok()?;
            ident.push_str(slice);
            self.collect_words(slice);
            self.pos += n;
            if !has_words {
                break;
            }
        }
        let node = Node::Ident(ident);
        self.subst.push(node.clone());
        self.stack.push(node);
        Some(())
    }

    fn collect_words(&mut self, slice: &str) {
        let b = slice.as_bytes();
        let mut start: Option<usize> = None;
        for i in 0..=b.len() {
            let c = b.get(i).copied().unwrap_or(0);
            if let Some(st) = start {
                let prev = b[i - 1];
                let end =
                    c == b'_' || c == 0 || (!prev.is_ascii_uppercase() && c.is_ascii_uppercase());
                if end {
                    if i - st >= 2 && self.words.len() < 26 {
                        self.words.push(slice[st..i].to_string());
                    }
                    start = None;
                }
            }
            if start.is_none() && c != 0 && c != b'_' && !c.is_ascii_digit() {
                start = Some(i);
            }
        }
    }

    fn push_type(&mut self, t: Ty, subst: bool) {
        let n = Node::Type(t);
        if subst {
            self.subst.push(n.clone());
        }
        self.stack.push(n);
    }

    fn standard_substitution(&mut self) -> Option<()> {
        match self.peek()? {
            b'o' => {
                self.pos += 1;
                self.stack.push(Node::Module("__C".into()));
            }
            b'C' => {
                self.pos += 1;
                self.stack.push(Node::Module("__C_Synthesized".into()));
            }
            b'g' => {
                self.pos += 1;
                let t = self.pop_type()?;
                self.push_type(
                    Ty::Generic {
                        base: Box::new(swift("Optional")),
                        args: vec![t],
                    },
                    true,
                );
            }
            _ => {
                let repeat = self.natural().unwrap_or(1);
                if repeat == 0 || repeat > 2048 {
                    return None;
                }
                let name = standard_type(self.next()?)?;
                for _ in 0..repeat {
                    self.push_type(swift(name), false);
                }
            }
        }
        Some(())
    }

    fn substitution(&mut self) -> Option<()> {
        let mut repeat: Option<usize> = None;
        loop {
            let c = self.peek()?;
            if c.is_ascii_digit() {
                repeat = Some(self.natural()?);
                continue;
            }
            self.pos += 1;
            let (idx, last) = if c.is_ascii_lowercase() {
                (usize::from(c - b'a'), false)
            } else if c.is_ascii_uppercase() {
                (usize::from(c - b'A'), true)
            } else {
                return None;
            };
            let node = self.subst.get(idx)?.clone();
            for _ in 0..repeat.unwrap_or(1).min(2048) {
                self.stack.push(node.clone());
            }
            repeat = None;
            if last {
                return Some(());
            }
        }
    }

    fn builtin(&mut self) -> Option<()> {
        let name = match self.next()? {
            b'o' => "NativeObject".to_string(),
            b'O' => "UnknownObject".to_string(),
            b'b' => "BridgeObject".to_string(),
            b'B' => "UnsafeValueBuffer".to_string(),
            b'p' => "RawPointer".to_string(),
            b'w' => "Word".to_string(),
            b'i' => {
                let n = self.natural()?;
                if !self.next_if(b'_') {
                    return None;
                }
                format!("Int{n}")
            }
            b'f' => {
                let n = self.natural()?;
                if !self.next_if(b'_') {
                    return None;
                }
                format!("FPIEEE{n}")
            }
            _ => return None,
        };
        self.push_type(Ty::Builtin(name), false);
        Some(())
    }

    fn tuple(&mut self) -> Option<()> {
        let mut elems = Vec::new();
        if matches!(self.stack.last(), Some(Node::EmptyList)) {
            self.stack.pop();
        } else {
            loop {
                let first = matches!(self.stack.last(), Some(Node::First));
                if first {
                    self.stack.pop();
                }
                let t = self.pop_type()?;
                elems.push((None, t));
                if first {
                    break;
                }
            }
            elems.reverse();
        }
        self.push_type(Ty::Tuple(elems), false);
        Some(())
    }

    fn bound_generic(&mut self) -> Option<()> {
        let mut args = Vec::new();
        loop {
            let mut list = Vec::new();
            while let Some(t) = self.pop_type() {
                list.push(t);
            }
            list.reverse();
            let mut all = list;
            all.extend(args);
            args = all;
            if matches!(self.stack.last(), Some(Node::EmptyList)) {
                self.stack.pop();
                break;
            }
            if !matches!(self.stack.last(), Some(Node::First)) {
                return None;
            }
            self.stack.pop();
        }
        let base = self.pop_type()?;
        self.push_type(
            Ty::Generic {
                base: Box::new(base),
                args,
            },
            true,
        );
        Some(())
    }

    fn function_type(&mut self) -> Option<Ty> {
        let throws = matches!(self.stack.last(), Some(Node::Throws));
        if throws {
            self.stack.pop();
        }
        let is_async = matches!(self.stack.last(), Some(Node::Async));
        if is_async {
            self.stack.pop();
        }
        let params = self.pop_list_type()?;
        let result = self.pop_list_type()?;
        Some(Ty::Func {
            params: Box::new(params),
            result: Box::new(result),
            throws,
            is_async,
        })
    }

    /// Pops argument labels for a function with `n` parameters; an empty
    /// vector means the symbol carries no labels.
    fn labels(&mut self, n: usize) -> Option<Vec<String>> {
        if matches!(self.stack.last(), Some(Node::EmptyList)) {
            self.stack.pop();
            return Some(Vec::new());
        }
        let mut labels = Vec::with_capacity(n);
        for _ in 0..n {
            match self.pop()? {
                Node::Ident(s) => labels.push(s),
                Node::First => labels.push("_".into()),
                _ => return None,
            }
        }
        labels.reverse();
        Some(labels)
    }

    fn signature(func: &Ty, labels: &[String]) -> String {
        let Ty::Func {
            params,
            result,
            throws,
            is_async,
        } = func
        else {
            return String::new();
        };
        let mut elems = elements(params);
        for (e, l) in elems.iter_mut().zip(labels) {
            e.0 = Some(l.clone());
        }
        format!(
            "{}{}{} -> {}",
            tuple_str(&elems),
            if *is_async { " async" } else { "" },
            if *throws { " throws" } else { "" },
            ty_str(result)
        )
    }

    fn param_count(func: &Ty) -> usize {
        match func {
            Ty::Func { params, .. } => match params.as_ref() {
                Ty::Tuple(e) => e.len(),
                _ => 1,
            },
            _ => 0,
        }
    }

    fn function_entity(&mut self) -> Option<()> {
        let func = self.function_type()?;
        let labels = self.labels(Self::param_count(&func))?;
        let name = self.pop_ident()?;
        let ctx = self.pop_context()?;
        let text = format!(
            "{}.{name}{}",
            ctx_str(&ctx),
            Self::signature(&func, &labels)
        );
        self.stack.push(Node::Entity {
            text,
            is_static: false,
        });
        Some(())
    }

    fn init_entity(&mut self, kind: u8) -> Option<()> {
        let text = match kind {
            b'C' | b'c' => {
                let Node::Type(func @ Ty::Func { .. }) = self.pop()? else {
                    return None;
                };
                let labels = self.labels(Self::param_count(&func))?;
                let ctx = self.pop_context()?;
                let class = matches!(&ctx, Ctx::Type(t) if matches!(t.as_ref(), Ty::Nominal { kind: b'C', .. }));
                format!(
                    "{}.{}{}",
                    ctx_str(&ctx),
                    if kind == b'C' && class {
                        "__allocating_init"
                    } else {
                        "init"
                    },
                    Self::signature(&func, &labels)
                )
            }
            b'D' | b'd' => {
                let ctx = self.pop_context()?;
                format!(
                    "{}.{}",
                    ctx_str(&ctx),
                    if kind == b'D' {
                        "__deallocating_deinit"
                    } else {
                        "deinit"
                    }
                )
            }
            _ => return None,
        };
        self.stack.push(Node::Entity {
            text,
            is_static: false,
        });
        Some(())
    }

    fn variable(&mut self) -> Option<()> {
        let ty = self.pop_type()?;
        let name = self.pop_ident()?;
        let ctx = self.pop_context()?;
        let accessor = match self.peek() {
            Some(b'g') => Some(".getter"),
            Some(b's') => Some(".setter"),
            Some(b'M') => Some(".modify"),
            Some(b'm') => Some(".materializeForSet"),
            Some(b'r') => Some(".read"),
            Some(b'w') => Some(".willset"),
            Some(b'W') => Some(".didset"),
            Some(b'p') => Some(""),
            _ => None,
        };
        let suffix = match accessor {
            Some(a) => {
                self.pos += 1;
                a
            }
            None => "",
        };
        let text = format!("{}.{name}{suffix} : {}", ctx_str(&ctx), ty_str(&ty));
        self.stack.push(Node::Entity {
            text,
            is_static: false,
        });
        Some(())
    }

    fn wrap(&mut self, prefix: &str) -> Option<()> {
        let inner = match self.pop()? {
            Node::Type(t) => ty_str(&t),
            Node::Entity { text, is_static } => {
                if is_static {
                    format!("static {text}")
                } else {
                    text
                }
            }
            Node::Ident(m) | Node::Module(m) => m,
            _ => return None,
        };
        self.stack.push(Node::Entity {
            text: format!("{prefix}{inner}"),
            is_static: false,
        });
        Some(())
    }

    fn operator(&mut self) -> Option<()> {
        let c = self.peek()?;
        if c.is_ascii_digit() {
            return self.identifier();
        }
        self.pos += 1;
        match c {
            b'S' => self.standard_substitution()?,
            b's' => self.stack.push(Node::Module("Swift".into())),
            b'A' => self.substitution()?,
            b'B' => self.builtin()?,
            b'V' | b'C' | b'O' | b'P' | b'a' => {
                let name = self.pop_ident()?;
                let ctx = self.pop_context()?;
                self.push_type(Ty::Nominal { ctx, name, kind: c }, true);
            }
            b'y' => self.stack.push(Node::EmptyList),
            b'_' => self.stack.push(Node::First),
            b't' => self.tuple()?,
            b'G' => self.bound_generic()?,
            b'z' => {
                let t = self.pop_type()?;
                self.push_type(Ty::Inout(Box::new(t)), false);
            }
            b'x' => self.push_type(Ty::Param("A".into()), false),
            b'K' => self.stack.push(Node::Throws),
            b'Y' => {
                if self.next()? != b'a' {
                    return None;
                }
                self.stack.push(Node::Async);
            }
            b'c' => {
                let f = self.function_type()?;
                self.push_type(f, false);
            }
            b'E' => {
                let (Node::Ident(module) | Node::Module(module)) = self.pop()? else {
                    return None;
                };
                let ty = self.pop_type()?;
                self.stack.push(Node::Extension(Ctx::Extension {
                    module,
                    ty: Box::new(ty),
                }));
            }
            b'F' => self.function_entity()?,
            b'f' => {
                let k = self.next()?;
                self.init_entity(k)?;
            }
            b'v' => self.variable()?,
            b'Z' => match self.stack.last_mut()? {
                Node::Entity { is_static, .. } => *is_static = true,
                _ => return None,
            },
            b'N' => self.wrap("type metadata for ")?,
            b'M' => {
                if self.peek() == Some(b'p') && matches!(self.stack.last(), Some(Node::Ident(_))) {
                    // Protocol descriptors name the protocol without `P`.
                    let name = self.pop_ident()?;
                    let ctx = self.pop_context()?;
                    self.push_type(
                        Ty::Nominal {
                            ctx,
                            name,
                            kind: b'P',
                        },
                        false,
                    );
                }
                let prefix = match self.next()? {
                    b'a' => "type metadata accessor for ",
                    b'n' => "nominal type descriptor for ",
                    b'f' => "full type metadata for ",
                    b'm' => "metaclass for ",
                    b'F' => "reflection metadata field descriptor ",
                    b'p' => "protocol descriptor for ",
                    b'V' => "property descriptor for ",
                    b'X' if self.next_if(b'M') => "module descriptor ",
                    _ => return None,
                };
                self.wrap(prefix)?;
            }
            b'W' => {
                if self.next()? != b'V' {
                    return None;
                }
                self.wrap("value witness table for ")?;
            }
            b'T' => match self.next()? {
                b'q' => self.wrap("method descriptor for ")?,
                b'L' => {
                    if matches!(self.stack.last(), Some(Node::Ident(_))) {
                        let name = self.pop_ident()?;
                        let ctx = self.pop_context()?;
                        self.push_type(
                            Ty::Nominal {
                                ctx,
                                name,
                                kind: b'P',
                            },
                            false,
                        );
                    }
                    self.wrap("protocol requirements base descriptor for ")?;
                }
                _ => return None,
            },
            b'w' => {
                let a = self.next()?;
                let b = self.next()?;
                let name = match (a, b) {
                    (b'e', b't') => "getEnumTagSinglePayload",
                    (b's', b't') => "storeEnumTagSinglePayload",
                    (b'u', b'g') => "getEnumTag",
                    (b'u', b'i') => "destructiveInjectEnumTag",
                    (b'u', b'p') => "destructiveProjectEnumData",
                    (b'x', b'x') => "destroy",
                    (b'c', b'p') => "initializeWithCopy",
                    (b'c', b'a') => "assignWithCopy",
                    (b't', b'k') => "initializeWithTake",
                    (b't', b'a') => "assignWithTake",
                    (b'C', b'P') => "initializeBufferWithCopyOfBuffer",
                    _ => return None,
                };
                self.wrap(&format!("{name} value witness for "))?;
            }
            _ => return None,
        }
        Some(())
    }
}

/// Demangles a Swift symbol; `None` when it is not Swift or uses
/// unsupported grammar.
pub fn demangle_swift(symbol: &str) -> Option<String> {
    let s = symbol.strip_prefix('_').unwrap_or(symbol);
    let body = s
        .strip_prefix("$s")
        .or_else(|| s.strip_prefix("$S"))
        .or_else(|| s.strip_prefix("$e"))?;
    let (mangled, suffix) = match body.find('.') {
        Some(i) => (&body[..i], Some(&body[i..])),
        None => (body, None),
    };
    let mut d = Demangler {
        s: mangled.as_bytes(),
        pos: 0,
        stack: Vec::new(),
        subst: Vec::new(),
        words: Vec::new(),
    };
    while d.pos < d.s.len() {
        d.operator()?;
        if d.stack.len() > 4096 {
            return None;
        }
    }
    if d.stack.len() != 1 {
        return None;
    }
    let text = match d.stack.pop()? {
        Node::Entity { text, is_static } => {
            if is_static {
                format!("static {text}")
            } else {
                text
            }
        }
        Node::Type(t) => ty_str(&t),
        _ => return None,
    };
    Some(match suffix {
        Some(sfx) => format!("{text} with unmangled suffix \"{sfx}\""),
        None => text,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Expected strings come from Apple's `swift-demangle` on symbols of a
    /// `swiftc`-compiled module.
    #[test]
    fn real_symbols() {
        let cases = [
            ("_$s4main4noopyyF", "main.noop() -> ()"),
            (
                "_$s4main3add1a1bS2i_SitF",
                "main.add(a: Swift.Int, b: Swift.Int) -> Swift.Int",
            ),
            (
                "$s4main5greetySSSgSSF",
                "main.greet(Swift.String) -> Swift.String?",
            ),
            (
                "_$s4main4list_1mSbSaySiG_SDySSSiGtF",
                "main.list(_: [Swift.Int], m: [Swift.String : Swift.Int]) -> Swift.Bool",
            ),
            (
                "_$s4main3tupySb_SftSi_SSt_tF",
                "main.tup((Swift.Int, Swift.String)) -> (Swift.Bool, Swift.Float)",
            ),
            (
                "_$s4main5PointV1x1yACSi_SdtcfC",
                "main.Point.init(x: Swift.Int, y: Swift.Double) -> main.Point",
            ),
            (
                "_$s4main5PointV4move_2byySi_SitF",
                "main.Point.move(_: Swift.Int, by: Swift.Int) -> ()",
            ),
            (
                "_$s4main5PointV8distance2toSdAC_tF",
                "main.Point.distance(to: main.Point) -> Swift.Double",
            ),
            ("_$s4main5PointV1xSivg", "main.Point.x.getter : Swift.Int"),
            (
                "_$s4main5PointV1ySdvs",
                "main.Point.y.setter : Swift.Double",
            ),
            ("_$s4main5PointV1xSivM", "main.Point.x.modify : Swift.Int"),
            (
                "_$s4main5PointV1xSivpMV",
                "property descriptor for main.Point.x : Swift.Int",
            ),
            (
                "_$s4main5ShapeC4areaSdyF",
                "main.Shape.area() -> Swift.Double",
            ),
            (
                "_$s4main5ShapeC4areaSdyFTq",
                "method descriptor for main.Shape.area() -> Swift.Double",
            ),
            (
                "_$s4main5ShapeCACycfC",
                "main.Shape.__allocating_init() -> main.Shape",
            ),
            ("_$s4main5ShapeCACycfc", "main.Shape.init() -> main.Shape"),
            ("_$s4main5ShapeCfD", "main.Shape.__deallocating_deinit"),
            ("_$s4main5ShapeCfd", "main.Shape.deinit"),
            ("_$s4main5ShapeCMa", "type metadata accessor for main.Shape"),
            (
                "_$s4main5ShapeCMn",
                "nominal type descriptor for main.Shape",
            ),
            ("_$s4main5ShapeCN", "type metadata for main.Shape"),
            ("_$s4main5ShapeCMm", "metaclass for main.Shape"),
            (
                "_$s4main5ShapeCMF",
                "reflection metadata field descriptor main.Shape",
            ),
            ("_$s4main5ColorOWV", "value witness table for main.Color"),
            (
                "_$s4main8DrawableMp",
                "protocol descriptor for main.Drawable",
            ),
            ("_$s4mainMXM", "module descriptor main"),
            (
                "_$s4main5ColorOwug",
                "getEnumTag value witness for main.Color",
            ),
            (
                "_$s4main5PointVwet",
                "getEnumTagSinglePayload value witness for main.Point",
            ),
            (
                "_$s4main8DrawableTL",
                "protocol requirements base descriptor for main.Drawable",
            ),
            ("_$sSiN", "type metadata for Swift.Int"),
            ("_$sSHMp", "protocol descriptor for Swift.Hashable"),
            ("_$sBoWV", "value witness table for Builtin.NativeObject"),
            (
                "_$s4main5ColorO21__derived_enum_equalsySbAC_ACtFZ",
                "static main.Color.__derived_enum_equals(main.Color, main.Color) -> Swift.Bool",
            ),
            (
                "_$s4main5ColorO4hash4intoys6HasherVz_tF",
                "main.Color.hash(into: inout Swift.Hasher) -> ()",
            ),
            (
                "_$s4main5ColorO9hashValueSivg",
                "main.Color.hashValue.getter : Swift.Int",
            ),
            (
                "_$sSHsE13_rawHashValue4seedS2i_tF",
                "(extension in Swift):Swift.Hashable._rawHashValue(seed: Swift.Int) -> Swift.Int",
            ),
            (
                "_$sSS21_builtinStringLiteral17utf8CodeUnitCount7isASCIISSBp_BwBi1_tcfC",
                "Swift.String.init(_builtinStringLiteral: Builtin.RawPointer, \
                 utf8CodeUnitCount: Builtin.Word, isASCII: Builtin.Int1) -> Swift.String",
            ),
            (
                "_$s4main36VeryLongClassNameForWordSubstitutionC04veryc6MethodefG019substitutionTestingySi_tF",
                "main.VeryLongClassNameForWordSubstitution.veryLongMethodNameForWord(substitutionTesting: Swift.Int) -> ()",
            ),
            (
                "_$s4main5PointV1xSivM.resume.0",
                "main.Point.x.modify : Swift.Int with unmangled suffix \".resume.0\"",
            ),
        ];
        for (sym, want) in cases {
            assert_eq!(demangle_swift(sym).as_deref(), Some(want), "{sym}");
        }
    }

    #[test]
    fn unsupported_returns_none() {
        // generic signature
        assert_eq!(demangle_swift("_$ss10_hashValue3forSix_tSHRzlF"), None);
        // protocol conformance descriptor
        assert_eq!(demangle_swift("_$s4main5ColorOSHAAMc"), None);
        assert_eq!(demangle_swift("_main"), None);
        assert_eq!(demangle_swift("$s"), None);
        assert_eq!(demangle_swift("$s99abc"), None);
    }
}
