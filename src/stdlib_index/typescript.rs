// stdlib_index::typescript — TypeScript / JavaScript builtins symbol table.
//
// source: TC39 ECMAScript 2024 specification (Array.prototype, String.prototype,
// Object, Map, Set, Promise builtins) and the TypeScript lib.es2024.d.ts
// declarations that ship with typescript@5.4. `receiver_type` is the builtin
// type name; `canonical_path` is the prototype-chain path used as the
// StdlibSymbol id.

use super::{StdlibSymbol, StdlibTable};

pub struct TypeScriptStdlib;

impl StdlibTable for TypeScriptStdlib {
    fn language(&self) -> &'static str {
        "typescript"
    }
    fn symbols(&self) -> &'static [StdlibSymbol] {
        TS_SYMBOLS
    }
}

macro_rules! m {
    ($recv:expr, $mname:expr, $path:expr) => {
        StdlibSymbol {
            receiver_type: $recv,
            method_name: $mname,
            canonical_path: $path,
            is_method: true,
            language: "typescript",
        }
    };
}

// source: TC39 ECMAScript 2024 specification + TypeScript lib.es2024.d.ts.
// 60+ total, covering the high-frequency builtins (Array/String/Map/Set/
// Promise/Object/JSON/Math/console).
pub const TS_SYMBOLS: &[StdlibSymbol] = &[
    // Array.prototype — source: TC39 §23.1.3 Array.prototype
    m!("Array", "push", "Array.prototype.push"),
    m!("Array", "pop", "Array.prototype.pop"),
    m!("Array", "shift", "Array.prototype.shift"),
    m!("Array", "unshift", "Array.prototype.unshift"),
    m!("Array", "slice", "Array.prototype.slice"),
    m!("Array", "splice", "Array.prototype.splice"),
    m!("Array", "concat", "Array.prototype.concat"),
    m!("Array", "join", "Array.prototype.join"),
    m!("Array", "reverse", "Array.prototype.reverse"),
    m!("Array", "sort", "Array.prototype.sort"),
    m!("Array", "map", "Array.prototype.map"),
    m!("Array", "filter", "Array.prototype.filter"),
    m!("Array", "forEach", "Array.prototype.forEach"),
    m!("Array", "reduce", "Array.prototype.reduce"),
    m!("Array", "reduceRight", "Array.prototype.reduceRight"),
    m!("Array", "find", "Array.prototype.find"),
    m!("Array", "findIndex", "Array.prototype.findIndex"),
    m!("Array", "indexOf", "Array.prototype.indexOf"),
    m!("Array", "lastIndexOf", "Array.prototype.lastIndexOf"),
    m!("Array", "includes", "Array.prototype.includes"),
    m!("Array", "some", "Array.prototype.some"),
    m!("Array", "every", "Array.prototype.every"),
    m!("Array", "flat", "Array.prototype.flat"),
    m!("Array", "flatMap", "Array.prototype.flatMap"),
    m!("Array", "entries", "Array.prototype.entries"),
    m!("Array", "keys", "Array.prototype.keys"),
    m!("Array", "values", "Array.prototype.values"),
    m!("Array", "from", "Array.from"),
    m!("Array", "isArray", "Array.isArray"),
    m!("Array", "of", "Array.of"),
    // String.prototype — source: TC39 §22.1.3
    m!("String", "charAt", "String.prototype.charAt"),
    m!("String", "charCodeAt", "String.prototype.charCodeAt"),
    m!("String", "concat", "String.prototype.concat"),
    m!("String", "includes", "String.prototype.includes"),
    m!("String", "indexOf", "String.prototype.indexOf"),
    m!("String", "lastIndexOf", "String.prototype.lastIndexOf"),
    m!("String", "match", "String.prototype.match"),
    m!("String", "matchAll", "String.prototype.matchAll"),
    m!("String", "padStart", "String.prototype.padStart"),
    m!("String", "padEnd", "String.prototype.padEnd"),
    m!("String", "repeat", "String.prototype.repeat"),
    m!("String", "replace", "String.prototype.replace"),
    m!("String", "replaceAll", "String.prototype.replaceAll"),
    m!("String", "slice", "String.prototype.slice"),
    m!("String", "split", "String.prototype.split"),
    m!("String", "startsWith", "String.prototype.startsWith"),
    m!("String", "endsWith", "String.prototype.endsWith"),
    m!("String", "toLowerCase", "String.prototype.toLowerCase"),
    m!("String", "toUpperCase", "String.prototype.toUpperCase"),
    m!("String", "trim", "String.prototype.trim"),
    m!("String", "trimStart", "String.prototype.trimStart"),
    m!("String", "trimEnd", "String.prototype.trimEnd"),
    // Object — source: TC39 §20.1
    m!("Object", "keys", "Object.keys"),
    m!("Object", "values", "Object.values"),
    m!("Object", "entries", "Object.entries"),
    m!("Object", "assign", "Object.assign"),
    m!("Object", "freeze", "Object.freeze"),
    m!("Object", "fromEntries", "Object.fromEntries"),
    // Map / Set — source: TC39 §24.1 / §24.2
    m!("Map", "get", "Map.prototype.get"),
    m!("Map", "set", "Map.prototype.set"),
    m!("Map", "has", "Map.prototype.has"),
    m!("Map", "delete", "Map.prototype.delete"),
    m!("Map", "clear", "Map.prototype.clear"),
    m!("Map", "forEach", "Map.prototype.forEach"),
    m!("Set", "add", "Set.prototype.add"),
    m!("Set", "has", "Set.prototype.has"),
    m!("Set", "delete", "Set.prototype.delete"),
    m!("Set", "clear", "Set.prototype.clear"),
    // Promise — source: TC39 §27.2
    m!("Promise", "then", "Promise.prototype.then"),
    m!("Promise", "catch", "Promise.prototype.catch"),
    m!("Promise", "finally", "Promise.prototype.finally"),
    m!("Promise", "all", "Promise.all"),
    m!("Promise", "race", "Promise.race"),
    m!("Promise", "resolve", "Promise.resolve"),
    m!("Promise", "reject", "Promise.reject"),
    // JSON — source: TC39 §25.5
    m!("JSON", "parse", "JSON.parse"),
    m!("JSON", "stringify", "JSON.stringify"),
    // console — source: WHATWG Console standard
    m!("console", "log", "console.log"),
    m!("console", "error", "console.error"),
    m!("console", "warn", "console.warn"),
    m!("console", "info", "console.info"),
    m!("console", "debug", "console.debug"),
];
