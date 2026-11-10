//! Symbol demangling (RE-08).
//!
//! * C++ Itanium ABI (`_Z...`, Mach-O `__Z...`) via `cpp_demangle`;
//! * C++ MSVC (`?...`) via `msvc-demangler`;
//! * Rust legacy (`_ZN...17h<hash>E`) and v0 (`_R...`) via
//!   `rustc-demangle` (hashes are omitted from the output);
//! * Swift 5 via the built-in [`swift`] decoder (see its limits).

pub mod swift;

use serde::{Deserialize, Serialize};

/// Mangling scheme.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ManglingScheme {
    /// Itanium C++ ABI (GCC, Clang).
    Itanium,
    /// Microsoft Visual C++.
    Msvc,
    /// Rust legacy mangling (Itanium-shaped with a hash).
    RustLegacy,
    /// Rust v0 mangling.
    RustV0,
    /// Swift 5.
    Swift,
}

/// Demangling result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Demangled {
    /// Detected scheme.
    pub scheme: ManglingScheme,
    /// Human readable name.
    pub text: String,
}

fn strip_macho_underscore(s: &str) -> &str {
    if s.starts_with("__Z") || s.starts_with("__R") {
        &s[1..]
    } else {
        s
    }
}

fn is_rust_legacy(s: &str) -> bool {
    let Some(body) = s.strip_suffix('E') else {
        return false;
    };
    body.len() > 19
        && body[body.len() - 19..].starts_with("17h")
        && body[body.len() - 16..]
            .bytes()
            .all(|c| c.is_ascii_hexdigit())
        && s.starts_with("_ZN")
}

/// Guesses the scheme from the symbol prefix.
pub fn detect_scheme(symbol: &str) -> Option<ManglingScheme> {
    let s = strip_macho_underscore(symbol);
    if s.starts_with("_R") {
        Some(ManglingScheme::RustV0)
    } else if is_rust_legacy(s) {
        Some(ManglingScheme::RustLegacy)
    } else if s.starts_with("_Z") {
        Some(ManglingScheme::Itanium)
    } else if s.starts_with('?') {
        Some(ManglingScheme::Msvc)
    } else if ["$s", "_$s", "$S", "_$S", "$e"]
        .iter()
        .any(|p| s.starts_with(p))
    {
        Some(ManglingScheme::Swift)
    } else {
        None
    }
}

/// Demangles with a specific scheme.
pub fn demangle_with(scheme: ManglingScheme, symbol: &str) -> Option<String> {
    let s = strip_macho_underscore(symbol);
    match scheme {
        ManglingScheme::RustV0 | ManglingScheme::RustLegacy => rustc_demangle::try_demangle(s)
            .ok()
            .map(|d| format!("{d:#}")),
        ManglingScheme::Itanium => cpp_demangle::Symbol::new(s.as_bytes())
            .ok()?
            .demangle()
            .ok(),
        ManglingScheme::Msvc => {
            msvc_demangler::demangle(s, msvc_demangler::DemangleFlags::llvm()).ok()
        }
        ManglingScheme::Swift => swift::demangle_swift(symbol),
    }
}

/// Detects the scheme and demangles (RE-08). Returns `None` for plain C
/// names and undecodable symbols.
pub fn demangle(symbol: &str) -> Option<Demangled> {
    let scheme = detect_scheme(symbol)?;
    let text = demangle_with(scheme, symbol)?;
    (text != symbol).then_some(Demangled { scheme, text })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn itanium() {
        let d = demangle("_ZN3foo3barEi").unwrap();
        assert_eq!(d.scheme, ManglingScheme::Itanium);
        assert_eq!(d.text, "foo::bar(int)");
        assert_eq!(
            demangle("__ZNSt3__16vectorIiNS_9allocatorIiEEE9push_backERKi")
                .unwrap()
                .text,
            "std::__1::vector<int, std::__1::allocator<int> >::push_back(int const&)"
        );
        assert_eq!(demangle("_Z1fv").unwrap().text, "f()");
    }

    #[test]
    fn msvc() {
        let d = demangle("?world@@YA?AUhello@@XZ").unwrap();
        assert_eq!(d.scheme, ManglingScheme::Msvc);
        assert_eq!(d.text, "struct hello __cdecl world(void)");
        assert_eq!(
            demangle("??0Foo@@QAE@XZ").unwrap().text,
            "public: __thiscall Foo::Foo(void)"
        );
    }

    #[test]
    fn rust() {
        let d = demangle("_ZN4core3fmt5write17h0123456789abcdefE").unwrap();
        assert_eq!(d.scheme, ManglingScheme::RustLegacy);
        assert_eq!(d.text, "core::fmt::write");
        let d = demangle("_RNvCs1234_7mycrate3foo").unwrap();
        assert_eq!(d.scheme, ManglingScheme::RustV0);
        assert_eq!(d.text, "mycrate::foo");
        // Mach-O adds an underscore.
        assert_eq!(
            demangle("__ZN4core3fmt5write17h0123456789abcdefE")
                .unwrap()
                .text,
            "core::fmt::write"
        );
    }

    #[test]
    fn swift_and_plain() {
        let d = demangle("_$s4main4noopyyF").unwrap();
        assert_eq!(d.scheme, ManglingScheme::Swift);
        assert_eq!(d.text, "main.noop() -> ()");
        assert!(demangle("main").is_none());
        assert!(demangle("_printf").is_none());
        assert!(demangle("?").is_none());
        assert!(demangle("_Zxyz").is_none());
    }
}
