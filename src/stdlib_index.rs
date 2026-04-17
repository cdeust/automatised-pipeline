// stdlib_index — Layer 5 of the stage-3b-v2 resolver.
//
// Language-neutral stdlib symbol tables. Each language contributes one table
// via the StdlibTable trait. Dispatch is by language id string. The resolver
// consults these tables AFTER primary call resolution: a receiver with a
// known nominal type + a method name known in the stdlib table yields a
// StdlibSymbol node and a Calls_* edge with confidence 0.95.
//
// source: stages/stage-3b-v2.md §5 (Layer 5 — Stdlib index, universal in
// shape, per-language in data).

pub mod python;
pub mod rust;
pub mod typescript;

/// One entry in a per-language stdlib table.
///
/// The shape is identical across languages; the values are per-ecosystem.
/// `receiver_type` is the short display name as seen in source code
/// (e.g. `Vec`, not `alloc::vec::Vec`). `canonical_path` is the language's
/// canonical fully-qualified form used as the StdlibSymbol node id.
#[derive(Debug, Clone, Copy)]
#[allow(dead_code)] // is_method / language are inspected by future layers.
pub struct StdlibSymbol {
    pub receiver_type: &'static str,
    pub method_name: &'static str,
    pub canonical_path: &'static str,
    pub is_method: bool,
    pub language: &'static str,
}

/// Contract every per-language stdlib table implements.
#[allow(dead_code)] // language() is documentation for future language dispatch.
pub trait StdlibTable: Send + Sync {
    fn language(&self) -> &'static str;
    fn symbols(&self) -> &'static [StdlibSymbol];
}

/// Dispatch: language id -> table. Returns None for unknown languages so
/// callers can degrade gracefully.
pub fn get_stdlib_table(language: &str) -> Option<&'static dyn StdlibTable> {
    match language {
        "rust" => Some(&rust::RustStdlib),
        "python" => Some(&python::PythonStdlib),
        "typescript" => Some(&typescript::TypeScriptStdlib),
        _ => None,
    }
}

/// Lookup: (language, receiver_type, method_name) -> canonical_path.
/// O(n) scan of the per-language table; fine at ~200 entries each.
#[allow(dead_code)] // public API for future Layer 5 narrowing.
pub fn lookup(language: &str, receiver_type: &str, method_name: &str) -> Option<&'static StdlibSymbol> {
    let table = get_stdlib_table(language)?;
    table.symbols().iter().find(|s| {
        s.receiver_type == receiver_type && s.method_name == method_name
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_stdlib_table_size_rust() {
        // Target: at least 150 curated entries covering Vec/HashMap/String/
        // Option/Result/Iterator/Box/Rc/Arc and the other heavy hitters.
        let n = rust::RustStdlib.symbols().len();
        assert!(n >= 150, "rust stdlib table should have >=150 entries, got {n}");
    }

    #[test]
    fn test_stdlib_table_size_python() {
        let n = python::PythonStdlib.symbols().len();
        assert!(n >= 80, "python stdlib table should have >=80 entries, got {n}");
    }

    #[test]
    fn test_stdlib_table_size_typescript() {
        let n = typescript::TypeScriptStdlib.symbols().len();
        assert!(n >= 60, "typescript stdlib table should have >=60 entries, got {n}");
    }

    #[test]
    fn test_lookup_vec_push() {
        let sym = lookup("rust", "Vec", "push").expect("Vec::push must be indexed");
        assert_eq!(sym.canonical_path, "std::vec::Vec::push");
    }
}
