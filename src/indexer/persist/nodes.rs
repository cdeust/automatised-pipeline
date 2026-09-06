// indexer::persist::nodes — node insertion from parsed results.
//
// Extracted from indexer/persist.rs (Fowler "Move Function") to keep the file
// under the §4.1 cap. Pure move: the node-property mapping concern (accumulate
// symbol nodes, PublicApi visibility gate, ExtractedNode→schema-column mapping)
// relocated verbatim. No behavior change.

use super::SymbolBatch;
use crate::graph_store::cypher_str;
use crate::parser;

// ---------------------------------------------------------------------------
// Node insertion from parsed results
// ---------------------------------------------------------------------------

pub(super) fn accumulate_parsed_nodes(
    batch: &mut SymbolBatch,
    nodes: &[parser::ExtractedNode],
    label_by_qn: &mut std::collections::HashMap<String, std::collections::HashSet<String>>,
    seen_node_ids: &mut std::collections::HashSet<(String, String)>,
    language: &str,
    restrict_to_public_api: bool,
) {
    // Accumulate into the cross-file batch (flushed in large bulk calls).
    // source: Fermi audit — per-row CREATE was ~100x slower than batched;
    // the April 2026 scalability audit further found per-FILE batching still
    // dominated indexing time, so accumulation now spans files.
    //
    // Defensive dedup: parsers should produce unique ids per node OF THE SAME
    // LABEL, but a bug there would abort the whole bulk flush (LadybugDB
    // rejects duplicate primary keys atomically), taking down every file in
    // the batch, not one. The id set is global to the run, so cross-file
    // collisions are caught too.
    //
    // Keyed on (label, qualified_name), not qualified_name alone: distinct
    // labels are separate tables in the graph schema (NODE_FIELD vs
    // NODE_METHOD, etc.), so a Field and a Method sharing the same
    // qualified_name string (e.g. a `len` field and a `len()` method on the
    // same struct) are two real, non-colliding nodes, not a parser bug — a
    // label-blind key silently dropped the second one. source: measured
    // 2026-09-03 on the dy-wcet corpus (get_symbol on `TaskSet::len`
    // returned symbol_not_found; only the Field node existed in the graph,
    // the Method was entirely absent).
    //
    // label_by_qn (below) records EVERY label ever seen for a qualified_name,
    // not just the last one written: Rust's namespace rules let a `mod foo {}`
    // and a `fn foo() {}` legally share one qualified_name (modules live in
    // the type namespace, functions/consts/statics in the value namespace —
    // Rust Reference §Namespaces), and `resolve_defines_table`'s to-candidate
    // list accepts both "Module" and "Function". A single-label map silently
    // kept whichever label was written last, mis-routing the OTHER structural
    // edge into the wrong (but schema-valid) rel table. source: verified
    // 2026-09-03 by parsing `mod foo { pub fn inner() {} }\nfn foo() {}`
    // through parser::parse_file(RUST_SPEC) — it emits one Module node and
    // one Function node both at qualified_name "src/lib.rs::foo", plus two
    // structurally-identical `Defines` refs (same from_qn, same to_qn) from
    // "src/lib.rs" to "src/lib.rs::foo". See indexer::persist::edges::
    // lookup_label_among for how the multi-label read side handles this.
    //
    // Enum qualified-names dropped by the PublicApi filter within THIS file's
    // node list. A Variant's own `visibility` is always "" — parsers never
    // declare it independently (source: src/parser/rust/extract/g2.rs:30) —
    // so a Variant is kept iff its parent Enum was kept. Scoped per-file
    // because `nodes` is one file's ExtractedNode list and parsers always
    // emit an Enum before its Variants within it.
    let mut dropped_enums: std::collections::HashSet<&str> = std::collections::HashSet::new();

    for node in nodes {
        if restrict_to_public_api && !keep_under_public_api(node, language, &mut dropped_enums) {
            continue;
        }
        if !seen_node_ids.insert((node.label.clone(), node.qualified_name.clone())) {
            eprintln!(
                "indexer: dropped duplicate-id {} node '{}'",
                node.label, node.qualified_name
            );
            continue;
        }
        label_by_qn
            .entry(node.qualified_name.clone())
            .or_default()
            .insert(node.label.clone());
        let props = build_node_properties(node, language);
        batch.push_node(&node.label, props);
    }
}

