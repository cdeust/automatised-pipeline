// stdlib_index::rust — Rust standard library symbol table.
//
// source: Rust std library API, https://doc.rust-lang.org/std/ (stable 1.94).
// Entries match documented method signatures; `receiver_type` is the
// simplified display name used in Rust source (`Vec` not `alloc::vec::Vec`);
// `canonical_path` is the canonical std:: path used as the StdlibSymbol id.

use super::{StdlibSymbol, StdlibTable};

pub struct RustStdlib;

impl StdlibTable for RustStdlib {
    fn language(&self) -> &'static str {
        "rust"
    }
    fn symbols(&self) -> &'static [StdlibSymbol] {
        RUST_SYMBOLS
    }
}

macro_rules! m {
    ($recv:expr, $mname:expr, $path:expr) => {
        StdlibSymbol {
            receiver_type: $recv,
            method_name: $mname,
            canonical_path: $path,
            is_method: true,
            language: "rust",
        }
    };
}

// source: https://doc.rust-lang.org/std/ (stable 1.94) — entries chosen
// from the methods actually documented on each listed type. 150+ total.
pub const RUST_SYMBOLS: &[StdlibSymbol] = &[
    // Vec — source: https://doc.rust-lang.org/std/vec/struct.Vec.html
    m!("Vec", "new", "std::vec::Vec::new"),
    m!("Vec", "with_capacity", "std::vec::Vec::with_capacity"),
    m!("Vec", "push", "std::vec::Vec::push"),
    m!("Vec", "pop", "std::vec::Vec::pop"),
    m!("Vec", "len", "std::vec::Vec::len"),
    m!("Vec", "is_empty", "std::vec::Vec::is_empty"),
    m!("Vec", "iter", "std::vec::Vec::iter"),
    m!("Vec", "iter_mut", "std::vec::Vec::iter_mut"),
    m!("Vec", "into_iter", "std::vec::Vec::into_iter"),
    m!("Vec", "get", "std::vec::Vec::get"),
    m!("Vec", "get_mut", "std::vec::Vec::get_mut"),
    m!("Vec", "clone", "std::vec::Vec::clone"),
    m!("Vec", "clear", "std::vec::Vec::clear"),
    m!("Vec", "contains", "std::vec::Vec::contains"),
    m!("Vec", "extend", "std::vec::Vec::extend"),
    m!("Vec", "insert", "std::vec::Vec::insert"),
    m!("Vec", "remove", "std::vec::Vec::remove"),
    m!("Vec", "sort", "std::vec::Vec::sort"),
    m!("Vec", "sort_by", "std::vec::Vec::sort_by"),
    m!("Vec", "sort_by_key", "std::vec::Vec::sort_by_key"),
    m!("Vec", "dedup", "std::vec::Vec::dedup"),
    m!("Vec", "drain", "std::vec::Vec::drain"),
    m!("Vec", "retain", "std::vec::Vec::retain"),
    m!("Vec", "truncate", "std::vec::Vec::truncate"),
    m!("Vec", "resize", "std::vec::Vec::resize"),
    m!("Vec", "split_off", "std::vec::Vec::split_off"),
    m!("Vec", "first", "std::vec::Vec::first"),
    m!("Vec", "last", "std::vec::Vec::last"),
    m!("Vec", "as_slice", "std::vec::Vec::as_slice"),
    m!("Vec", "as_mut_slice", "std::vec::Vec::as_mut_slice"),
    // VecDeque — source: https://doc.rust-lang.org/std/collections/struct.VecDeque.html
    m!("VecDeque", "new", "std::collections::VecDeque::new"),
    m!("VecDeque", "push_back", "std::collections::VecDeque::push_back"),
    m!("VecDeque", "push_front", "std::collections::VecDeque::push_front"),
    m!("VecDeque", "pop_back", "std::collections::VecDeque::pop_back"),
    m!("VecDeque", "pop_front", "std::collections::VecDeque::pop_front"),
    m!("VecDeque", "len", "std::collections::VecDeque::len"),
    m!("VecDeque", "is_empty", "std::collections::VecDeque::is_empty"),
    m!("VecDeque", "iter", "std::collections::VecDeque::iter"),
    // HashMap — source: https://doc.rust-lang.org/std/collections/struct.HashMap.html
    m!("HashMap", "new", "std::collections::HashMap::new"),
    m!("HashMap", "with_capacity", "std::collections::HashMap::with_capacity"),
    m!("HashMap", "get", "std::collections::HashMap::get"),
    m!("HashMap", "get_mut", "std::collections::HashMap::get_mut"),
    m!("HashMap", "insert", "std::collections::HashMap::insert"),
    m!("HashMap", "remove", "std::collections::HashMap::remove"),
    m!("HashMap", "contains_key", "std::collections::HashMap::contains_key"),
    m!("HashMap", "iter", "std::collections::HashMap::iter"),
    m!("HashMap", "iter_mut", "std::collections::HashMap::iter_mut"),
    m!("HashMap", "keys", "std::collections::HashMap::keys"),
    m!("HashMap", "values", "std::collections::HashMap::values"),
    m!("HashMap", "values_mut", "std::collections::HashMap::values_mut"),
    m!("HashMap", "len", "std::collections::HashMap::len"),
    m!("HashMap", "is_empty", "std::collections::HashMap::is_empty"),
    m!("HashMap", "entry", "std::collections::HashMap::entry"),
    m!("HashMap", "clear", "std::collections::HashMap::clear"),
    m!("HashMap", "drain", "std::collections::HashMap::drain"),
    // HashSet — source: https://doc.rust-lang.org/std/collections/struct.HashSet.html
    m!("HashSet", "new", "std::collections::HashSet::new"),
    m!("HashSet", "with_capacity", "std::collections::HashSet::with_capacity"),
    m!("HashSet", "insert", "std::collections::HashSet::insert"),
    m!("HashSet", "remove", "std::collections::HashSet::remove"),
    m!("HashSet", "contains", "std::collections::HashSet::contains"),
    m!("HashSet", "iter", "std::collections::HashSet::iter"),
    m!("HashSet", "len", "std::collections::HashSet::len"),
    m!("HashSet", "is_empty", "std::collections::HashSet::is_empty"),
    // BTreeMap — source: https://doc.rust-lang.org/std/collections/struct.BTreeMap.html
    m!("BTreeMap", "new", "std::collections::BTreeMap::new"),
    m!("BTreeMap", "get", "std::collections::BTreeMap::get"),
    m!("BTreeMap", "insert", "std::collections::BTreeMap::insert"),
    m!("BTreeMap", "remove", "std::collections::BTreeMap::remove"),
    m!("BTreeMap", "contains_key", "std::collections::BTreeMap::contains_key"),
    m!("BTreeMap", "iter", "std::collections::BTreeMap::iter"),
    m!("BTreeMap", "len", "std::collections::BTreeMap::len"),
    // BTreeSet — source: https://doc.rust-lang.org/std/collections/struct.BTreeSet.html
    m!("BTreeSet", "new", "std::collections::BTreeSet::new"),
    m!("BTreeSet", "insert", "std::collections::BTreeSet::insert"),
    m!("BTreeSet", "contains", "std::collections::BTreeSet::contains"),
    m!("BTreeSet", "iter", "std::collections::BTreeSet::iter"),
    // String / &str — source: https://doc.rust-lang.org/std/string/struct.String.html
    m!("String", "new", "std::string::String::new"),
    m!("String", "with_capacity", "std::string::String::with_capacity"),
    m!("String", "from", "std::string::String::from"),
    m!("String", "len", "std::string::String::len"),
    m!("String", "is_empty", "std::string::String::is_empty"),
    m!("String", "push_str", "std::string::String::push_str"),
    m!("String", "push", "std::string::String::push"),
    m!("String", "pop", "std::string::String::pop"),
    m!("String", "clone", "std::string::String::clone"),
    m!("String", "to_string", "std::string::String::to_string"),
    m!("String", "as_str", "std::string::String::as_str"),
    m!("String", "chars", "std::string::String::chars"),
    m!("String", "bytes", "std::string::String::bytes"),
    m!("String", "split", "std::string::String::split"),
    m!("String", "splitn", "std::string::String::splitn"),
    m!("String", "trim", "std::string::String::trim"),
    m!("String", "trim_start", "std::string::String::trim_start"),
    m!("String", "trim_end", "std::string::String::trim_end"),
    m!("String", "contains", "std::string::String::contains"),
    m!("String", "starts_with", "std::string::String::starts_with"),
    m!("String", "ends_with", "std::string::String::ends_with"),
    m!("String", "replace", "std::string::String::replace"),
    m!("String", "to_uppercase", "std::string::String::to_uppercase"),
    m!("String", "to_lowercase", "std::string::String::to_lowercase"),
    m!("String", "parse", "std::string::String::parse"),
    // Option — source: https://doc.rust-lang.org/std/option/enum.Option.html
    m!("Option", "unwrap", "std::option::Option::unwrap"),
    m!("Option", "unwrap_or", "std::option::Option::unwrap_or"),
    m!("Option", "unwrap_or_else", "std::option::Option::unwrap_or_else"),
    m!("Option", "unwrap_or_default", "std::option::Option::unwrap_or_default"),
    m!("Option", "expect", "std::option::Option::expect"),
    m!("Option", "is_some", "std::option::Option::is_some"),
    m!("Option", "is_none", "std::option::Option::is_none"),
    m!("Option", "as_ref", "std::option::Option::as_ref"),
    m!("Option", "as_mut", "std::option::Option::as_mut"),
    m!("Option", "take", "std::option::Option::take"),
    m!("Option", "replace", "std::option::Option::replace"),
    m!("Option", "map", "std::option::Option::map"),
    m!("Option", "and_then", "std::option::Option::and_then"),
    m!("Option", "or", "std::option::Option::or"),
    m!("Option", "or_else", "std::option::Option::or_else"),
    m!("Option", "ok_or", "std::option::Option::ok_or"),
    m!("Option", "ok_or_else", "std::option::Option::ok_or_else"),
    // Result — source: https://doc.rust-lang.org/std/result/enum.Result.html
    m!("Result", "unwrap", "std::result::Result::unwrap"),
    m!("Result", "unwrap_or", "std::result::Result::unwrap_or"),
    m!("Result", "unwrap_or_else", "std::result::Result::unwrap_or_else"),
    m!("Result", "expect", "std::result::Result::expect"),
    m!("Result", "is_ok", "std::result::Result::is_ok"),
    m!("Result", "is_err", "std::result::Result::is_err"),
    m!("Result", "ok", "std::result::Result::ok"),
    m!("Result", "err", "std::result::Result::err"),
    m!("Result", "map", "std::result::Result::map"),
    m!("Result", "map_err", "std::result::Result::map_err"),
    m!("Result", "and_then", "std::result::Result::and_then"),
    m!("Result", "or", "std::result::Result::or"),
    m!("Result", "or_else", "std::result::Result::or_else"),
    // Iterator — source: https://doc.rust-lang.org/std/iter/trait.Iterator.html
    m!("Iterator", "next", "std::iter::Iterator::next"),
    m!("Iterator", "map", "std::iter::Iterator::map"),
    m!("Iterator", "filter", "std::iter::Iterator::filter"),
    m!("Iterator", "fold", "std::iter::Iterator::fold"),
    m!("Iterator", "sum", "std::iter::Iterator::sum"),
    m!("Iterator", "count", "std::iter::Iterator::count"),
    m!("Iterator", "min", "std::iter::Iterator::min"),
    m!("Iterator", "max", "std::iter::Iterator::max"),
    m!("Iterator", "collect", "std::iter::Iterator::collect"),
    m!("Iterator", "enumerate", "std::iter::Iterator::enumerate"),
    m!("Iterator", "zip", "std::iter::Iterator::zip"),
    m!("Iterator", "chain", "std::iter::Iterator::chain"),
    m!("Iterator", "flat_map", "std::iter::Iterator::flat_map"),
    m!("Iterator", "filter_map", "std::iter::Iterator::filter_map"),
    m!("Iterator", "take", "std::iter::Iterator::take"),
    m!("Iterator", "skip", "std::iter::Iterator::skip"),
    m!("Iterator", "peekable", "std::iter::Iterator::peekable"),
    m!("Iterator", "rev", "std::iter::Iterator::rev"),
    m!("Iterator", "any", "std::iter::Iterator::any"),
    m!("Iterator", "all", "std::iter::Iterator::all"),
    m!("Iterator", "find", "std::iter::Iterator::find"),
    m!("Iterator", "position", "std::iter::Iterator::position"),
    m!("Iterator", "cloned", "std::iter::Iterator::cloned"),
    m!("Iterator", "copied", "std::iter::Iterator::copied"),
    // Box — source: https://doc.rust-lang.org/std/boxed/struct.Box.html
    m!("Box", "new", "std::boxed::Box::new"),
    m!("Box", "as_ref", "std::boxed::Box::as_ref"),
    m!("Box", "as_mut", "std::boxed::Box::as_mut"),
    // Rc — source: https://doc.rust-lang.org/std/rc/struct.Rc.html
    m!("Rc", "new", "std::rc::Rc::new"),
    m!("Rc", "clone", "std::rc::Rc::clone"),
    m!("Rc", "strong_count", "std::rc::Rc::strong_count"),
    m!("Rc", "weak_count", "std::rc::Rc::weak_count"),
    // Arc — source: https://doc.rust-lang.org/std/sync/struct.Arc.html
    m!("Arc", "new", "std::sync::Arc::new"),
    m!("Arc", "clone", "std::sync::Arc::clone"),
    m!("Arc", "strong_count", "std::sync::Arc::strong_count"),
    m!("Arc", "weak_count", "std::sync::Arc::weak_count"),
    // RefCell — source: https://doc.rust-lang.org/std/cell/struct.RefCell.html
    m!("RefCell", "new", "std::cell::RefCell::new"),
    m!("RefCell", "borrow", "std::cell::RefCell::borrow"),
    m!("RefCell", "borrow_mut", "std::cell::RefCell::borrow_mut"),
    m!("RefCell", "replace", "std::cell::RefCell::replace"),
    m!("RefCell", "take", "std::cell::RefCell::take"),
    // Cell — source: https://doc.rust-lang.org/std/cell/struct.Cell.html
    m!("Cell", "new", "std::cell::Cell::new"),
    m!("Cell", "get", "std::cell::Cell::get"),
    m!("Cell", "set", "std::cell::Cell::set"),
    m!("Cell", "replace", "std::cell::Cell::replace"),
    m!("Cell", "take", "std::cell::Cell::take"),
    // Mutex — source: https://doc.rust-lang.org/std/sync/struct.Mutex.html
    m!("Mutex", "new", "std::sync::Mutex::new"),
    m!("Mutex", "lock", "std::sync::Mutex::lock"),
    m!("Mutex", "try_lock", "std::sync::Mutex::try_lock"),
    // RwLock — source: https://doc.rust-lang.org/std/sync/struct.RwLock.html
    m!("RwLock", "new", "std::sync::RwLock::new"),
    m!("RwLock", "read", "std::sync::RwLock::read"),
    m!("RwLock", "write", "std::sync::RwLock::write"),
    // Path / PathBuf — source: https://doc.rust-lang.org/std/path/struct.Path.html
    m!("Path", "new", "std::path::Path::new"),
    m!("Path", "exists", "std::path::Path::exists"),
    m!("Path", "is_dir", "std::path::Path::is_dir"),
    m!("Path", "is_file", "std::path::Path::is_file"),
    m!("Path", "to_str", "std::path::Path::to_str"),
    m!("Path", "extension", "std::path::Path::extension"),
    m!("Path", "file_name", "std::path::Path::file_name"),
    m!("Path", "parent", "std::path::Path::parent"),
    m!("PathBuf", "new", "std::path::PathBuf::new"),
    m!("PathBuf", "join", "std::path::PathBuf::join"),
    m!("PathBuf", "push", "std::path::PathBuf::push"),
    m!("PathBuf", "pop", "std::path::PathBuf::pop"),
    // io — source: https://doc.rust-lang.org/std/io/
    m!("Read", "read", "std::io::Read::read"),
    m!("Read", "read_to_string", "std::io::Read::read_to_string"),
    m!("Write", "write", "std::io::Write::write"),
    m!("Write", "write_all", "std::io::Write::write_all"),
    m!("Write", "flush", "std::io::Write::flush"),
    m!("BufRead", "read_line", "std::io::BufRead::read_line"),
    m!("BufRead", "lines", "std::io::BufRead::lines"),
    // fmt — source: https://doc.rust-lang.org/std/fmt/
    m!("Display", "fmt", "std::fmt::Display::fmt"),
    m!("Debug", "fmt", "std::fmt::Debug::fmt"),
];
