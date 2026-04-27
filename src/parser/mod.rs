// parser — Multi-language tree-sitter parser module.
//
// Dispatches to language-specific parsers (Rust, Python, TypeScript).
// All parsers produce the same ParseResult/ExtractedNode/ExtractedRef types.
// The indexer calls `parse_file(source, file_path, language)` and gets a
// uniform result regardless of language.

pub mod c;
pub mod cpp;
pub mod go;
pub mod java;
pub mod kotlin;
pub mod objc;
pub mod python;
pub mod rust;
pub mod swift;
pub mod typescript;

// source: H2 fix — per-file tree-sitter parse timeout. 5 seconds is ~100x
// the typical parse time for a 1 MB source file on an M-series Mac (measured
// ~50 ms in practice) and matches the default suggested in the tree-sitter
// CLI (`--time-limit 5`). Parser::parse returns None when this is exceeded.
pub(crate) const PARSE_TIMEOUT_MICROS: u64 = 5_000_000;

use tree_sitter::Node;

// ---------------------------------------------------------------------------
// Supported languages
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Language {
    Rust,
    Python,
    TypeScript,
    Java,
    Kotlin,
    Swift,
    ObjC,
    C,
    Cpp,
    Go,
}

impl Language {
    /// Detects language from file extension. Returns None for unsupported.
    pub fn from_extension(ext: &str) -> Option<Self> {
        match ext {
            "rs" => Some(Language::Rust),
            "py" => Some(Language::Python),
            "ts" | "tsx" => Some(Language::TypeScript),
            "java" => Some(Language::Java),
            "kt" | "kts" => Some(Language::Kotlin),
            "swift" => Some(Language::Swift),
            // ``.m`` is ObjC; ``.mm`` is ObjC++ which we handle with the
            // ObjC grammar (it supports mixed C++ constructs via embedded
            // C++ rules; full fidelity would require a separate parser).
            "m" | "mm" => Some(Language::ObjC),
            // ``.h`` is ambiguous (C/C++/ObjC). Default to C to cover the
            // majority case; projects that need C++ headers parsed as C++
            // can pass ``language: "cpp"`` explicitly.
            "c" | "h" => Some(Language::C),
            "cc" | "cpp" | "cxx" | "hh" | "hpp" | "hxx" => Some(Language::Cpp),
            "go" => Some(Language::Go),
            _ => None,
        }
    }

    #[allow(dead_code)]
    pub fn as_str(&self) -> &'static str {
        match self {
            Language::Rust => "rust",
            Language::Python => "python",
            Language::TypeScript => "typescript",
            Language::Java => "java",
            Language::Kotlin => "kotlin",
            Language::Swift => "swift",
            Language::ObjC => "objc",
            Language::C => "c",
            Language::Cpp => "cpp",
            Language::Go => "go",
        }
    }

    /// Parses a language string from the tool schema.
    pub fn from_str_opt(s: &str) -> Option<Self> {
        match s {
            "rust" => Some(Language::Rust),
            "python" => Some(Language::Python),
            "typescript" => Some(Language::TypeScript),
            "java" => Some(Language::Java),
            "kotlin" => Some(Language::Kotlin),
            "swift" => Some(Language::Swift),
            "objc" | "objective-c" => Some(Language::ObjC),
            "c" => Some(Language::C),
            "cpp" | "c++" => Some(Language::Cpp),
            "go" => Some(Language::Go),
            _ => None,
        }
    }
}

// ---------------------------------------------------------------------------
// Output labels — match graph_store NODE_* constants by value, not by import.
// ---------------------------------------------------------------------------

pub const LABEL_FUNCTION: &str = "Function";
pub const LABEL_METHOD: &str = "Method";
pub const LABEL_STRUCT: &str = "Struct";
pub const LABEL_ENUM: &str = "Enum";
pub const LABEL_VARIANT: &str = "Variant";
pub const LABEL_TRAIT: &str = "Trait";
pub const LABEL_FIELD: &str = "Field";
pub const LABEL_CONSTANT: &str = "Constant";
pub const LABEL_TYPE_ALIAS: &str = "TypeAlias";
pub const LABEL_IMPORT: &str = "Import";
pub const LABEL_MODULE: &str = "Module";
pub const LABEL_CALL_SITE: &str = "CallSite";

// ---------------------------------------------------------------------------
// Public types — shared contract for all language parsers
// ---------------------------------------------------------------------------

pub struct ParseResult {
    pub nodes: Vec<ExtractedNode>,
    pub refs: Vec<ExtractedRef>,
}

pub struct ExtractedNode {
    pub label: String,
    pub name: String,
    pub qualified_name: String,
    pub start_line: u64,
    pub end_line: u64,
    pub visibility: String,
    pub properties: Vec<(String, String)>,
}

pub struct ExtractedRef {
    pub kind: String,
    pub from_qualified_name: String,
    pub to_qualified_name: String,
}

// ---------------------------------------------------------------------------
// Unified entry point
// ---------------------------------------------------------------------------

/// Parses a source file and extracts typed symbols and relationships.
/// Dispatches to the appropriate language-specific parser.
pub fn parse_file(source: &str, file_path: &str, lang: Language) -> Result<ParseResult, String> {
    match lang {
        Language::Rust => rust::parse_rust_file(source, file_path),
        Language::Python => python::parse_python_file(source, file_path),
        Language::TypeScript => typescript::parse_typescript_file(source, file_path),
        Language::Java => java::parse_java_file(source, file_path),
        Language::Kotlin => kotlin::parse_kotlin_file(source, file_path),
        Language::Swift => swift::parse_swift_file(source, file_path),
        Language::ObjC => objc::parse_objc_file(source, file_path),
        Language::C => c::parse_c_file(source, file_path),
        Language::Cpp => cpp::parse_cpp_file(source, file_path),
        Language::Go => go::parse_go_file(source, file_path),
    }
}

// ---------------------------------------------------------------------------
// Shared helpers used by all language parsers
// ---------------------------------------------------------------------------

/// Extracts the full text of a node from source.
pub(crate) fn node_text(source: &str, node: Node) -> String {
    source[node.byte_range()].to_string()
}

/// Extracts text from a named field of a node. Returns empty string if absent.
pub(crate) fn node_field_text(source: &str, node: Node, field: &str) -> String {
    node.child_by_field_name(field)
        .map(|n| node_text(source, n))
        .unwrap_or_default()
}

/// Builds a qualified name with `::` separator (normalized form).
pub(crate) fn qual(scope: &str, name: &str) -> String {
    format!("{scope}::{name}")
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_language_from_extension() {
        assert_eq!(Language::from_extension("rs"), Some(Language::Rust));
        assert_eq!(Language::from_extension("py"), Some(Language::Python));
        assert_eq!(Language::from_extension("ts"), Some(Language::TypeScript));
        assert_eq!(Language::from_extension("tsx"), Some(Language::TypeScript));
        assert_eq!(Language::from_extension("js"), None);
        assert_eq!(Language::from_extension("go"), None);
    }

    #[test]
    fn test_language_from_str() {
        assert_eq!(Language::from_str_opt("rust"), Some(Language::Rust));
        assert_eq!(Language::from_str_opt("python"), Some(Language::Python));
        assert_eq!(Language::from_str_opt("typescript"), Some(Language::TypeScript));
        assert_eq!(Language::from_str_opt("auto"), None);
        assert_eq!(Language::from_str_opt("java"), None);
    }

    #[test]
    fn test_qual() {
        assert_eq!(qual("src/main.rs", "foo"), "src/main.rs::foo");
    }
}
