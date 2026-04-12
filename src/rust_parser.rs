// rust_parser — backward-compatibility re-export.
//
// The actual implementation lives in parser::rust. This module re-exports
// the public API so that existing tests and external code continue to work.

#[allow(unused_imports)]
pub use crate::parser::{ExtractedNode, ExtractedRef, ParseResult};
#[allow(unused_imports)]
pub use crate::parser::rust::parse_rust_file;
