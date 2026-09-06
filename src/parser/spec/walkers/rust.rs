// parser::spec::walkers::rust — the Rust definition walker (ADR-0055 phase 8).
// Reproduces the pre-migration hand-written Rust walker (`parser::rust::extract`,
// 1649 LOC) at exact parity, driven by the `RustFamilySpec` sub-table instead of
// hardcoded `TS_*` constants.
//
// Rust fits none of the three existing shapes — not the class-model `walk_defs`
// (its `impl` blocks emit no node of their own and re-scope their methods onto a
// receiver type they do not declare), not the flat `clike` walker (it has
// modules, traits and variants), and not the hybrid `cpp` walker (cross-sibling
// attribute state and two import edge kinds). So it gets this dedicated walker and
// `walk_defs` delegates here whenever a `LangSpec` carries
// `rust_family: Some(_)` — the #109 (`clike`) / #125 (`cpp`) precedent. The seven
// languages riding `walk_defs`/`clike`/`cpp` stay untouched. Calls and imports
// still route through the SHARED generic walkers (`calls::walk_calls`,
// `imports::walk_imports`) and supertraits through the shared
// `types::collect_bases`; only the definition shapes are Rust-specific.
//
// This file is the walker SPINE: the dispatcher, the derive-attribute
// accumulator, the shared `Def`/`push_def` emission, and the flat items
// (functions, consts, macro definitions, type aliases, modules). Every emitter
// whose unit of work is a TYPE and its members — structs/unions with fields,
// enums with variants, traits with requirements, `impl` blocks with methods —
// lives in the sibling `rust_types` module (§4.1, 500-line cap).
//
// Two extraction defects the #60 phase-8 migration preserved for parity were
// fixed deliberately afterward (behavior-changing, ground truth re-captured):
//   - #130: an `impl` inside a module now scopes its methods to
//     `{module_qn}::{Type}` (the QN the `Struct`/`Enum` node already uses), so
//     `mod helpers { impl Inner { … } }` yields `file.rs::helpers::Inner::toggle`;
//   - #131: a trait requirement's DEFAULT body is now scanned for calls exactly as
//     an `impl` method's body is (a bodiless requirement stays call-free).

use tree_sitter::Node;

use super::super::lang_spec::{LangSpec, RustFamilySpec};
use super::{
    call_scan_of, calls, end_line_of, imports, kind_in, line_of, rust_types, type_uses, WalkCtx,
};
use crate::parser::{
    node_field_text, node_text, qual, ExtractedNode, ExtractedRef, LABEL_CONSTANT, LABEL_FUNCTION,
    LABEL_MODULE, LABEL_TYPE_ALIAS,
};

/// The two spec halves a Rust emitter always needs together: the shared
/// `LangSpec` row and the Rust sub-table it delegates through. They are never
/// apart — `rf` IS `spec.rust_family` already unwrapped — so they travel as one
/// parameter object, which is what keeps every emitter below at four arguments
/// or fewer (§4.4).
#[derive(Copy, Clone)]
pub(super) struct RustSpecs<'a> {
    pub spec: &'a LangSpec,
    pub rf: &'a RustFamilySpec,
}

/// Where an item is being emitted and which `#[derive(...)]` traits are pending
/// for it. A parameter object (§4.4): the two always arrive together, and only
/// for the struct/union and enum emitters — the only items derives apply to.
#[derive(Copy, Clone)]
pub(super) struct DeriveScope<'a> {
    pub scope: &'a str,
    pub derives: &'a [String],
}

/// One definition node plus the single edge that owns it. A parameter object
/// (§4.4) so every emitter stays at three arguments and the node-then-edge
/// emission ORDER — which the parity pin asserts — is written once. Shared with
/// the type/member emitters in `rust_types`.
pub(super) struct Def<'a> {
    pub label: &'static str,
    pub name: &'a str,
    pub qn: &'a str,
    pub visibility: String,
    pub properties: Vec<(String, String)>,
    pub edge_kind: &'static str,
    pub edge_from: &'a str,
}