/// PublicApi-tier gate: true iff `node` belongs on the dependency's public
/// API surface. Only applied to files under dependency directories — see
/// `restrict_to_public_api` at the call site.
/// source: ADR-4253701 §Decision 1 ("public_api": only visibility==public
/// symbols persisted from dependency files).
fn keep_under_public_api<'a>(
    node: &'a parser::ExtractedNode,
    language: &str,
    dropped_enums: &mut std::collections::HashSet<&'a str>,
) -> bool {
    if node.label == "Variant" {
        let Some((enum_qn, _)) = node.qualified_name.rsplit_once("::") else {
            return true;
        };
        return !dropped_enums.contains(enum_qn);
    }
    if !is_visibility_declaring_label(&node.label) {
        // Import, CallSite, Module, File, Directory: no declared-visibility
        // contract to filter on. Kept as-is — they are structural/navigation
        // nodes, not part of the "public API surface" the tier scopes.
        return true;
    }
    if is_public_symbol(language, &node.visibility) {
        true
    } else {
        if node.label == "Enum" {
            dropped_enums.insert(node.qualified_name.as_str());
        }
        false
    }
}

/// True for node labels whose `visibility` field is genuinely populated by
/// every parser via an explicit visibility/export check. `Variant` is
/// excluded — see `keep_under_public_api`.
/// source: src/parser/rust/extract/g2.rs,g3.rs (Function/Method/Struct/Enum/
/// Trait/Field/Constant/TypeAlias all call extract_visibility()) and
/// src/parser/typescript/extract/g1.rs:47,79 (export-keyword check).
fn is_visibility_declaring_label(label: &str) -> bool {
    matches!(
        label,
        "Function" | "Method" | "Struct" | "Enum" | "Trait" | "Field" | "Constant" | "TypeAlias"
    )
}

/// True when `visibility` denotes a publicly visible symbol for `language`.
///
/// Python's parser convention has the OPPOSITE polarity of every other
/// supported language: `python_visibility` (src/parser/python/mod.rs:105-116,
/// tested at lines 205-211) emits "" for a PUBLIC name and "private" for an
/// underscore-prefixed one. Rust/TypeScript/JVM/Go/Swift emit "" when no
/// visibility keyword is present (module-private by default) and a keyword
/// token ("pub"/"export"/"public"/"open") when the symbol is public.
/// Deliberately NOT reusing clustering::process::PUBLIC_VISIBILITY_VALUES:
/// that list's "public" entry for Python never matches python_visibility's
/// actual output ("" or "private"), which would silently exclude every
/// Python symbol from this filter — a Bug-5-class inconsistency this
/// function avoids rather than propagates.
/// source: src/parser/{rust,typescript}/mod.rs visibility tests
/// (rust/mod.rs:190 "pub", typescript/mod.rs:217 "pub"); python/mod.rs:205-211.
fn is_public_symbol(language: &str, visibility: &str) -> bool {
    if language == "python" {
        visibility != "private"
    } else {
        matches!(visibility, "pub" | "export" | "public" | "open")
    }
}

/// Builds the full property list for a node, mapping ExtractedNode fields
/// to the schema columns defined in graph_store.rs node_table_ddl().
///
/// source: Spike B' BUG #5 fix — `language` is appended for every
/// symbol-bearing label (anything that isn't File / Directory) so consumers
/// can filter by language without re-parsing.
fn build_node_properties(node: &parser::ExtractedNode, language: &str) -> Vec<(String, String)> {
    let mut props = vec![("id".to_string(), cypher_str(&node.qualified_name))];
    if has_name_col(&node.label) {
        props.push(("name".to_string(), cypher_str(&node.name)));
    }
    if has_qualified_name_col(&node.label) {
        props.push((
            "qualified_name".to_string(),
            cypher_str(&node.qualified_name),
        ));
    }
    if has_line_cols(&node.label) {
        props.push(("start_line".to_string(), node.start_line.to_string()));
        props.push(("end_line".to_string(), node.end_line.to_string()));
    }
    if has_visibility_col(&node.label) {
        props.push(("visibility".to_string(), cypher_str(&node.visibility)));
    }
    append_label_properties(&mut props, node);
    if has_language_col(&node.label) {
        props.push(("language".to_string(), cypher_str(language)));
    }
    props
}

/// True for every symbol-bearing node label (everything carrying source-code
/// semantics). File and Directory are excluded — they cross language boundaries.
fn has_language_col(label: &str) -> bool {
    matches!(
        label,
        "Function"
            | "Method"
            | "Struct"
            | "Enum"
            | "Variant"
            | "Trait"
            | "Field"
            | "Constant"
            | "TypeAlias"
            | "Import"
            | "CallSite"
    )
}

