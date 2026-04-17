// macro_expansion::python — Python decorator expansion rules.
//
// Decorators in Python are the closest analog to Rust macros: a known
// decorator name rewrites the decorated symbol at import time. These rules
// capture the well-known stdlib decorators (dataclass, property,
// staticmethod, classmethod) whose implicit calls a static analyzer can
// attribute. Framework decorators (Flask @app.route, Django @csrf_exempt)
// are intentionally out of scope at Layer 4 stdlib — they belong in
// framework overlays.
//
// source: https://docs.python.org/3/library/dataclasses.html,
// https://docs.python.org/3/library/functions.html#property.

use super::{MacroExpansion, MacroTable};

pub struct PythonMacros;

impl MacroTable for PythonMacros {
    fn language(&self) -> &'static str {
        "python"
    }
    fn expansions(&self) -> &'static [MacroExpansion] {
        PYTHON_MACROS
    }
}

pub const PYTHON_MACROS: &[MacroExpansion] = &[
    // source: https://docs.python.org/3/library/dataclasses.html — @dataclass
    // synthesizes __init__, __repr__, __eq__; treat as Implements-like
    // relationships to the dataclass protocol for now.
    MacroExpansion {
        macro_name: "dataclass",
        emit_calls: &["dataclasses.dataclass"],
        emit_implements: &[],
        language: "python",
    },
    MacroExpansion {
        macro_name: "property",
        emit_calls: &["builtins.property"],
        emit_implements: &[],
        language: "python",
    },
    MacroExpansion {
        macro_name: "staticmethod",
        emit_calls: &["builtins.staticmethod"],
        emit_implements: &[],
        language: "python",
    },
    MacroExpansion {
        macro_name: "classmethod",
        emit_calls: &["builtins.classmethod"],
        emit_implements: &[],
        language: "python",
    },
    MacroExpansion {
        macro_name: "cached_property",
        emit_calls: &["functools.cached_property"],
        emit_implements: &[],
        language: "python",
    },
    MacroExpansion {
        macro_name: "lru_cache",
        emit_calls: &["functools.lru_cache"],
        emit_implements: &[],
        language: "python",
    },
    MacroExpansion {
        macro_name: "wraps",
        emit_calls: &["functools.wraps"],
        emit_implements: &[],
        language: "python",
    },
    MacroExpansion {
        macro_name: "abstractmethod",
        emit_calls: &["abc.abstractmethod"],
        emit_implements: &[],
        language: "python",
    },
];