/// Appends `d`'s node and then its owning edge.
/// precondition: `d.name` is non-empty and `d.qn` is its qualified name.
/// postcondition: exactly one node and one ref are appended, node first.
pub(super) fn push_def(ctx: &mut WalkCtx, node: Node, d: Def) {
    ctx.nodes.push(ExtractedNode {
        label: d.label.to_string(),
        name: d.name.to_string(),
        qualified_name: d.qn.to_string(),
        start_line: line_of(node),
        end_line: end_line_of(node),
        visibility: d.visibility,
        properties: d.properties,
    });
    ctx.refs.push(ExtractedRef {
        kind: d.edge_kind.to_string(),
        from_qualified_name: d.edge_from.to_string(),
        to_qualified_name: d.qn.to_string(),
    });
}

/// Rust item-list walker: dispatches each child of `parent` to the concern its
/// node kind names in `specs.rf`, threading the pending-derive accumulator across
/// siblings. The dispatched kinds are disjoint, so order among the arms is safe.
///
/// invariant: `pending_derives` holds exactly the derive names of the
/// `attribute_kinds` run immediately preceding the current child, interrupted
/// only by children that match no arm.
pub(super) fn walk_rust_defs(specs: RustSpecs, ctx: &mut WalkCtx, parent: Node, scope: &str) {
    let rf = specs.rf;
    let mut cursor = parent.walk();
    // Derive-trait names accumulated from preceding `#[derive(...)]` siblings,
    // applied to the next struct/enum/union and cleared by any OTHER dispatched
    // item. A child matching no arm (a comment, an attribute that is not a
    // derive) leaves the accumulator intact — the hand-written walker's `_ => {}`
    // arm did not clear it, so `#[derive(Debug)]` followed by a comment still
    // derives on the struct after it.
    // source: stages/stage-3b-v2.md §5 Layer 4 (derive → Implements).
    let mut pending_derives: Vec<String> = Vec::new();
    for child in parent.children(&mut cursor) {
        let k = child.kind();
        if kind_in(rf.attribute_kinds, k) {
            collect_derives(ctx.source, child, &mut pending_derives);
            continue;
        }
        let ds = DeriveScope {
            scope,
            derives: &pending_derives,
        };
        if kind_in(rf.function_kinds, k) {
            emit_function(specs, ctx, child, scope);
        } else if kind_in(rf.struct_like_kinds, k) {
            rust_types::emit_struct(specs, ctx, child, ds);
        } else if kind_in(rf.enum_kinds, k) {
            rust_types::emit_enum(specs, ctx, child, ds);
        } else if kind_in(rf.trait_kinds, k) {
            rust_types::emit_trait(specs, ctx, child, scope);
        } else if kind_in(rf.impl_kinds, k) {
            rust_types::emit_impl(specs, ctx, child, scope);
        } else if kind_in(rf.constant_kinds, k) {
            emit_constant(specs.spec, ctx, child, scope);
        } else if kind_in(rf.macro_def_kinds, k) {
            emit_macro_def(specs.spec, ctx, child, scope);
        } else if kind_in(rf.type_alias_kinds, k) {
            emit_type_alias(specs.spec, ctx, child, scope);
        } else if kind_in(rf.use_kinds, k) || kind_in(rf.extern_crate_kinds, k) {
            // Both import statement kinds route through the shared import
            // walker; `RustConventions::imports_of` shapes each and
            // `import_ref_kind` picks the edge (`Defines` vs `Imports`).
            imports::walk_imports(specs.spec, ctx, child, scope);
        } else if kind_in(rf.mod_kinds, k) {
            emit_mod(specs, ctx, child, scope);
        } else {
            continue;
        }
        pending_derives.clear();
    }
}

/// Appends the trait names of a `#[derive(A, B, C)]` attribute to `out`. A
/// non-derive attribute (`#[non_exhaustive]`) contributes nothing and — because
/// the caller does not reset — leaves an earlier derive run intact.
/// source: Rust Reference, https://doc.rust-lang.org/reference/attributes/derive.html.
fn collect_derives(source: &str, node: Node, out: &mut Vec<String>) {
    let text = node_text(source, node);
    let trimmed = text.trim();
    let inner = match trimmed.strip_prefix("#[").and_then(|s| s.strip_suffix(']')) {
        Some(s) => s.trim(),
        None => return,
    };
    let payload = match inner
        .strip_prefix("derive(")
        .and_then(|s| s.strip_suffix(')'))
    {
        Some(s) => s,
        None => return,
    };
    for tok in payload.split(',') {
        let name = tok.trim();
        if !name.is_empty() {
            out.push(name.to_string());
        }
    }
}

