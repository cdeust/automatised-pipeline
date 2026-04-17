// macro_expansion — Layer 4 of the stage-3b-v2 resolver.
//
// Language-neutral macro / decorator / intrinsic expansion tables. Each
// language contributes rules that map a name-triggered site (a macro
// invocation, a decorator, a derive marker) to the set of canonical symbols
// it implicitly references. The parser emits synthetic ExtractedRefs
// (kind = "Calls" or "Implements") using the canonical path as the target,
// which the resolver then wires to StdlibSymbol nodes at confidence 0.85.
//
// source: stages/stage-3b-v2.md §5 (Layer 4 — universal strategy,
// per-language expansion data).

pub mod python;
pub mod rust;
pub mod typescript;

/// One expansion rule. `emit_calls` is the canonical-path set implied by a
/// call site; `emit_implements` is the canonical-trait set implied by a
/// derive/decorator marker.
#[derive(Debug, Clone, Copy)]
#[allow(dead_code)] // language is inspected by future multi-lang dispatch.
pub struct MacroExpansion {
    pub macro_name: &'static str,
    pub emit_calls: &'static [&'static str],
    pub emit_implements: &'static [&'static str],
    pub language: &'static str,
}

#[allow(dead_code)] // language() is documentation for future dispatch.
pub trait MacroTable: Send + Sync {
    fn language(&self) -> &'static str;
    fn expansions(&self) -> &'static [MacroExpansion];
}

pub fn get_macro_table(language: &str) -> Option<&'static dyn MacroTable> {
    match language {
        "rust" => Some(&rust::RustMacros),
        "python" => Some(&python::PythonMacros),
        "typescript" => Some(&typescript::TypeScriptMacros),
        _ => None,
    }
}

/// Lookup by macro name. O(n) scan; tables are small.
pub fn lookup(language: &str, macro_name: &str) -> Option<&'static MacroExpansion> {
    let table = get_macro_table(language)?;
    table.expansions().iter().find(|e| e.macro_name == macro_name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_rust_macro_count() {
        let n = rust::RustMacros.expansions().len();
        assert!(n >= 20, "rust macro table should have >=20 entries, got {n}");
    }

    #[test]
    fn test_lookup_println() {
        let exp = lookup("rust", "println").expect("println macro must be indexed");
        assert!(exp.emit_calls.contains(&"std::io::_print"));
    }

    #[test]
    fn test_derive_debug_implements() {
        let exp = lookup("rust", "derive_Debug").expect("derive_Debug must be indexed");
        assert!(exp.emit_implements.contains(&"std::fmt::Debug"));
        assert!(exp.emit_calls.is_empty());
    }
}
