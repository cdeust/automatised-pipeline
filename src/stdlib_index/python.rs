// stdlib_index::python — Python standard library symbol table.
//
// source: Python 3.12 standard library reference,
// https://docs.python.org/3/library/ and the builtins module
// https://docs.python.org/3/library/stdtypes.html. `receiver_type` is the
// builtin display name (`list`, `dict`, `str`); `canonical_path` is the
// dotted path used as the StdlibSymbol id (builtins.<type>.<method>).

use super::{StdlibSymbol, StdlibTable};

pub struct PythonStdlib;

impl StdlibTable for PythonStdlib {
    fn language(&self) -> &'static str {
        "python"
    }
    fn symbols(&self) -> &'static [StdlibSymbol] {
        PYTHON_SYMBOLS
    }
}

macro_rules! m {
    ($recv:expr, $mname:expr, $path:expr) => {
        StdlibSymbol {
            receiver_type: $recv,
            method_name: $mname,
            canonical_path: $path,
            is_method: true,
            language: "python",
        }
    };
}

// source: https://docs.python.org/3/library/stdtypes.html for list/dict/str/
// set/tuple; https://docs.python.org/3/library/os.html for os.* ; same origin
// for the remaining modules. 80+ total.
pub const PYTHON_SYMBOLS: &[StdlibSymbol] = &[
    // list — source: https://docs.python.org/3/library/stdtypes.html#list
    m!("list", "append", "builtins.list.append"),
    m!("list", "extend", "builtins.list.extend"),
    m!("list", "insert", "builtins.list.insert"),
    m!("list", "remove", "builtins.list.remove"),
    m!("list", "pop", "builtins.list.pop"),
    m!("list", "clear", "builtins.list.clear"),
    m!("list", "index", "builtins.list.index"),
    m!("list", "count", "builtins.list.count"),
    m!("list", "sort", "builtins.list.sort"),
    m!("list", "reverse", "builtins.list.reverse"),
    m!("list", "copy", "builtins.list.copy"),
    // dict — source: https://docs.python.org/3/library/stdtypes.html#dict
    m!("dict", "get", "builtins.dict.get"),
    m!("dict", "keys", "builtins.dict.keys"),
    m!("dict", "values", "builtins.dict.values"),
    m!("dict", "items", "builtins.dict.items"),
    m!("dict", "update", "builtins.dict.update"),
    m!("dict", "pop", "builtins.dict.pop"),
    m!("dict", "popitem", "builtins.dict.popitem"),
    m!("dict", "setdefault", "builtins.dict.setdefault"),
    m!("dict", "clear", "builtins.dict.clear"),
    m!("dict", "copy", "builtins.dict.copy"),
    // str — source: https://docs.python.org/3/library/stdtypes.html#str
    m!("str", "split", "builtins.str.split"),
    m!("str", "rsplit", "builtins.str.rsplit"),
    m!("str", "splitlines", "builtins.str.splitlines"),
    m!("str", "join", "builtins.str.join"),
    m!("str", "strip", "builtins.str.strip"),
    m!("str", "lstrip", "builtins.str.lstrip"),
    m!("str", "rstrip", "builtins.str.rstrip"),
    m!("str", "replace", "builtins.str.replace"),
    m!("str", "find", "builtins.str.find"),
    m!("str", "rfind", "builtins.str.rfind"),
    m!("str", "index", "builtins.str.index"),
    m!("str", "count", "builtins.str.count"),
    m!("str", "startswith", "builtins.str.startswith"),
    m!("str", "endswith", "builtins.str.endswith"),
    m!("str", "upper", "builtins.str.upper"),
    m!("str", "lower", "builtins.str.lower"),
    m!("str", "capitalize", "builtins.str.capitalize"),
    m!("str", "title", "builtins.str.title"),
    m!("str", "format", "builtins.str.format"),
    m!("str", "encode", "builtins.str.encode"),
    m!("str", "isdigit", "builtins.str.isdigit"),
    m!("str", "isalpha", "builtins.str.isalpha"),
    m!("str", "isalnum", "builtins.str.isalnum"),
    m!("str", "isspace", "builtins.str.isspace"),
    // set — source: https://docs.python.org/3/library/stdtypes.html#set
    m!("set", "add", "builtins.set.add"),
    m!("set", "remove", "builtins.set.remove"),
    m!("set", "discard", "builtins.set.discard"),
    m!("set", "pop", "builtins.set.pop"),
    m!("set", "clear", "builtins.set.clear"),
    m!("set", "union", "builtins.set.union"),
    m!("set", "intersection", "builtins.set.intersection"),
    m!("set", "difference", "builtins.set.difference"),
    m!("set", "update", "builtins.set.update"),
    m!("set", "copy", "builtins.set.copy"),
    // tuple — source: https://docs.python.org/3/library/stdtypes.html#tuple
    m!("tuple", "count", "builtins.tuple.count"),
    m!("tuple", "index", "builtins.tuple.index"),
    // os — source: https://docs.python.org/3/library/os.html
    m!("os", "getcwd", "os.getcwd"),
    m!("os", "chdir", "os.chdir"),
    m!("os", "listdir", "os.listdir"),
    m!("os", "mkdir", "os.mkdir"),
    m!("os", "makedirs", "os.makedirs"),
    m!("os", "remove", "os.remove"),
    m!("os", "rename", "os.rename"),
    m!("os", "walk", "os.walk"),
    // os.path — source: https://docs.python.org/3/library/os.path.html
    m!("os.path", "join", "os.path.join"),
    m!("os.path", "exists", "os.path.exists"),
    m!("os.path", "isfile", "os.path.isfile"),
    m!("os.path", "isdir", "os.path.isdir"),
    m!("os.path", "dirname", "os.path.dirname"),
    m!("os.path", "basename", "os.path.basename"),
    m!("os.path", "splitext", "os.path.splitext"),
    m!("os.path", "abspath", "os.path.abspath"),
    // sys — source: https://docs.python.org/3/library/sys.html
    m!("sys", "exit", "sys.exit"),
    m!("sys", "argv", "sys.argv"),
    m!("sys", "stdout", "sys.stdout"),
    m!("sys", "stderr", "sys.stderr"),
    // json — source: https://docs.python.org/3/library/json.html
    m!("json", "loads", "json.loads"),
    m!("json", "dumps", "json.dumps"),
    m!("json", "load", "json.load"),
    m!("json", "dump", "json.dump"),
    // re — source: https://docs.python.org/3/library/re.html
    m!("re", "match", "re.match"),
    m!("re", "search", "re.search"),
    m!("re", "findall", "re.findall"),
    m!("re", "sub", "re.sub"),
    m!("re", "compile", "re.compile"),
    m!("re", "split", "re.split"),
];