/// Reads one extra property the parser attached to `node`, or "" when absent.
/// A free function (not a closure) so each per-label helper below can share
/// it without capturing `node` by reference through a closure boundary.
fn find_property(node: &parser::ExtractedNode, key: &str) -> String {
    node.properties
        .iter()
        .find(|(k, _)| k == key)
        .map(|(_, v)| v.clone())
        .unwrap_or_default()
}

/// Maps parser extra properties to schema columns by label.
///
/// One small per-label helper each (Fowler "Extract Function", §4.2): this
/// dispatch table used to inline every label's body directly, which grew the
/// function past the §4.2 cap one property at a time — a size cap that is
/// itself the failure mode this split closes off structurally rather than
/// re-opening at the next property added.
fn append_label_properties(props: &mut Vec<(String, String)>, node: &parser::ExtractedNode) {
    match node.label.as_str() {
        "Function" => {
            append_function_properties(props, node);
            props.push((
                "entry_kind".to_string(),
                cypher_str(&find_property(node, "entry_kind")),
            ));
        }
        "Method" => append_method_properties(props, node),
        // Field and Constant carry the identical single property.
        "Field" | "Constant" => props.push((
            "type_annotation".to_string(),
            cypher_str(&find_property(node, "type_annotation")),
        )),
        "TypeAlias" => props.push((
            "target_type".to_string(),
            cypher_str(&find_property(node, "target_type")),
        )),
        "Struct" | "Enum" | "Trait" => append_bases_and_implements(props, node),
        "Import" => append_import_properties(props, node),
        "CallSite" => append_callsite_properties(props, node),
        _ => {}
    }
}

fn append_function_properties(props: &mut Vec<(String, String)>, node: &parser::ExtractedNode) {
    props.push(("is_async".to_string(), find_property(node, "is_async")));
    // source: issue #92 — Uses-edge inputs; "" when the parser set none.
    props.push((
        "return_type".to_string(),
        cypher_str(&find_property(node, "return_type")),
    ));
    props.push((
        "constructed_types".to_string(),
        cypher_str(&find_property(node, "constructed_types")),
    ));
}

fn append_method_properties(props: &mut Vec<(String, String)>, node: &parser::ExtractedNode) {
    append_function_properties(props, node);
    props.push((
        "receiver_type".to_string(),
        cypher_str(&find_property(node, "receiver_type")),
    ));
    // source: implements fix — trait_name set by the parser on methods
    // inside `impl Trait for Type` blocks; resolve_implements reads it.
    props.push((
        "trait_name".to_string(),
        cypher_str(&find_property(node, "trait_name")),
    ));
}

/// source: Spike B' BUG #9 — bases CSV emitted by parser/python.rs for
/// class/struct/trait/enum nodes; consumed by resolver.resolve_extends.
/// implements fix — `implements` CSV (derived/declared trait names) is the
/// parallel column consumed by resolver.resolve_implements.
fn append_bases_and_implements(props: &mut Vec<(String, String)>, node: &parser::ExtractedNode) {
    props.push((
        "bases".to_string(),
        cypher_str(&find_property(node, "bases")),
    ));
    props.push((
        "implements".to_string(),
        cypher_str(&find_property(node, "implements")),
    ));
}

fn append_import_properties(props: &mut Vec<(String, String)>, node: &parser::ExtractedNode) {
    props.push(("path".to_string(), cypher_str(&find_property(node, "path"))));
    props.push((
        "alias".to_string(),
        cypher_str(&find_property(node, "alias")),
    ));
    props.push(("is_glob".to_string(), find_property(node, "is_glob")));
    // §10.1 span for the import statement; §10.4 is_resolved starts false
    // and is flipped by the resolver's resolve pass.
    props.push(("start_line".to_string(), node.start_line.to_string()));
    props.push(("end_line".to_string(), node.end_line.to_string()));
    props.push(("is_resolved".to_string(), "false".to_string()));
}

fn append_callsite_properties(props: &mut Vec<(String, String)>, node: &parser::ExtractedNode) {
    props.push((
        "callee_name".to_string(),
        cypher_str(&find_property(node, "callee_name")),
    ));
    props.push(("line".to_string(), node.start_line.to_string()));
    // source: LSP 3.17 Base Protocol §Text Documents — positions are
    // 0-based. Every parser spec now emits the call node's raw tree-sitter
    // 0-based column as the `lsp_col` property (the qualified_name's own
    // embedded column is 1-based in most specs, used only for id
    // uniqueness — see call_site()/call_entry() in src/parser/spec/*.rs).
    // Falls back to 0 only if a parser spec omitted the property, which is
    // a parser bug to fix at the source, not a legitimate "column 0" call
    // site.
    let lsp_col = find_property(node, "lsp_col").parse::<u64>().unwrap_or(0);
    props.push(("col".to_string(), lsp_col.to_string()));
    // §10.4 is_resolved starts false; the resolver flips it to true when it
    // emits the resolved Calls edge for this site.
    props.push(("is_resolved".to_string(), "false".to_string()));
}

