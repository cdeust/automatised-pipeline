// macro_expansion::typescript — TypeScript / Angular decorator rules.
//
// TypeScript decorators (stage-3 proposal, shipped 5.0+) behave like
// macros: `@Component({...})` rewrites the class at compile time by
// invoking the decorator function. These rules cover the well-known
// Angular stack (@Component, @Injectable, @NgModule, @Pipe, @Directive)
// and the plain ECMAScript decorator-function contract. React hooks
// (useState, useEffect) are pattern-matched as implicit calls via the
// Layer 4 rules below.
//
// source: Angular API reference https://angular.dev/api/core, React API
// reference https://react.dev/reference/react, TC39 decorators proposal
// https://github.com/tc39/proposal-decorators.

use super::{MacroExpansion, MacroTable};

pub struct TypeScriptMacros;

impl MacroTable for TypeScriptMacros {
    fn language(&self) -> &'static str {
        "typescript"
    }
    fn expansions(&self) -> &'static [MacroExpansion] {
        TS_MACROS
    }
}

pub const TS_MACROS: &[MacroExpansion] = &[
    MacroExpansion {
        macro_name: "Component",
        emit_calls: &["@angular/core.Component"],
        emit_implements: &[],
        language: "typescript",
    },
    MacroExpansion {
        macro_name: "Injectable",
        emit_calls: &["@angular/core.Injectable"],
        emit_implements: &[],
        language: "typescript",
    },
    MacroExpansion {
        macro_name: "NgModule",
        emit_calls: &["@angular/core.NgModule"],
        emit_implements: &[],
        language: "typescript",
    },
    MacroExpansion {
        macro_name: "Pipe",
        emit_calls: &["@angular/core.Pipe"],
        emit_implements: &[],
        language: "typescript",
    },
    MacroExpansion {
        macro_name: "Directive",
        emit_calls: &["@angular/core.Directive"],
        emit_implements: &[],
        language: "typescript",
    },
    MacroExpansion {
        macro_name: "Input",
        emit_calls: &["@angular/core.Input"],
        emit_implements: &[],
        language: "typescript",
    },
    MacroExpansion {
        macro_name: "Output",
        emit_calls: &["@angular/core.Output"],
        emit_implements: &[],
        language: "typescript",
    },
    MacroExpansion {
        macro_name: "useState",
        emit_calls: &["react.useState"],
        emit_implements: &[],
        language: "typescript",
    },
    MacroExpansion {
        macro_name: "useEffect",
        emit_calls: &["react.useEffect"],
        emit_implements: &[],
        language: "typescript",
    },
    MacroExpansion {
        macro_name: "useMemo",
        emit_calls: &["react.useMemo"],
        emit_implements: &[],
        language: "typescript",
    },
    MacroExpansion {
        macro_name: "useCallback",
        emit_calls: &["react.useCallback"],
        emit_implements: &[],
        language: "typescript",
    },
];