/// The `implements` property (a CSV of derived trait names) the resolver's
/// Layer-4 pass reads to bind each derive to a local or stdlib trait. Empty when
/// nothing is derived — the property is then absent, not blank.
pub(super) fn implements_props(derives: &[String]) -> Vec<(String, String)> {
    if derives.is_empty() {
        Vec::new()
    } else {
        vec![("implements".to_string(), derives.join(","))]
    }
}

/// Emits one synthetic `DeriveImplements` ref per derived trait, from the item's
/// QN to the bare trait name, AFTER the item's own node/edges — the order the
/// hand-written dispatcher produced. The resolver's Layer 4 maps each through the
/// macro table.
pub(super) fn emit_derive_implements(
    spec: &LangSpec,
    ctx: &mut WalkCtx,
    item: Node,
    ds: DeriveScope,
) {
    if ds.derives.is_empty() {
        return;
    }
    let name = node_field_text(ctx.source, item, spec.name_field);
    if name.is_empty() {
        return;
    }
    let from_qn = qual(ds.scope, &name);
    for trait_name in ds.derives {
        ctx.refs.push(ExtractedRef {
            kind: "DeriveImplements".to_string(),
            from_qualified_name: from_qn.clone(),
            to_qualified_name: trait_name.clone(),
        });
    }
}

/// Whether a function carries the `async` modifier: an `async_kind` grandchild
/// under the node's `function_modifiers_kind` child.
pub(super) fn has_async(rf: &RustFamilySpec, node: Node) -> bool {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() != rf.function_modifiers_kind {
            continue;
        }
        let mut inner = child.walk();
        if child
            .children(&mut inner)
            .any(|gc| gc.kind() == rf.async_kind)
        {
            return true;
        }
    }
    false
}

/// The `decl_list_kinds` body of a trait/impl/module node, or `None` when the
/// node is bodiless (`mod foo;`) or its body is some other kind.
pub(super) fn decl_list_body<'t>(specs: RustSpecs, node: Node<'t>) -> Option<Node<'t>> {
    match specs
        .spec
        .body_field
        .and_then(|f| node.child_by_field_name(f))
    {
        Some(b) if kind_in(specs.rf.decl_list_kinds, b.kind()) => Some(b),
        _ => None,
    }
}

/// Classifies only exact, argument-free outer attribute paths. These markers
/// declare harness entry points, never successful test/proof execution.
/// Sources: Rust Reference attributes/testing.html; Kani reference/attributes.html.
fn entry_attribute(source: &str, item: Node) -> Option<&'static str> {
    let attribute = item.named_child(0)?;
    if attribute.kind() != "attribute"
        || attribute.child_by_field_name("arguments").is_some()
        || attribute.child_by_field_name("value").is_some()
    {
        return None;
    }
    let path = attribute.named_child(0)?;
    if path.kind() == "identifier" && node_text(source, path) == "test" {
        return Some("test");
    }
    if path.kind() != "scoped_identifier" {
        return None;
    }
    let prefix = path.child_by_field_name("path")?;
    let name = path.child_by_field_name("name")?;
    (prefix.kind() == "identifier"
        && node_text(source, prefix) == "kani"
        && name.kind() == "identifier"
        && node_text(source, name) == "proof")
        .then_some("proof")
}

/// Outer attributes belong to the immediately following item; comments may
/// intervene. Stop at any other item, so an attribute cannot leak to a sibling.
fn function_entry_kind(source: &str, node: Node) -> Option<&'static str> {
    let mut sibling = node.prev_named_sibling();
    let mut kind = None;
    while let Some(previous) = sibling {
        match previous.kind() {
            "attribute_item" => match entry_attribute(source, previous) {
                Some("proof") => return Some("proof"),
                Some(marker) => kind = Some(marker),
                None => {}
            },
            "line_comment" | "block_comment" => {}
            _ => break,
        }
        sibling = previous.prev_named_sibling();
    }
    kind
}

