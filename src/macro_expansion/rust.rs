// macro_expansion::rust — Rust macro + derive expansion rules.
//
// source: https://doc.rust-lang.org/std/ and the Rust Reference
// https://doc.rust-lang.org/reference/macros-by-example.html. Each rule is
// either (a) a `name!(...)` invocation whose canonical lowering involves
// known std:: symbols, or (b) a `derive_<Trait>` marker fabricated by the
// parser when it encounters `#[derive(Trait)]`, whose lowering is an
// `impl Trait for Struct` and thus an Implements edge.

use super::{MacroExpansion, MacroTable};

pub struct RustMacros;

impl MacroTable for RustMacros {
    fn language(&self) -> &'static str {
        "rust"
    }
    fn expansions(&self) -> &'static [MacroExpansion] {
        RUST_MACROS
    }
}

// source: https://doc.rust-lang.org/std/macro.println.html (and siblings) —
// each macro's official expansion documented by the std crate. Derive
// markers come from https://doc.rust-lang.org/reference/attributes/derive.html
// and the Rust book chapter 19.6.
pub const RUST_MACROS: &[MacroExpansion] = &[
    MacroExpansion {
        macro_name: "println",
        emit_calls: &["std::io::_print", "std::fmt::Arguments::new_v1"],
        emit_implements: &[],
        language: "rust",
    },
    MacroExpansion {
        macro_name: "eprintln",
        emit_calls: &["std::io::_eprint", "std::fmt::Arguments::new_v1"],
        emit_implements: &[],
        language: "rust",
    },
    MacroExpansion {
        macro_name: "print",
        emit_calls: &["std::io::_print"],
        emit_implements: &[],
        language: "rust",
    },
    MacroExpansion {
        macro_name: "eprint",
        emit_calls: &["std::io::_eprint"],
        emit_implements: &[],
        language: "rust",
    },
    MacroExpansion {
        macro_name: "format",
        emit_calls: &["std::fmt::format"],
        emit_implements: &[],
        language: "rust",
    },
    MacroExpansion {
        macro_name: "vec",
        emit_calls: &[
            "std::vec::Vec::new",
            "std::vec::Vec::push",
            "std::vec::Vec::with_capacity",
        ],
        emit_implements: &[],
        language: "rust",
    },
    MacroExpansion {
        macro_name: "panic",
        emit_calls: &["core::panicking::panic"],
        emit_implements: &[],
        language: "rust",
    },
    MacroExpansion {
        macro_name: "assert",
        emit_calls: &["core::panicking::assert_failed"],
        emit_implements: &[],
        language: "rust",
    },
    MacroExpansion {
        macro_name: "assert_eq",
        emit_calls: &["core::panicking::assert_failed"],
        emit_implements: &[],
        language: "rust",
    },
    MacroExpansion {
        macro_name: "assert_ne",
        emit_calls: &["core::panicking::assert_failed"],
        emit_implements: &[],
        language: "rust",
    },
    MacroExpansion {
        macro_name: "debug_assert",
        emit_calls: &["core::panicking::assert_failed"],
        emit_implements: &[],
        language: "rust",
    },
    MacroExpansion {
        macro_name: "debug_assert_eq",
        emit_calls: &["core::panicking::assert_failed"],
        emit_implements: &[],
        language: "rust",
    },
    MacroExpansion {
        macro_name: "dbg",
        emit_calls: &["std::io::_eprint"],
        emit_implements: &[],
        language: "rust",
    },
    MacroExpansion {
        macro_name: "todo",
        emit_calls: &["core::panicking::panic"],
        emit_implements: &[],
        language: "rust",
    },
    MacroExpansion {
        macro_name: "unimplemented",
        emit_calls: &["core::panicking::panic"],
        emit_implements: &[],
        language: "rust",
    },
    MacroExpansion {
        macro_name: "unreachable",
        emit_calls: &["core::panicking::panic"],
        emit_implements: &[],
        language: "rust",
    },
    MacroExpansion {
        macro_name: "write",
        emit_calls: &["std::fmt::Write::write_fmt", "std::io::Write::write_fmt"],
        emit_implements: &[],
        language: "rust",
    },
    MacroExpansion {
        macro_name: "writeln",
        emit_calls: &["std::fmt::Write::write_fmt", "std::io::Write::write_fmt"],
        emit_implements: &[],
        language: "rust",
    },
    // Derive markers — emit Implements edges, never Calls edges.
    // source: https://doc.rust-lang.org/reference/attributes/derive.html
    MacroExpansion {
        macro_name: "derive_Debug",
        emit_calls: &[],
        emit_implements: &["std::fmt::Debug"],
        language: "rust",
    },
    MacroExpansion {
        macro_name: "derive_Clone",
        emit_calls: &[],
        emit_implements: &["std::clone::Clone"],
        language: "rust",
    },
    MacroExpansion {
        macro_name: "derive_Copy",
        emit_calls: &[],
        emit_implements: &["std::marker::Copy"],
        language: "rust",
    },
    MacroExpansion {
        macro_name: "derive_PartialEq",
        emit_calls: &[],
        emit_implements: &["std::cmp::PartialEq"],
        language: "rust",
    },
    MacroExpansion {
        macro_name: "derive_Eq",
        emit_calls: &[],
        emit_implements: &["std::cmp::Eq"],
        language: "rust",
    },
    MacroExpansion {
        macro_name: "derive_Hash",
        emit_calls: &[],
        emit_implements: &["std::hash::Hash"],
        language: "rust",
    },
    MacroExpansion {
        macro_name: "derive_Default",
        emit_calls: &[],
        emit_implements: &["std::default::Default"],
        language: "rust",
    },
    MacroExpansion {
        macro_name: "derive_PartialOrd",
        emit_calls: &[],
        emit_implements: &["std::cmp::PartialOrd"],
        language: "rust",
    },
    MacroExpansion {
        macro_name: "derive_Ord",
        emit_calls: &[],
        emit_implements: &["std::cmp::Ord"],
        language: "rust",
    },
];
