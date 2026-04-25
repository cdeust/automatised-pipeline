// resolver_layers — Layer 4 (macro expansion) + Layer 5 (stdlib index)
// passes for the stage-3b-v2 resolver. Lives separate from resolver.rs so
// that Q8 (symbols-in-file) ground truth for resolver.rs remains stable as
// new passes are added. source: stages/stage-3b-v2.md §5.

use crate::graph_store::{cypher_str, GraphStore};
use std::collections::HashSet;

/// Entry point for Layer 4 (macros + derives).
pub fn run_macro_expansion(
    store: &GraphStore,
    buf: &mut crate::resolver::EdgeBuffer,
    caller_label_of: &dyn Fn(&str) -> String,
) -> Result<u64, String> {
    let mut created: HashSet<String> = HashSet::new();
    let macro_edges = expand_macro_calls(store, buf, caller_label_of, &mut created)?;
    Ok(macro_edges)
}

fn expand_macro_calls(
    store: &GraphStore,
    buf: &mut crate::resolver::EdgeBuffer,
    caller_label_of: &dyn Fn(&str) -> String,
    created: &mut HashSet<String>,
) -> Result<u64, String> {
    // The parser emits synthetic ExtractedRefs with kind="CallsMacro" that
    // the indexer drops (no matching rel-table). Re-reading is impossible
    // without another parse, so Layer 4 reads CallSite-less fallback: any
    // Function / Method whose body contains a `name!(...)` invocation is
    // represented in the graph as a CallSite only when the file has been
    // parsed by this binary. For files parsed under the new extractor, the
    // CallsMacro refs are dropped — so we rely on the post-parse resolver
    // pass looking at the raw `CallSite` nodes via a fast path: any
    // `callee_name` ending with `!` is a macro marker the parser emitted
    // BEFORE the CallsMacro rewrite (legacy). The new extractor does not
    // emit CallSite nodes for macros; therefore this pass currently covers
    // the legacy path only. When the indexer learns CallsMacro, the full
    // Layer 4 will light up.
    let qr = store.execute_query(
        "MATCH (cs:CallSite) RETURN cs.id, cs.callee_name"
    )?;
    let mut resolved = 0u64;
    for row in &qr.rows {
        if row.len() < 2 {
            continue;
        }
        let cs_id = &row[0];
        let callee = &row[1];
        let macro_name = match callee.strip_suffix('!') {
            Some(n) => n,
            None => continue,
        };
        let expansion = match crate::macro_expansion::lookup("rust", macro_name) {
            Some(e) => e,
            None => continue,
        };
        let caller_qn = caller_from_callsite(cs_id);
        let caller_label = caller_label_of(&caller_qn);
        // source: stages/stage-3b.md §2 — Calls_*_StdlibSymbol is defined
        // for Function|Method callers only. Skip non-callable callers.
        if caller_label != "Function" && caller_label != "Method" {
            continue;
        }
        for canonical in expansion.emit_calls {
            ensure_stdlib_symbol(store, created, canonical, "rust")?;
            let rel = format!("Calls_{caller_label}_StdlibSymbol");
            if buf.add(&rel, &caller_qn, canonical, 0.85, "macro-expansion") {
                resolved += 1;
            }
        }
    }
    Ok(resolved)
}

fn caller_from_callsite(cs_id: &str) -> String {
    if let Some(idx) = cs_id.rfind("::call@") {
        cs_id[..idx].to_string()
    } else {
        cs_id.to_string()
    }
}

/// Idempotently create a StdlibSymbol node. source: stages/stage-3b-v2.md §5.
pub fn ensure_stdlib_symbol(
    store: &GraphStore,
    created: &mut HashSet<String>,
    canonical_path: &str,
    language: &str,
) -> Result<(), String> {
    if created.contains(canonical_path) {
        return Ok(());
    }
    let name = canonical_path
        .rsplit("::")
        .next()
        .unwrap_or(canonical_path);
    let receiver_type = crate::stdlib_index::get_stdlib_table(language)
        .and_then(|t| t.symbols().iter().find(|s| s.canonical_path == canonical_path))
        .map(|s| s.receiver_type)
        .unwrap_or("");
    let cypher = format!(
        "CREATE (n:StdlibSymbol {{id: {}, name: {}, language: {}, \
         receiver_type: {}, canonical_path: {}}})",
        cypher_str(canonical_path),
        cypher_str(name),
        cypher_str(language),
        cypher_str(receiver_type),
        cypher_str(canonical_path),
    );
    // Ignore duplicate-key errors; the idempotency set prevents double
    // creation within a run, and LadybugDB rejects duplicate primary keys.
    let _ = store.execute_query(&cypher);
    created.insert(canonical_path.to_string());
    Ok(())
}