/// Emits a free function (`Function` + `Defines`) and scans its body for calls.
fn emit_function(specs: RustSpecs, ctx: &mut WalkCtx, node: Node, scope: &str) {
    let spec = specs.spec;
    let name = node_field_text(ctx.source, node, spec.name_field);
    if name.is_empty() {
        return;
    }
    let seq = ctx.next_seq();
    let qn = spec.conventions.def_qn(scope, &name, seq);
    let mut properties = vec![(
        "is_async".to_string(),
        has_async(specs.rf, node).to_string(),
    )];
    if let Some(kind) = function_entry_kind(ctx.source, node) {
        properties.push(("entry_kind".to_string(), kind.to_string()));
    }
    // Return type + constructed types (issue #92).
    type_uses::append_type_use_props(spec, ctx, node, call_scan_of(spec, node), &mut properties);
    push_def(
        ctx,
        node,
        Def {
            label: LABEL_FUNCTION,
            name: &name,
            qn: &qn,
            visibility: spec.conventions.node_visibility(ctx.source, node, &name),
            properties,
            edge_kind: "Defines",
            edge_from: scope,
        },
    );
    if let Some(body) = call_scan_of(spec, node) {
        calls::walk_calls(spec, ctx, body, &qn);
    }
}

/// Emits a `const` or `static` item as a `Constant` + `Defines` carrying its
/// `type_annotation` (both kinds expose the same `name`/`type` fields).
fn emit_constant(spec: &LangSpec, ctx: &mut WalkCtx, node: Node, scope: &str) {
    let name = node_field_text(ctx.source, node, spec.name_field);
    if name.is_empty() {
        return;
    }
    let qn = qual(scope, &name);
    let type_ann = node_field_text(ctx.source, node, spec.type_field);
    push_def(
        ctx,
        node,
        Def {
            label: LABEL_CONSTANT,
            name: &name,
            qn: &qn,
            visibility: spec.conventions.node_visibility(ctx.source, node, &name),
            properties: vec![("type_annotation".to_string(), type_ann)],
            edge_kind: "Defines",
            edge_from: scope,
        },
    );
}

/// Emits a `macro_rules!` definition as a `Constant` (`is_macro=true`) +
/// `Defines`. AP has no dedicated Macro label, and this keeps a macro definition
/// queryable and consistent with macro *invocations*, which are already tracked
/// as `CallSite`s with a trailing `!`.
fn emit_macro_def(spec: &LangSpec, ctx: &mut WalkCtx, node: Node, scope: &str) {
    let name = node_field_text(ctx.source, node, spec.name_field);
    if name.is_empty() {
        return;
    }
    let qn = qual(scope, &name);
    push_def(
        ctx,
        node,
        Def {
            label: LABEL_CONSTANT,
            name: &name,
            qn: &qn,
            visibility: spec.conventions.node_visibility(ctx.source, node, &name),
            properties: vec![("is_macro".to_string(), "true".to_string())],
            edge_kind: "Defines",
            edge_from: scope,
        },
    );
}

/// Emits a `type` item as a `TypeAlias` + `Defines` carrying the aliased type as
/// `target_type`.
fn emit_type_alias(spec: &LangSpec, ctx: &mut WalkCtx, node: Node, scope: &str) {
    let name = node_field_text(ctx.source, node, spec.name_field);
    if name.is_empty() {
        return;
    }
    let qn = qual(scope, &name);
    let target = node_field_text(ctx.source, node, spec.type_field);
    push_def(
        ctx,
        node,
        Def {
            label: LABEL_TYPE_ALIAS,
            name: &name,
            qn: &qn,
            visibility: spec.conventions.node_visibility(ctx.source, node, &name),
            properties: vec![("target_type".to_string(), target)],
            edge_kind: "Defines",
            edge_from: scope,
        },
    );
}

/// Emits a module (`Module` + `Defines`) and recurses its inline body under the
/// module's QN. A `mod foo;` declaration has no body and recurses nothing.
fn emit_mod(specs: RustSpecs, ctx: &mut WalkCtx, node: Node, scope: &str) {
    let spec = specs.spec;
    let name = node_field_text(ctx.source, node, spec.name_field);
    if name.is_empty() {
        return;
    }
    let qn = qual(scope, &name);
    push_def(
        ctx,
        node,
        Def {
            label: LABEL_MODULE,
            name: &name,
            qn: &qn,
            visibility: spec.conventions.node_visibility(ctx.source, node, &name),
            properties: Vec::new(),
            edge_kind: "Defines",
            edge_from: scope,
        },
    );
    if let Some(body) = decl_list_body(specs, node) {
        walk_rust_defs(specs, ctx, body, &qn);
    }
}