// Schema awareness — source: graph_store.rs node_table_ddl().
// Each function returns true iff the label's CREATE NODE TABLE includes that column.

fn has_name_col(label: &str) -> bool {
    // All node tables have `name` EXCEPT Import (path/alias only).
    // CallSite stores callee_name via properties, not via 'name' column.
    !matches!(label, "Import" | "CallSite")
}

fn has_qualified_name_col(label: &str) -> bool {
    matches!(
        label,
        "Module"
            | "Function"
            | "Method"
            | "Struct"
            | "Enum"
            | "Variant"
            | "Trait"
            | "Constant"
            | "TypeAlias"
    )
}

fn has_line_cols(label: &str) -> bool {
    // source: stages/stage-3.md §10.1 — every symbol carries its span. Variant,
    // Field, Constant and TypeAlias now have span columns too (Import keeps its
    // span via append_label_properties, alongside path/alias). CallSite records
    // position via its own line/col columns, not start_line/end_line.
    matches!(
        label,
        "Function"
            | "Method"
            | "Struct"
            | "Enum"
            | "Trait"
            | "Variant"
            | "Field"
            | "Constant"
            | "TypeAlias"
    )
}

fn has_visibility_col(label: &str) -> bool {
    matches!(
        label,
        "Function" | "Method" | "Struct" | "Enum" | "Trait" | "Field"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds a minimal ExtractedNode for dedup testing — only `label` and
    /// `qualified_name` matter to `accumulate_parsed_nodes`'s dedup key.
    fn node(label: &str, qualified_name: &str) -> parser::ExtractedNode {
        parser::ExtractedNode {
            label: label.to_string(),
            name: qualified_name.rsplit("::").next().unwrap_or("").to_string(),
            qualified_name: qualified_name.to_string(),
            start_line: 1,
            end_line: 1,
            visibility: "pub".to_string(),
            properties: Vec::new(),
        }
    }

    /// Regression test for the label-blind dedup bug: a struct field and a
    /// method sharing the identical qualified_name string (e.g. Rust's
    /// `len: usize` field alongside a `pub const fn len(&self)` method) are
    /// two real, distinct nodes stored in separate schema tables (NODE_FIELD
    /// vs NODE_METHOD) — not a parser-produced id collision. Fails on the
    /// pre-fix `HashSet<String>` keyed on qualified_name alone, which drops
    /// the second node regardless of label.
    #[test]
    fn a_field_and_a_method_sharing_a_qualified_name_are_both_kept() {
        let mut batch = SymbolBatch::default();
        let mut label_by_qn = std::collections::HashMap::new();
        let mut seen_node_ids = std::collections::HashSet::new();
        let nodes = vec![
            node("Field", "src/lib.rs::TaskSet::len"),
            node("Method", "src/lib.rs::TaskSet::len"),
        ];

        accumulate_parsed_nodes(
            &mut batch,
            &nodes,
            &mut label_by_qn,
            &mut seen_node_ids,
            "rust",
            false,
        );

        assert_eq!(
            batch.node_row_count, 2,
            "both the Field and the Method node must be kept — they are \
             distinct nodes in separate schema tables, not a duplicate id"
        );
    }

    /// Two nodes of the SAME label colliding on qualified_name is exactly the
    /// parser-bug signal this dedup set is meant to catch — must still drop
    /// the second one, not regress into keeping both.
    #[test]
    fn two_nodes_of_the_same_label_and_qualified_name_drop_the_duplicate() {
        let mut batch = SymbolBatch::default();
        let mut label_by_qn = std::collections::HashMap::new();
        let mut seen_node_ids = std::collections::HashSet::new();
        let nodes = vec![
            node("Method", "src/lib.rs::TaskSet::len"),
            node("Method", "src/lib.rs::TaskSet::len"),
        ];

        accumulate_parsed_nodes(
            &mut batch,
            &nodes,
            &mut label_by_qn,
            &mut seen_node_ids,
            "rust",
            false,
        );

        assert_eq!(
            batch.node_row_count, 1,
            "a genuine same-label id collision must still drop the duplicate"
        );
    }
}
