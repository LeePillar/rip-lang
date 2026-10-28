# Rip Language Development Specification v1.3
> [中文](RipLang-level.md) | **English**

> This document is the complete specification for compiler implementers. Rip is a strongly typed, no inference, class-free language with reference counting plus cycle collection that preserves Rust ownership semantics, targeting server/Web use, with the suffix `.rp`, the tool `rip`, and the package manifest `rip.toml`. **Core principle: single canonical form.** The same semantics allow only one syntax; aliases, sugar, and second spellings are forbidden (except the convenience sugar explicitly listed in this specification). **Implementation goal**: dual mode of interpreter + AOT compiler, hosted on Rust. **Authoritative boundary (mandatory)**: the deliverable of this repository is the Rip runtime and toolchain implemented in Rust (shipping compiled artifacts or Rust source). Rip is a general-purpose language for everyone, not an internal scripting language of any single project. `demo/` is merely a validation example written in Rip; **examples submit to the language specification, the language specification does not submit to examples**—any practice of modifying lexical, syntactic, type, ownership, or standard library semantics in order to make some demo work is forbidden.

---

## 0. Delivery Form and Demo Boundary

### 0.1 Public Release

There are only two legal forms of release; nothing else is published:

| Form | Content | What users need |
| --- | --- | --- |
| Runtime binary | The `rip` binary compiled with Rust plus its bundled runtime library directory | Use it directly to run / package / compile natively (see 10.11) |
| Runtime source | Rust workspace source | Toolchain maintainers only; `cargo build` is required to obtain `rip` |

The language specification, syntax manual, and directory specification are documentation deliverables and are distributed together with the runtime.

### 0.2 Two Execution Modes

| Mode | Command | Dependency |
| --- | --- | --- |
| Direct source execution | `rip run`, `rip dev` | The target machine must have the `rip` runtime installed |
| Native binary (AOT) | `rip build` | The artifact is a native executable; the target machine needs no runtime and does not contain Rip source |

- Source execution is used for development and hot updates in operations; native binaries are used for delivery and deployment.
- `rip build` performs full AOT by default: it translates all Rip syntax into the host program, links the runtime library distributed with `rip`, and then generates a native binary with the built-in compile component.
- The `--aot` flag is retained for compatibility with old scripts and behaves identically to the default.
- The two modes must be semantically identical: the same source must have the same behavior, diagnostics, and exit codes under `run` and `build`; `rip test --diff <dir>` automatically compares them file by file.
- The former "interpreter + embedded source" single-file artifact has been removed: it is neither more convenient than source execution nor lighter than a native binary, so it is a middle ground that wins in neither direction.

**Execution model**: the runtime consists of a "bytecode fast path + tree-walking interpreter".

- Pure scalar functions (local variables, arithmetic/comparison/logic, `if`/`for`, function calls, module and method calls) are compiled into stack-based bytecode for execution; variables are accessed by slot, with no more name-hash lookups.
- Functions containing structs, `vec`/`map`, `match`, closures, globals, `?`, and similar syntax fall back entirely to the interpreter; both paths share the same set of operation, calling, and error semantics, and `#[test]` and `demo/server` regression cases must produce identical output on both.
- Bytecode compile-time optimizations: literal constant folding, scope-based recycling and reuse of local slots, `int`-slot-specific instructions (slot op constant, slot self-update, conditional direct jump), hot-expression fusion (`(x op const) op y` and `expr % const` written back directly), and direct calls to already-compiled functions by index; overflow/division-by-zero/type-mismatch still falls back to the unified implementation and reports the same error.
- Bytecode is compiled only once: the compilation result is shared with `RuntimeSeed` as an `Arc`, so new runtimes created by HTTP worker threads, `parallel` branches, `task.spawn`, and parallel tests do not recompile it; strings inside instructions are likewise `Arc`, so the compiled artifact can be reused across threads.
- Memory model: containers are `Rc<RefCell<…>>` and are released immediately when acyclic; cycles such as "container → closure → captured container" are found by the per-thread cycle collector using trial deletion, which finds those without external references and clears them (triggered automatically at allocation intervals at the end of a request/test, or manually via `sys.gc()`). `sys.heap_usage()` returns the heap usage reported by the counting allocator; `[server] max_heap_mb` sets the process heap limit; when exceeded, collection runs first, and if it is still exceeded the current request fails.
- Debugging switch: setting `RIP_NO_VM=1` completely disables the bytecode path and forces the interpreter for behavior comparison.

**Experimental native compilation (`rip build --native`)**:

- Translates the "scalar int subset" into the host program, then invokes the built-in compile component to generate a native executable; intermediate source is written only to **`.tmp/` under the current project root** and deleted when compilation finishes (the directory is deleted too if it becomes empty), so it never appears in the delivery directory.
- Supported scope: `int`/`void` parameters and return values, local `int` variables, arithmetic/comparison/logic, `if`/`else`, both forms of `for`, `return`/`break`/`continue`, direct function calls (including recursion), string literals and `+` concatenation, `int.to_string()`, `print` (single int/string), and int constants.
- Anything outside this scope (structs, `vec`/`map`, `match`, closures, module calls, `db`/`http`, mutable globals, etc.) produces an explicit error suggesting `rip build` instead, and **never** generates an artifact with inconsistent semantics.
- This path is still experimental and is suitable for compute-intensive tasks and performance validation; business applications (databases, HTTP) use the full AOT of `rip build`.

**Full AOT (`rip build` default)**:

- Translates **all Rip syntax** into the host program (value model, operations, and calls uniformly use the runtime `Value`), links the host support library, and then generates a native binary with the built-in compile component; it bypasses the interpreter and does not embed Rip source.
- Coverage: functions/recursion, closures and captures, `struct`/`vec`/`map`, `match` (`some`/`none`/`ok`/`err`/structs), `loop`/`while`/`while let`/`for-in`, `?`, modules and third-party packages, and all of `std/*`.
- Native scheduling: the generated binary carries a function table, an annotation route table (`#[get]`, etc.), and a `#[interval]` scheduled task table; HTTP worker threads, `task.spawn`, and `ws.serve` are rebuilt by name, with behavior identical to interpreted mode; `[runtime]`, `[server]`, and `[cluster]` in `rip.toml` are written into the artifact at build time.
- Command line: the generated binary accepts `--port` / `--host` / `--cert` / `--key`, the same as `rip run`.
- Build dependency: the user's machine **does not need** any compilation tooling installed; the compile component, linker, and runtime library are distributed with `rip` (see 10.11). On Windows, the MinGW target uses the toolchain-bundled `rust-lld` and the MSVC target uses the system `link.exe`.
- **Clean artifact**: by default it links `rp-core` (`rp-runtime` with the `interp` feature disabled), so the artifact contains no AST interpreter and no bytecode VM; if `rp-core` cannot be found it falls back to linking `rp-runtime` and prints a notice. See 10.9 for build instructions.
- Difference from `--native`: `--native` covers only the scalar int subset and produces a fully standalone artifact; `rip build` covers the whole language and its artifact still links the host support library (already statically linked into the binary).

**Platform rules (mandatory)**:

| Platform | Source + rip runtime | rip build artifact |
| --- | --- | --- |
| Windows | Supported | .exe (PE format) |
| Linux | Supported | Extensionless executable (ELF) |
| macOS | Supported | Extensionless executable (Mach-O) |
| Other Unix | Supported (when the Rust target exists) | Native executable format of that platform |

- Artifact formats follow the target platform's native standard; no custom container or script wrapper is introduced.
- The default is to build for the current platform; `rip build --target <triple>` (or `[build] target`) enables cross-compilation. Cross-compilation requires the target standard library, linker, and C dependency toolchain; see the build dependency table in 10.13.
- When the target triple equals the host, the local fast path is still used (calling the built-in compiler directly), bypassing cargo.

**Packaging directory rules (mandatory)**: `rip build` does not produce just one binary; it produces a self-contained delivery directory (default `dist/`), and copying the whole directory at delivery time is enough to run it. The directory must contain:

| Content | Source | Notes |
| --- | --- | --- |
| <name>[.exe] | Entry + all .rp merged | The name comes from [build] name, defaulting to [module] name, then defaulting to the entry file name |
| README.md | Auto-generated | Must state the application name, version number (from [module] version), platform, and how to start |
| Paths declared by [build] copy | Same paths within the project | Runtime data (databases, uploaded files, etc.) is declared by the project and copied recursively, so that data is not lost after delivery; the build must not wipe existing data; logs and temporary files are not copied. The toolchain does not presuppose directory names such as storage/ or uploads/ |
| migrations/ | Project directory | Copied as-is when present |
| Static assets | public/, assets/, static/, [build] assets | Copied into the delivery directory by directory name; front-end/back-end separated projects should not declare the front-end directory |

The output directory and executable name can be specified via configuration or the command line, in descending priority:

| Item | Command line | rip.toml | Default |
| --- | --- | --- | --- |
| Output directory | -o <dir> | [build] dir | dist/ |
| Executable name | --name <name> | [build] name | [module] name |
| Bundled data | None | [build] copy | Empty (no runtime directory is copied) |

`[build] copy` and `[compile] exclude` are both free-form configuration and are not bound to any directory name. Example:

```toml
[build]
copy = ["storage", "uploads"]   # directories or individual files, relative to the project root
[compile]
exclude = ["storage", "uploads", "data/db"]
```

`dist/` and `build/` do not participate in the compilation unit.

### 0.3 Example (`demo/`) Boundary

`demo/` is a **consumer** of the language and toolchain, not a definer:

1. Examples may only use capabilities already defined in the current specification; anything missing must go through language evolution first, and no special case may be invented in an example.
1. It is forbidden to add keywords, aliases, implicit conversions, or syntactic sugar for examples, or to relax type/ownership checks.
1. It is forbidden to add runtime special cases "effective only for a certain directory / a certain file name" for examples.
1. When an example reveals a defect in the specification, the order is: **fix this specification first → then fix the implementation and tests → finally fix the example**, and all three steps leave a record.
1. Example code must not become the de facto standard for standard library semantics; standard library behavior is governed by Section 11.
1. A demo may freely choose its directory organization and naming, but must not in turn require changes to the project directory structure specification in Section 10 of this specification or in `demo/README.md`.

---
## 1. Language Positioning

| Item | Definition |
| --- | --- |
| Name | Rip |
| Suffix | .rp |
| Tool | rip |
| Package manifest | rip.toml |
| Positioning | Server / Web language |
| Style | Go structure + PHP ease of use + Rust safety |
| Typing | Strongly typed, no inference, fully explicit |
| Memory | Reference counting + cycle collection, Rust ownership semantics, users do not write & / 'a |
| Classes | No class; ary + free functions + structural typing |
| Execution | Interpreted mode (development) + AOT mode (deployment) |
| Rust interop | Supported, called only through Rip syntax |
| Templates / GUI | Not built in; extended through Rust packages |

---

## 2. Lexical Structure

### 2.1 Identifiers

```text
[a-zA-Z_][a-zA-Z0-9_]*
```

Case-sensitive; the `$` prefix is forbidden.

### 2.2 Keywords (Complete Set)

```text
let  mut  const  static  self  func  async  await  return  defer
if  elif  else  for  in  loop  while  match  parallel  break  continue
mod  use  as  pub  type  struct  obj  interface  enum
read  write  share
true  false  null
ok  err  some  none
```

Keywords may not be used as variable names, function names, type names, or module names. The only exception: a member name after `.` may have the same name as a keyword (the standard library already has `http.static`); it is treated as a member name, and this must not be extended to other positions.

`package` and `import` have been removed from the language: they are not keywords, no corresponding declaration exists, and writing them in source only yields a syntax error, with no compatibility path.

### 2.3 Literals

| Type | Spelling | Example |
| --- | --- | --- |
| Integer | Decimal, optional type suffix | 42, 1_000, 42i32, 7u8 |
| Integer (hexadecimal) | Prefix `0x`, optional suffix | 0xFF, 0xFFu8, 0xdead_beefu32 |
| Integer (octal) | Prefix `0o`, optional suffix | 0o755, 0o755u16 |
| Integer (binary) | Prefix `0b`, optional suffix | 0b1010, 0b1010u8 |
| float | With decimal point or exponent, optional suffix `f64` | 3.14, 1e9, 2.0f64 |
| string | `"` or `'` | "hello", 'a' (`'` must contain exactly one Unicode scalar) |
| bool | Keyword | true / false |
| null | Keyword | Available only for option<T> |

**Rules** (consistent with Rust):

- **A prefix indicates only the radix and does not determine the type**; the type is given by the suffix (`u8`/`u16`/`u32`/`u64`/`i8`/`i16`/`i32`/`i64`).
- An integer without a suffix is `int` (= `i64`); a decimal without a suffix is `float` (= `f64`).
- The suffix and the value must match (`256u8` is a compile error); `int2` / `int8` / `int16` are no longer types.
- `_` separators are allowed between digits for readability.

**Quote rules**:

- `"..."` is a string of arbitrary length and supports the escapes `\n` `\t` `\"` `\\`.
- `'...'` **must contain exactly one Unicode scalar**; `''` and `'ab'` are compile errors.
- The single-character type is still `string`; no `char` type is introduced.
- `"""..."""` is a multi-line template: newlines and indentation are preserved as-is, `${expr}` inside the content is evaluated and concatenated; `\${` denotes a literal `${`.

Interpolation is expanded at parse time into ordinary string concatenation (`"a" + x.to_string() + "b"`), with identical behavior under source execution and native compilation.

- Ordinary double-quoted strings do no interpolation and keep their original semantics.

### 2.4 Symbol Responsibilities

| Symbol | Sole Purpose |
| --- | --- |
| : | Name → type |
| :: | `use` path segmentation |
| = | Variable binding / assignment (`let`, assignment, pattern binding in `while let`) |
| => | Match arm: pattern on the left matches → execute on the right ( `match` arms, after the `while let` condition) |
| -> | Return type |
| ; | End of statement |
| . | Dot access / call |
| ? | result error propagation (only inside a result function) |
| # | Attribute introduction (only #[...], see 2.6) |

**There is no** **`:=`, `|>`, or `as` (when used as a type cast). `as`** **is used only for** **`use ... as`** **aliases. `=>` appears only in the two places above; everywhere else use `=`.**

### 2.5 Comments

```rp
// single line
/* multi-line */
```

### 2.6 Attributes (Annotations)

Attributes are **language-level metadata**, not comments:

```rp
#[name]
#[name("string", "string")]
```

Rules:

- They may be written only before a module-level `func` declaration; a function may have multiple attributes, each on its own line.
- Attribute names are identifiers; arguments may only be one or more double-quoted strings.
- Attributes are parsed into the AST at compile time, do not participate in runtime evaluation, and cannot be used as ordinary expressions.
- Attribute names must belong to the set already defined by the standard library; an undefined attribute name is a compile error.
  - HTTP routes: `#[get(...)]`, `#[post(...)]`, `#[put(...)]`, `#[delete(...)]`, `#[patch(...)]`, `#[options(...)]`, `#[head(...)]`. See 11.2.1 for semantics.
  - Background tasks: `#[interval("1s")]`; time units support `ms`, `s`, `m`, `h`.
  - Tests: `#[test]`.
- Route attributes must have exactly one string argument, and the path must start with `/`.
- `#[test]` takes no arguments; the function under test must take no parameters and must have return type `void`. The body uses `dev.assert` assertions and is executed by `rip test` (see 10.7).

---

## 3. Type System

### 3.1 Integers and Floating Point (Rust Semantics)

| Type | Description |
| --- | --- |
| int | Signed 64-bit, equivalent to `i64`, the default integer type |
| float | 64-bit floating point, equivalent to `f64`, the default floating-point type |
| i8 / i16 / i32 / i64 | Fixed-width signed integers |
| u8 / u16 / u32 / u64 | Fixed-width unsigned integers |

**Rules**:

- Radix uses a prefix (`0x` / `0o` / `0b`), type uses a suffix (`42i32`, `7u8`); an integer without a suffix is `int`.
- `int2` / `int8` / `int16` are no longer types, and a literal prefix no longer determines the type.
- Different integer types have **no implicit conversion**, and mixing them is a compile error; operations on the same type are computed at that type's width and overflow panics (no wrapping), with the error reported **at the operation site**—intermediate results are checked as well, rather than only failing at assignment time.
- When initializing from a literal, the suffix type must be able to hold the value (`256u8` is a compile error).

```rp
let a: int = 42;          // int = i64
let b: u8 = 0xFF;         // the prefix is only the radix
let c: u16 = 0o755;
let d: i32 = 42i32;       // explicit suffix
let e: u64 = 1_000_000u64;

let f: int = b.to_int();  // explicit conversion
let g: u8 = a.to_u8();    // truncation
```

**Methods**: `.to_int(base)` (= `.to_i64(base)`) / `.to_i8(base)` / `.to_i16(base)` / `.to_i32(base)` / `.to_i64(base)` / `.to_u8(base)` / `.to_u16(base)` / `.to_u32(base)` / `.to_u64(base)` (`base` is the radix, 2..36, default 10; all integer conversion methods support it) / `.to_float()` / `.to_string()` / `.to_string_base(base)` / `.abs()` / `.min(o)` / `.max(o)` / `.pow(e)` / `.mod(m)` / `.clamp(lo, hi)`.

**Conversion semantics (consistent with Rust `as`)**: narrowing truncates modulo 2ⁿ, with `u*` interpreted as unsigned and `i*` sign-extended from two's complement; float → integer saturates to the target range; integer → float takes the nearest representable value.

**Two spellings (identical for all integer conversion methods)**:

- **The receiver must be a variable**: conversion methods cannot be applied directly to a literal. `11.to_i16()` and `"11".to_int(2)` are both compile errors; bind with `let` first, then convert.
- **Without arguments**: `.to_i8()` / `.to_i16()` / `.to_u16()` … perform only a width conversion and keep the value unchanged (truncation / sign extension per Rust `as`).
- **With arguments**: `.to_i8(16)` / `.to_i16(8)` … first reinterpret the receiver's decimal spelling in `base` (2..36), then perform the width conversion; the default 10 is equivalent to passing no argument.

```rp
let x: i8 = -5;
let a: i16 = x.to_i16();       // -5: width conversion only
let b: u8 = x.to_u8();         // 251: truncated per Rust as

let y: i8 = 55;
let c: i16 = y.to_i16(8);      // 45: width + radix 8
let d: i8 = y.to_i8(16);       // 85: width + radix 16
let e: i16 = y.to_i16(10);     // 55: radix 10 = no argument
let f: i16 = y.to_i16(32);     // 33: "55" in radix 32

let text: string = "ff";
let hex: result<u8, string> = text.to_u8(16);    // ok(255)
let text2: string = "755";
let oct: result<u16, string> = text2.to_u16(8);  // ok(493)
let text3: string = "300";
let bad: result<u8, string> = text3.to_u8();     // err("integer out of range for u8")
```

- String receiver: the text is parsed in the given radix, and out-of-range values return `err`; numeric receiver: after reinterpretation, it is truncated to the target width per Rust `as`; a digit that is invalid in that radix (e.g. `y.to_i8(2)` with y = 55) or a radix out of range (`base must be 2..36`) is a runtime error.

### 3.2 Other Basic Types

| Type | Description |
| --- | --- |
| float | 64-bit floating point (= `f64`) |
| string | UTF-8, immutable |
| bool | Boolean |
| void | Return type only; `()` is its equivalent spelling |

### 3.3 Composite Types

| Type | Syntax | Corresponding Rust | Description |
| --- | --- | --- | --- |
| ary / struct | [name: T, age: T] | struct | Fixed-field named structure; the declaration may use `type` / `struct` / `obj` |
| vec | vec<T> / vec<T1, T2> / vec | Vec<T> | Ordered with implicit numeric keys |
| map | map<K, V> | HashMap<K,V> | Dynamic key-value dictionary |
| option | option<T> | Option<T> | some(T) / none |
| result | result<T, E> | Result<T,E> | ok(T) / err(E) |
| share | share<T> | Arc<T> | Shared across threads; T must be Send + Sync |
| task | task<T> | Task handle | The result of an `async` call, awaited to obtain the value (see 11.5) |
| func | func(A, B) -> C | Function pointer/closure | Function type |
| enum | type x = enum { ... } | Rust enum | Named variants, exhaustive `match` checking (see 3.4.1) |

### 3.4 Named Types

```rp
type user = [name: string, age: int, tags: vec<string>];
struct user = [name: string, age: int, tags: vec<string>];   // equivalent to type
obj user = [name: string, age: int, tags: vec<string>];      // equivalent to type
```

Rules:

- Type declarations have exactly three spellings: `type` / `struct` / `obj`; all three are fully equivalent and interchangeable, and a fourth keyword is not allowed.
- The declaration form is fixed as `keyword name = type`; `struct` / `obj` are merely aliases with semantics identical to `type`.
- Already-defined types may be referenced in nested form.
- Type names are unique within a module (a duplicate definition in the same file is an immediate error).
- Type definitions cannot be modified at runtime.

**Recursive types** may self-reference; in practice they must self-reference indirectly through containers such as `vec` / `option` / `result` / `map` in order to construct finite values:

```rp
type node = [value: int, children: vec<node>];   // legal: indirect self-reference through vec
type tree = [root: node];                        // legal: chained reference
type a = b;                                      // compile error: pure alias cycle, never resolves to a concrete type
type b = a;
```

Rules:

- Alias expansion is detected with an "expansion stack": expansion stops as soon as an alias is already on the stack, so recursive types expand only one level, without exponential blowup or stack overflow.
- A pure alias cycle (a cycle formed only through `type x = y` edges) has no container to break it and is statically reported as `type 'x' is a recursive alias and never resolves`.
- The form in which a field type is directly the type itself (`type bad = [self_ref: bad]`) passes type checking but cannot construct a finite value, being equivalent to always empty; when recursive data is needed, always self-reference indirectly through `vec` / `option` / `map`.
- Two recursive types may be assigned to each other as long as their structures are isomorphic: besides exact equality, `same_type` also performs a structural comparison with a depth limit of 32, so that "different expansion depths" are not misjudged as type mismatches.

### 3.4.1 Enumerations (ADT)

```rp
type status = enum {
    active,
    pending,
    banned(reason: string),
}

type expr = enum {
    lit(value: int),
    neg(inner: expr),
    add(left: expr, right: expr),
}
```

Rules:

- Variants are separated by `,`, and a trailing comma is allowed; a fieldless variant is written with just its name.
- Variant names are unique within the same enum; enum names share the namespace with `type` / `interface`, and a duplicate name is an immediate error.
- Construction: `status.active` (fieldless), `status.banned[reason = "spam"]` (with fields; all fields must be written and their types must match).
- Enums are Move types; use `.clone()` when a second copy is needed.
- Enums may define methods: `func status.blocked(read s: status) -> bool { ... }`, called as `s.blocked()`.
- Enums may reference themselves or other enums recursively, and field types are likewise explicitly type-checked.
- Enum values have no direct field access: reading fields requires `match` (see 9.3), which guarantees fields are taken per variant.
- JSON serialization is `{"variant": "banned", "reason": "spam"}`; a fieldless variant has only `variant`.

### 3.5 No Inference

**An explicit type is mandatory in every position**. The only exceptions are `for item in xs` and bindings in `match` arms, where the type is derived from container/pattern destructuring and does not count as inference.

```rp
// legal
let count: int = 0;
let name: string = "a";

// illegal (compile error)
count = 0;
count := 0;
tags = ["a"];
func f(x) -> int { return x; }
func f(x: int) { return; }
```

---

## 4. Variable Definition

### 4.1 Four Bindings (Single Canonical Form)

```text
let var_name: type = value;
let mut var_name: type = value;
const CONST_NAME: type = value;
static STATIC_NAME: type = value;
```

Rules:

- Exactly one of `let` / `let mut` / `const` / `static` must be written; **there are no bare declarations**.
- **The type must be written**; there is no inference.
- **Statements end with** **`;`**.
- `const` and `static` must be module-level and cannot be defined inside a function.
- `const` is evaluated at compile time; `static` is initialized at runtime and has a fixed address.
- `static mut` requires an `unsafe` block and is **forbidden at the user level**.

```rp
const MAX_SIZE: int = 1024;
static GLOBAL_COUNT: int = 0;

func main() -> void {
    let count: int = 0;
    let mut total: int = 0;
    total = total + 1;
    // count = 1;   // compile error: count is immutable
}
```

### 4.2 Read-Only Variable Rules

- A `let` variable **can only be read**; it cannot be assigned to.
- To "change a variable", you can only create a new variable or use `let mut`.

```rp
let x: int = 1;
let y: int = x + 1;     // use a new variable to carry the result
// x = 2;               // compile error

let mut z: int = 1;
z = z + 1;              // legal
```

### 4.3 Shadowing Is Forbidden

Variable names are unique within the same scope; a duplicate declaration is a compile error.

```rp
let x: int = 1;
let x: int = 2;         // compile error: x is already defined
```

### 4.4 Naming Conventions (Mandatory)

| Category | Convention | Example |
| --- | --- | --- |
| Variables / functions / modules / packages | snake_case | user_name, get_user |
| Constants / statics | SCREAMING_SNAKE_CASE | MAX_SIZE, GLOBAL_COUNT |
| Types (type) | snake_case | user, http_request |

**Forbidden**: Hungarian notation (`iCount`, `strName`, `pUser`) and camelCase variable names (`userName`, `getUser`). Violations emit a `rustc`-style warning, which is escalated to an error under `--deny-warnings`.

---

## 5. The Three Data Structures

### 5.1 `ary` (Fixed-Field Named Structure)

**Type definition** (fields use `:`):

```rp
type address = [city: string, zip: string];
type user = [id: int, name: string, address: address];
```

**Literals** (fields use `=`, types are not repeated):

```rp
let mut u: user = [
    id = 1,
    name = "Zhang San",
    address = [city = "Beijing", zip = "100000"]
];
```

**Access**: dot notation only.

```rp
u.name                  // read
u.address.city          // nested read
u.name = "Li Si";       // write; u must be let mut
```

**Rules**:

- Field names are keys, accessed by dot; `u["name"]` and `u[0]` are forbidden.
- The field set is closed at compile time; at runtime only values can be changed, not fields added.
- To add a field: modify the `type` definition and recompile, or define a new type plus a new variable (copying values with `.clone()` of the old variable).
- Dynamic growth: design the mutable part as a `vec` / `map` field and `push` / `set` into that field.

### 5.2 `vec` (Ordered Sequence with Implicit Numeric Keys)

**Types**: `vec<T>` (homogeneous), `vec<T1, T2>` (heterogeneous, fixed length), `vec` (type defined inline).

**Three legal spellings**:

```rp
// homogeneous declared up front + shorthand
let v1: vec<int> = [1, 2, 3];

// homogeneous declared up front + explicit
let v2: vec<int> = [:int = 1, :int = 2];

// heterogeneous declared up front + shorthand
let v3: vec<string, int> = ["a", 1];

// heterogeneous declared up front + explicit
let v4: vec<string, int> = [:string = 'a', :int = 1];

// type defined inline (each element carries its own type)
let v5: vec = [:int = 1, :string = "a", :bool = true];

// empty vec; the type is determined by the declaration
let v6: vec<int> = [];
```

**Illegal**:

```rp
let bad1: vec = [1, "a"];           // an untyped vec cannot be combined with shorthand
let bad2: vec<int> = [1, "a"];      // homogeneous declaration, second element type mismatch
let bad3: vec<string, int> = [1, "a"];  // shorthand default types do not match the declaration
```

**Access**:

```rp
v.0                 // compile-time constant position
v.at(i)             // runtime position, returns result<T, string>
```

**Methods**:

```rp
v.len() / v.is_empty()
v.push(x) / v.pop() / v.insert(i, x) / v.remove(i) / v.clear()
v.reverse()                            // reverse order, keys renumbered
v.sort(cmp: func(T, T) -> int)         // sort, keys renumbered
v.map(f) / v.filter(f) / v.reduce(f, init) / v.find(f)
v.any(f) / v.all(f) / v.count(f)
v.slice(start, end) / v.concat(other) / v.unique() / v.flatten()
v.join(sep)                            // when elements are string
v.chunk(n) / v.zip(other)
```

**Rules**:

- Keys are always ordered `0,1,2...` and are not exposed.
- A homogeneous `vec<T>` supports `push` / `pop` / `insert` / `remove` / `sort` / `reverse`.
- A heterogeneous `vec<T1, T2>` is fixed-length; structural modification methods are compile errors, and it is read-only.
- Indexing with `v[0]` is forbidden.
- Default element types for the shorthand literal `[1, 2, 3]`: `0` → `int`, `"a"` → `string`, `'a'` → `string`, `1.5` → `float`, `true` → `bool`.

### 5.3 `map` (Dynamic Key-Value Dictionary)

```rp
let mut scores: map<string, int> = [];
scores.set("Zhang San", 90);
scores.get("Zhang San");          // option<int>
scores.has("Zhang San");          // bool
scores.delete("Zhang San");       // option<int>
scores.len();
scores.keys();               // vec<string>
scores.values();             // vec<int>
scores.items();              // vec<[key: string, value: int]>
scores.merge(other);
scores.filter(func(k: K, v: V) -> bool);

let config: map<string, string> = ["name" = "Zhang San", "city" = "Beijing"];
```

**Rules**:

- `K` must be hashable (`string` / `int*` / `bool`).
- `scores["Zhang San"]` is forbidden; use only `.get` / `.set` / `.has` / `.delete`.
- `.get` returns `option<V>`.
- Backed by HashMap; unordered.

### 5.4 Boundaries Between the Three

| Requirement | Use |
| --- | --- |
| Fixed fields known at compile time | ary |
| Ordered indexed sequence | vec |
| Dynamic key dictionary | map |

**No mixing, no implicit conversion between them.**

---

## 6. Copy Types vs Move Types

### 6.1 Classification

| Category | Types | Argument passing/assignment behavior |
| --- | --- | --- |
| Copy | int / i8 / i16 / i32 / i64 / u8 / u16 / u32 / u64 / float / bool | Copied; the original variable remains usable |
| Move | string / ary / vec / map / option / result / share / func | Ownership transfers by default; the original variable becomes invalid |

### 6.2 Examples

```rp
// Copy: assignment/argument passing does not invalidate
let a: int = 1;
let b: int = a;
dev.log(a.to_string());    // a is still usable

// Move: assignment transfers
let s: string = "hello";
let t: string = s;
// dev.log(s);             // compile error: s is no longer valid

let u: user = [id = 1, name = "Zhang San"];
let v: user = u;
// dev.log(u.name);        // compile error: u is no longer valid
```

### 6.3 `.clone()`

**Move types must provide a** **`.clone()`** **method** that returns a copy of the same type. It is used to avoid a move in "define a new variable and carry the value over" scenarios.

```rp
let u: user = [id = 1, name = "Zhang San"];
let u2: user = u.clone();     // u is still usable
```

---

## 7. Functions and Ownership

### 7.1 Function Definition (Single Canonical Form)

```text
func name(params, ...) -> return_type {
    statements;
    return value;
}
```

```rp
func add(a: int, b: int) -> int {
    return a + b;
}

func log(msg: string) -> void {
    // write -> void when there is no return value
}
```

Rules: parameter types must be written; the return type must be written, with `-> void` when there is no return; `fn` / `def` / `function` / arrow functions are not introduced.

### 7.1.1 Package-Internal Visibility `self`

Functions are publicly visible by default. With the `self` modifier, a function can be called only **inside the package that declares it**:

```rp
self func hash_password(raw: string) -> string {
    return raw;   // for internal use within this package only
}
```

Rules:

- Single spelling: `self func name(...)`; `self` occupies the position before `pub` and cannot appear together with `pub`.
- A `self func` can only be called by functions in the same package (the same directory); cross-package calls, passing the function as a value, or passing it as a callback are all compile errors.
- The check happens at compile time and applies consistently across `rip check` / `rip run` / `rip build`.
- The package boundary is determined by the `mod` declaration of the file containing the function; in project mode each directory is a package.
- Functions without `self` keep the current globally visible behavior, and `pub` may be written or omitted.

### 7.2 Parameter Modifiers (`read` / `write`, with `&T` / `&mut T` spellings available)

| Spelling | Underlying Rust | Semantics |
| --- | --- | --- |
| Not written | T | Copy copies / Move transfers |
| read name: T or name: &T | &T | Read-only access; the original variable is retained |
| write name: T or name: &mut T | &mut T | Writable access; the original variable is retained; the argument must be `let mut` |

Rules:

- Only the two keywords `read` / `write` are kept as parameter modifiers; `&T` / `&mut T` are their positional equivalent spellings, producing exactly the same compilation result.
- A single parameter may use only one spelling; mixed forms such as `read u: &user` are an immediate error.
- `&` / `&mut` appear only in parameter type positions; `let x: &T` is not a legal type.
- A standalone `mut` is used only in `let mut` and is **not a parameter modifier** (the `mut` in `&mut T` is part of that spelling).
- Lifetimes are still inferred by the compiler; users do not write `'a`.

```rp
func greet(read u: user) -> string {        // equivalent to u: &user
    return "Hi " + u.name;
}

func rename(write u: user, n: string) -> void {   // equivalent to u: &mut user
    u.name = n;
}
```

```rp
func greet(u: &user) -> string {
    return "Hi " + u.name;
}

func bump(u: &mut user) -> () {
    u.age = u.age + 1;
}

func consume(u: user) -> int {       // default: u is moved in
    return u.id;
}
```

### 7.3 Calls

```rp
let mut u: user = [id = 1, name = "Zhang San", address = [city = "Beijing", zip = "1"]];

let msg: string = greet(u);          // read; u is retained
rename(u, "Li Si");                  // write; u is retained and modified
let id: int = consume(u);             // default; u is moved and becomes invalid afterwards
```

### 7.4 Lifetimes

- Users never write `'a`.
- A returned reference can only come from the **sole input reference**:

```rp
func get_name(read u: user) -> read string {
    return u.name;
}
```

- Returning a reference when there are multiple input references → compile error; the signature must be changed.
- The compiler infers with NLL plus tightening rules; if inference fails, it is an immediate error.

### 7.5 Ownership Rules

| Rule | Description |
| --- | --- |
| Immutable by default | `let` is immutable, `let mut` is mutable |
| Copy/Move | Copy copies, Move transfers |
| Borrowing | Multiple `read` borrows or one `write` borrow at a time; they cannot be mixed |
| No partial moves | Fields of an ary are not moved out individually |
| No shadowing | Variable names are unique within a scope |
| Scope drop | Released automatically on leaving the scope, per Rust rules |
| No raw pointers / no unsafe | Forbidden at the user level |
| Reference counting + cycle collection | Containers are released immediately; cycles where closures and containers reference each other are handled by a periodic cycle collector (sys.gc) |
| Cross-thread | Use only share<T>, equivalent to Arc<T> |

### 7.6 Closures

```rp
let double: func(int) -> int = func(x: int) -> int {
    return x * 2;
};
```

Rules: parameters and return types must be written; captures are handled automatically per borrowing rules; the closure type must be explicit.

### 7.7 Generic Functions

Type parameters are written in `<...>` after the function name and may be referenced by parameter types, return types, and container types:

```rp
func identity<T>(x: T) -> T {
    return x;
}

func size<T>(read items: vec<T>) -> int {
    return items.len();
}

func head<T>(read items: vec<T>) -> option<T> {
    match items.at(0) {
        ok(v) => { return some(v); }
        err(e) => { return none; }
    }
}

let n: int = identity(1);              // T is inferred as int
let s: string = identity<string>("a"); // explicit type argument
```

Rules:

- **Inferred or explicit**: `identity(1)` infers from the argument; `identity<string>("a")` specifies explicitly, and the number must match the declared type parameters.
- **Constraints**: `func total<S: shape>(read items: vec<S>) -> float`; the constraint name must be an interface. Inside the function body, methods of `S` are resolved according to that interface (`item.area()`), and call sites check whether the argument type implements the interface.
- A type parameter that is declared but whose binding cannot be inferred (appearing only in return position) is treated as `any`, with no implicit assignment from `any` to a concrete type.
- **Runtime type erasure**: no monomorphization; a generic function has a single implementation in the interpreter / bytecode / AOT, with no extra runtime overhead.
- A type parameter inside the function body is an opaque type when **there is no constraint**: it can only be passed around as a whole (as an ordinary parameter, container element, return value, or the payload of `some` / `ok`), and methods cannot be called on it nor can it be used in operations with literals; when these operations are needed, add an interface constraint or rewrite the code with a concrete type.
- Generic functions are not overloaded: functions with the same name count as duplicate definitions regardless of whether their type parameters differ; use a different name when needed.
- The AOT artifact of `rip build` also supports generic functions: type parameters participate only in compile-time checking and do not exist at runtime.

### 7.8 Methods and Interfaces

**Methods**: `func type.method(...)`; the first parameter is the receiver and may only be `read` or `write`.

```rp
type user = [id: int, name: string];

pub func user.display(read u: user) -> string {
    return u.name;
}

func user.rename(write u: user, name: string) -> void {
    u.name = name;
}

let mut u: user = [id = 1, name = "Zhang San"];
u.display();          // statically dispatched to user.display(u)
u.rename("Li Si");    // the write receiver requires u to be let mut
```

Rules:

- The full name is `type.method` (e.g. `user.display`) and is globally unique in the function table; a method name is unique within the same type.
- The receiver must be written explicitly (`read u: user` / `write u: user`); `self` / `this` are not introduced.
- A `write` receiver requires the receiver to be a mutable variable or a field chain; a temporary value (`make().rename(...)`) is an immediate error.
- Methods cannot carry annotations, cannot be generic, and cannot use the `self` modifier.
- Methods may be `pub`; type visibility is still decided by `pub type`.
- Enums may likewise carry methods: `func status.blocked(read s: status) -> bool`, called as `s.blocked()`.

**Interfaces**: `type name = interface { ... }`, with method signatures only, and the receiver fixed as the interface itself.

```rp
pub type shape = interface {
    func area() -> float
    func name() -> string
    write func scale(factor: float) -> void
}

type circle = [radius: float];
type square = [side: float];

func circle.area(read c: circle) -> float { return 3.0 * c.radius * c.radius; }
func circle.name(read c: circle) -> string { return "circle"; }
func circle.scale(write c: circle, factor: float) -> void { c.radius = c.radius * factor; }

func describe(read s: shape) -> string {
    return s.name() + " area=" + s.area().to_string();
}

let shapes: vec<shape> = [circle[radius = 1.0], square[side = 3.0]];
for s in shapes {
    dev.log(describe(s));      // dispatched at runtime by class name to the circle / square methods
}
```

Rules:

- **Implicit implementation**: a concrete type that has methods with the required names and signatures is considered to implement the interface; no `impl` declaration is needed.
- Signatures must match exactly: parameter types, return types, and receiver mode (`write` corresponds to `write`).
- Interface values can be placed in a `vec`, used as parameters/return values, or used as `ary` fields; calls dispatch by runtime class name.
- Interface values are constructed with a named constructor (`circle[radius = 1.0]`) or from a variable declared with a concrete type, and the class name is recorded at runtime accordingly.
- Only user-named types (`type x = [...]`) can implement interfaces; built-in types (`string` / `int`, etc.) do not participate.
- Interfaces share the namespace with `type`, and duplicate names are an immediate error; method implementations cannot be written for an interface (`func shape.x` is an error).

### 7.9 defer / Resource Release

`defer call;` executes that call **when the current function exits**, and is used to close connections, release locks, and clean up temporary resources.

```rp
func read_file(path: string) -> result<string, string> {
    let conn: conn = open(path);
    defer conn.close();          // arguments are evaluated in place at the defer site
    let content: string = conn.read()?;
    return ok(content);
}
```

Rules:

- **Only call expressions are accepted**: `defer f(...)` / `defer x.m(...)` / `defer module.f(...)`; `defer 1 + 2;` is a syntax error.
- **Arguments are evaluated immediately at the defer statement**, and only the call itself is executed when the function exits (Go semantics). Hence `defer f(x)` captures the value at that moment, and later changes to `x` have no effect.
- **Last in, first out**: multiple `defer`s in the same function execute in reverse order of registration.
- **All exit paths execute them**: normal return, `return value`, `?` propagation, `return err(...)`, and runtime errors all trigger the current frame's `defer`s.
- **Isolated by function frame**: a `defer` belongs to the function frame that registered it; when an inner function returns, only the items registered in that inner frame execute.
- Return values are discarded directly: the value returned by a `defer` call does not participate in any expression.
- `defer` can be used only inside a function body: writing `defer` at the top level or inside a `parallel` branch is a compile error.
- When the `defer` itself errors, that error overrides the function's original exit result.

```rp
func demo() -> void {
    defer print("first");        // executes last
    defer print("second");       // executes first
    print("body");
}
// output: body / second / first
```

---

## 8. Operators

### 8.1 Arithmetic (Same Type)

```text
+  -  *  /  %
```

Rules: operands must be of the same type and the result has the same type; direct operations between different integer types are forbidden and must be converted with `.to_intX()`; division by zero panics.

### 8.2 Comparison (Same Type, Returns `bool`)

```text
==  !=  <  >  <=  >=
```

### 8.3 Logic (`bool`)

```text
&&  ||  !
```

### 8.4 String Concatenation

```rp
"a" + "b"       // "ab"
```

Only `string + string`; no implicit conversion.

### 8.5 Type Conversion

**Single canonical form: target type method**:

```rp
let a: u8 = 0b1010;
let b: int = a.to_int();

let s: string = "42";
let n: result<int, string> = s.to_int();
```

**Using** **`as`** **for type conversion is forbidden**.

---

## 9. Control Flow

### 9.1 Conditionals

```text
if condition { ... } else if condition { ... } else { ... }
if condition { ... } elif condition { ... } else { ... }
```

`elif` and `else if` are fully equivalent; both spellings are supported and may be mixed. For single-line branches, the more compact `elif` is preferred throughout.

The condition must be `bool`; there is no truthy/falsy, and parentheses may be omitted.

```rp
if score >= 90 {
    dev.log("A");
} elif score >= 80 {
    dev.log("B");
} else if score >= 60 {
    dev.log("C");
} else {
    dev.log("D");
}
```

### 9.2 Loops (Four Forms, Same Rules as Rust)

**Form 1: `loop`** — an unconditional loop that can exit only via `break`.

```rp
let mut count: int = 0;
loop {
    if count >= 10 { break; }
    count = count + 1;
}
```

**Form 2: `while`** — the condition is `bool`, checked before each iteration.

```rp
let mut count: int = 0;
while count < 10 {
    count = count + 1;
}
```

**Form 3: `while let`** — each iteration evaluates the expression and matches the pattern; the body runs only if the match succeeds, and the loop ends otherwise. `=` binds the pattern and `=>` introduces the loop body.

```rp
while let some(value) = cache.get("key") => {
    dev.log(value);
    cache.delete("key");
}
```

**Form 4: `for ... in`** — iterates a `vec` (elements) or a `map` (keys).

```rp
for item in tags {
    // the item type is destructured from the tags element type
}

for key in scores.keys() {
    // key: string
}
```

Rules:

- `break` / `continue` affect only the innermost loop; `continue` returns to the condition check (`for-in` goes to the next iteration).
- Loop conditions must be `bool`; there is no truthy / falsy.
- There is no `do-while`, and no C-style three-part `for`.
- The loop body must use braces.

### 9.3 Pattern Matching

```text
match expr {
    pattern1 => { statements }
    pattern2 => { statements }
}
```

**`match` arms use `=>`; `=` is used only for binding and assignment.** This is one of the only two uses of `=>` (the other is `while let`).

```rp
match result {
    ok(v) => { print(v); }
    err(e) => { print("error: " + e); }
}

match opt {
    some(x) => { print(x); }
    none => { print("empty"); }
}

match user {
    [id: int, name: string, address: _] => { print(name); }
}

match state {
    status.active => { print("ok"); }
    status.pending => { print("wait"); }
    status.banned[reason: string] => { print(reason); }
}
```

Rules: `match` must be exhaustive and is checked by the compiler; it is used for `option` / `result` / `ary` destructuring and enum variants; `while let` does not require exhaustiveness and ends the loop when the match fails.

- Enum patterns are written as `enum_name.variant`, and a variant with fields binds them with `[field: type, ...]` (`_` means no binding).
- Exhaustiveness checking is by variant name: a missing variant reports `match on enum 'x' must cover variant 'y'`.
- The enum name in a pattern must be consistent with the subject type, field names must belong to that variant, and field type annotations must match the declaration.
- Enum values do not support direct field access (`s.reason` is an error); the value must be extracted through `match`.
- Enum patterns can likewise be used in `while let`: a variant mismatch ends the loop, and exhaustiveness is not required.

### 9.4 Error Propagation

```rp
func read_config(path: string) -> result<string, string> {
    let content: string = read_file(path)?;
    return ok(content);
}
```

`?` may be used only inside a function returning `result`; for `err(e)` it returns `err(e)` immediately.

---

## 10. Modules and Imports

### 10.1 Module Declaration (`mod`)

The first line of every `.rp` file:

```rp
mod user_api;
```

Rules:

- The module name must equal the name of the directory containing the file, exactly, using a single `snake_case` segment; dot-separated names are not written.
- All `.rp` files in the same directory must declare the same module name.
- A subdirectory is an independent module, and its module name likewise equals that subdirectory's name.
- A missing `mod` declaration, a module name inconsistent with the directory name, or inconsistent module names within one directory all cause an immediate compile failure.
- `mod foo;` does not import submodule files: project `.rp` files are recursively collected and merged by directory by the project loader, and `mod` only declares the module identity of the current file.

### 10.2 Module Imports (`use`)

Rust-style path segmentation is used, but calls keep Rip's dot form:

```rp
use std::http;
use std::json;
use std::sys as sys;
use github::com::user::pkg as pkg;
use rust::serde_json as json;
```

Rules:

- Paths are segmented with `::` and are unquoted; a path has at least one segment.
- The name after `as` is an alias and must be a legal identifier; `as` is the only way to rename.
- Without an alias, the last segment of the path is used as the alias by default (`std::http` → `http`).
- Only modules are imported; item-by-item imports are not supported (`use std::http::serve;` is an error), consistent with the "single canonical form" principle.
- The prefix `std` is the standard library, `rust` is a Rust crate, and everything else is a third-party Rip package.
- After import, calls are written `alias.function(...)`; the `::` call spelling is not introduced; method calls on values remain `value.method()`.
- Third-party packages live under `packages/` in the project root (`package/` is also accepted; when both exist, `packages/` takes precedence); the `use` path equals the path relative to the package directory with `/` replaced by `::`: `packages/utils/http.rp` → `use utils::http;`; package names may have multiple segments, and a `.rpa` precompiled package is equivalent to a `.rp` source package.
- Third-party packages are merged with the project source into the same compilation unit at compile time, and `pub` functions in a package are called by ordinary function names (`http.client(...)` is resolved by the alias to a global function of the same name); functions, types, and globals with the same name must be globally unique after merging, and conflicts are reported before type checking along with both declaration sites.
- Source files inside a package are exempt from the "module name must equal directory name" check; package identity is determined by the path under `packages/`.
- `import` is not a keyword: writing `import "std/http";` only yields a syntax error.

### 10.3 Visibility

Private within the module by default, public with `pub`:

```rp
pub func serve(port: int, handler: func(http.request) -> http.response) -> void { ... }
pub type user = [id: int, name: string];
```

Rules: there are only two levels, "private by default / public with `pub`"; `private` / `protected` are not introduced.

### 10.4 Package Manifest `rip.toml`

```toml
[module]
name = "myapp"
version = "0.1.0"
entry = "main.rp"

[dependencies]
"github.com/user/rp-pkg" = "1.2.0"
"std/http" = "builtin"

[rust-dependencies]
serde_json = "1.0"

[compile]
exclude = ["storage", "data/db"]   # directories excluded from compilation (directory names or relative paths)

[build]
layout = "typed"     # flat (default, everything at the root) | typed (bin / lib / data / share)
target = "linux"     # defaults to the current platform; must match the host platform only
copy = ["storage", "uploads"]
assets = ["public"]

[runtime]
log = "english"        # language of the runtime's own messages: english (default) | chinese

[server]
workers = 8                   # number of HTTP worker threads; 0 or omitted means CPU cores
max_connections = 1024        # connection queue limit; beyond it returns 503
max_body_bytes = 8388608      # request body limit; beyond it returns 413
max_header_bytes = 65536      # total request header byte limit
max_headers = 100             # header count limit
read_timeout_ms = 30000
write_timeout_ms = 30000
keep_alive_ms = 5000          # keep-alive idle timeout
max_requests = 1000           # maximum requests per connection
health_path = "/healthz"      # empty disables it
ready_path = "/readyz"
metrics_path = ""             # when non-empty, exposes Prometheus metrics, e.g. "/metrics"

[cluster]
redis = "redis://127.0.0.1:6379/0"   # shared cluster backend; omit the whole section if not needed
task_lock = true                     # scheduled tasks elect a leader via a Redis lease
```

`[build]` semantics:

- `layout = "flat"` (default): the executable, data directory, static assets, and README are all at the root of the delivery directory, consistent with earlier versions.
- `layout = "typed"`: `bin/` holds the executable, `data/` holds the data declared by `copy` and `migrations/`, `share/` holds static assets, and `lib/packages/` holds copies of third-party packages (packages are already embedded; `lib/` merely makes them easy to inspect and replace after delivery).
- When a project has assets, migrations, or runtime data but is still `flat`, the build prints a single-line `hint` suggesting `typed`; the default is unchanged so that startup scripts of existing projects do not break after upgrading.
- `target`: the artifact format follows the target platform's native standard (Windows `.exe`; Linux/Unix/macOS extensionless ELF/Mach-O). The current platform is built by default; setting `[build] target = "<triple>"` or the command line `--target <triple>` enables cross-compilation. When the target triple equals the host, the built-in compile component is invoked directly (fast path); otherwise cargo is used with a temporary project linking `rp-core`, which requires the target standard library, linker, and C dependency toolchain (see 10.13). `RIP_TARGET_LINKER` can specify a cross-linker; a missing toolchain produces an error printing the required triple, and never produces an unusable file.
- `copy`: runtime data delivered with the package; the toolchain does not presuppose directory names.

After a successful `rip build`, the delivery directory is archived into a sibling `<dir>.zip` (unpacking yields the complete `dist/`) to make distribution easy; use `--no-archive` to disable this when the data directory is very large.

### 10.5 Third-Party Packages and Precompiled Packages `.rpa`

Project root layout convention:

```text
myapp/
├── main.rp
├── rip.toml
└── packages/
    ├── utils/http.rp          # source package: path utils/http
    ├── utils/strings.rp
    └── vendor/logger.rpa      # precompiled package: path vendor/logger
```

- They are introduced exactly like source packages: `use utils::http;`, `use vendor::logger as logger;`.
- Source packages and precompiled packages may be mixed; what actually participates in compilation is all `.rp` and `.rpa` files under `packages/`.
  - Directory input: `.rp` files in the directory are merged recursively, and the artifact is `<dir>.rpa` in the same directory;
  - File input: the artifact is a `.rpa` file with the same name;
  - The artifact header records `rip-package 1`, `rip-version`, and the package name; when `rip build` encounters a version mismatch it reports an error and refuses to load.
- `packages/`, like `tests/` and `dist/`, is not collected as project source and is handled only by the package loading logic.
- Packaging and distribution: packages are embedded into the executable by default (the same for `flat` and `typed`); the `typed` layout additionally copies them into `lib/packages/`. To distribute source packages separately, simply copy the `packages/` directory under the project root.

Project mode (multi-file compilation unit):

- A directory containing `rip.toml` is treated as a project; `rip run` / `rip dev` / `rip build` / `rip check` all operate on the project as a unit.
- The compilation unit = entry + all `.rp` files recursively collected within the project; `tests/`, `build/`, `dist/`, `target/`, `node_modules/`, and directories beginning with a dot do not participate in compilation; data directories and the like are declared via `[compile] exclude` (directory names or paths relative to the project root, such as `storage`, `data/db`), and the toolchain does not build in special cases like `storage/` or `uploads/`.
- Runtime data directory names (databases, user uploads) are decided entirely by the code: the first argument of `http.save_file` and the connection path of `std/db` are ordinary strings, so whichever directory is written is the one used; if such a directory should be skipped at compile time, add it to `[compile] exclude`.
- `[compile] exclude` and `[build] copy` are independent: the former affects only source collection, the latter only packaging copies.
- `rip test <project dir>` merges `packages/` and then runs all `#[test]` functions together in project mode; when there is no `#[test]`, `main` is not run (`main` may be a long-running service). When the directory is not a project, each `.rp` file is compiled and tested separately.
- The entry is taken from `[module] entry`; when unset, `<dir>.rp`, `main.rp`, and `src/main.rp` are tried in order. The entry must contain `func main() -> void`.
- Each file still declares `mod <containing directory name>;` on its own, so the same directory is the same module; cross-file calls are written directly by function name, with no `use` needed for local files (`use` is only for `std::*`, `rust::*`, and external Rip packages).
- Function names, type names, and constant names are unique within the same compilation unit; duplicates cause an immediate compile failure.
- Diagnostics must map back to the original file and line: `relative/path:line: error: ...`.
- Without `rip.toml`, the directory is treated as a single file, with behavior identical to single-file mode.
- `[runtime] log` controls the language of the **runtime's own messages**: `"english"` (default) or `"chinese"`, with any other value treated as the default. It affects only runtime errors, HTTP and WebSocket startup messages, background task and scheduled task logs, and `std/log` level labels; it **does not affect** the `print` output of the Rip program itself and does not change `result` error values returned to the program. Compile-time diagnostics (lexical, syntactic, type, ownership checks) uniformly use English. `rip build` writes this configuration into the artifact, so no `rip.toml` is needed after delivery.

### 10.6 Rust Package Interop (Single Approach)

**Goal**: a Rust package is an ordinary package in Rip, called entirely with Rip syntax, **exposing no Rust-specific syntax** (no `&`, no `'a`, no `unsafe`, no `Box`, no `impl`).

**Import and call**:

```rp
use rust::serde_json as json;

let s: result<string, string> = json.to_string(user);
let v: result<any, string> = json.from_str(s);
```

**Type mapping (fixed, not extensible)**:

| Rust | Rip |
| --- | --- |
| i64 | int |
| i8 / i16 / i32 / i64 | i8 / i16 / i32 / i64 |
| u8 / u16 / u32 / u64 | u8 / u16 / u32 / u64 |
| f64 / f32 | float |
| String / &str | string |
| bool | bool |
| Vec<T> | vec<T> |
| HashMap<K, V> | map<K, V> |
| Option<T> | option<T> |
| Result<T, E> | result<T, E> |
| () | void |
| struct | ary |

In the table, `ary` is Rip's structure type; the declaration keyword may be `type` / `struct` / `obj`, and the three are strict aliases of one another (see 3.4). The mapping table is fixed; package authors may not extend it or add aliases.

**Rip declaration file**: a Rust package author must provide a `.rp.decl` interface declaration describing the Rip signatures of exported functions:

```rp
// serde_json.rp.decl
pub func to_string(v: any) -> result<string, string>
pub func from_str(s: string) -> result<any, string>
```

**Safety boundary**:

- `unsafe` is isolated inside the Rust package and does not leak to the user level.
- The references `&T` / `&mut T` are not exposed to the user level; values are always returned, or `own` semantics are used.
- Panics must be caught by the package and converted to `result`; they must not penetrate into the Rip runtime.
- Rust packages are compiled and statically linked into `rip` **at toolchain release time**; the user's machine does not run `cargo` and does not need a Rust environment.
- `rip add rust:xxx` only records intent; whether a Rust package can be used depends on whether the current `rip` already includes it. The current version includes `rust/serde_json`; adding a new crate requires including it when releasing `rip` (see 10.11).

**Constraints**:

- `use rust::xxx;` is the only way to import.
- No second mechanism such as `extern` / `ffi` / `cgo` is introduced.
- The type mapping is fixed and cannot be extended by package authors.
- **Rust packages may cover any domain** (including GUI, audio and video, database drivers, etc.); Rip does not restrict the type of package, only the calling convention. This version of the standard library provides no GUI wrapper, so GUI needs are met through `use rust::xxx;`.
- To add Rust capabilities without republishing `rip`, use the native packages of 10.12 (dynamic library + `.rp.decl`); see that section for the trade-off between the built-in approach and new crates.

### 10.7 Commands

```text
rip run main.rp                    # one-shot execution (HTTP listens on 127.0.0.1 by default)
rip run main.rp --host 0.0.0.0 --port 8080   # serve externally
rip run main.rp --cert server.pem --key server.key   # HTTPS (PEM certificate chain + private key)
rip dev main.rp --port 8080        # development hot reload
rip repl                           # REPL
rip build main.rp                 # compile to a native binary and output to dist/ (program + assets + README)
rip build main.rp -o release      # output to a specified directory
rip build main.rp --no-archive    # do not create a sibling dist.zip
rip build main.rp --native        # experimental native compilation (scalar int subset)
rip build main.rp --target x86_64-unknown-linux-gnu   # cross-compile (requires the target toolchain)
rip profile main.rp               # function-level timing report (--json / --top N)
rip dap                           # DAP debug adapter (stdio; --port N uses TCP)
rip pack packages/utils/http      # compile a third-party package into http.rpa
rip lock                          # generate/update rip.lock
rip lock --check                  # validate only, no writes (for CI)
rip vendor                        # fetch dependencies into packages/ and generate rip.lock
rip add github.com/user/rp-pkg     # add a Rip dependency
rip add rust:serde_json            # reference a built-in Rust package (interop; no local Rust environment needed)
rip test                           # run all #[test]
rip test --coverage --coverage-min 80   # statement coverage report (fails below the threshold)
rip bench                          # run all #[bench] and print timing statistics
rip new shop --template http       # create a project skeleton (http / cli / empty)
rip doctor                         # environment / project / dependency / AOT toolchain self-check
rip migrate up                     # apply unapplied migrations under migrations/ (version table rip_schema_migrations)
rip migrate status                 # show applied / unapplied migrations
rip migrate down --to 1            # roll back to version 1 (that version is retained)
rip check main.rp --json           # emit structured diagnostics (CI gate; JSON is emitted on failure too)
rip test examples/s3_demo --filter signature   # run only tests whose name contains signature
rip check main.rp                  # static check
rip fmt main.rp                    # basic formatting
```

Common options:

| Option | Effect |
| --- | --- |
| --offline | Use only packages already available locally; requires `rip.lock` to exist and validate |
| --deny-warnings | Treat warnings as errors (for CI) |

`rip test` rules:

- Test functions are written as `#[test] func test_xxx() -> void { dev.assert(...); }` and take no parameters.
- In project mode, the project source and `packages/` are merged and then run together; for non-project directories, files are collected one by one, and **directory scanning does not execute `main`** (`main` may be a long-running service)—only compilation checks are performed and `#[test]` functions are run; `main` is executed only when a single file is passed explicitly and there are no `#[test]` functions.
- Output is `test <file>.<function> ... ok` / `FAIL`, ending with `N passed, M failed`.
- `--filter <name>` runs only tests whose function name contains that substring.
- Isolation: each `#[test]` uses a fresh runtime, and module-level `const` / `static` and runtime resources (database connections, network handles, timers) are not shared; `--no-isolate` switches to sharing one runtime per file (faster, but state leaks between tests).
- Concurrency: by default tests run in parallel according to CPU cores (each thread builds its own runtime); `--jobs N` sets the thread count, and `--jobs 1` is serial. Results are printed by file and declaration order, independent of execution order. `--no-isolate` can only run serially.
- Timeout: 30 seconds per case by default; `--timeout <ms>` adjusts it and `0` disables it. Timeouts are interrupted at statement / loop iteration / builtin call checkpoints; a single blocking system call already in progress cannot be interrupted midway. When tests use shared external resources (a fixed port, the same database file), use `--jobs 1` to avoid interference.

### 10.8 Dependency Locking and Static Checking

**`rip.lock` (dependency locking)**:

- `rip lock` scans all `.rp` / `.rpa` files under the project's `packages/` (or `package/`), recording for each package its package name, version, source path, SHA-256, and direct dependencies, then writes them to `rip.lock` in the project root.
- `rip run` / `rip dev` / `rip build` / `rip check` / `rip test` validate automatically when `rip.lock` exists: modified package contents, a missing package, or a newly added package that is not recorded all produce an error suggesting that `rip lock` be run again.
- Without `rip.lock`, behavior is the same as before (no validation); in `--offline` mode, `rip.lock` **must** exist, ensuring that offline environments never obtain unlocked dependencies.
- `[registry] source` records the private package registry address (written to the `registry` field of `rip.lock`); `[registry] offline = true` is equivalent to always passing `--offline`.

```toml
[registry]
source = "https://rip.example.com"
offline = false
```

**Static checking (`rip check` / compile-time warnings)**:

- Unused variables: a local variable declared with `let` is never read; names beginning with `_` are not checked.
- Unused imports: a `use` alias is used neither for calls nor for the `pkg.type` type; warning only. Third-party packages (under `packages/`) are merged with source at compile time and calls do not go through aliases, so they do not participate in this determination.
- Unused private functions: a `self func` is never called (functions with route/task/test annotations do not count).
- Cycles between packages: when two or more packages under `packages/` `use` each other, the output is `warning: package cycle: a -> b -> a`.
- All of the above are warnings and can be escalated to a failure with `--deny-warnings`.

**Formatting (`rip fmt`)**:

- Uniform 4-space indentation: lines are rearranged by `{}` / `[]` / `()` nesting, and a closing symbol at the start of a line returns to its corresponding level.
- Trailing whitespace is removed from each line, consecutive blank lines are compressed to one, and the file ends with a single newline.
- Adjacent `use ...;` blocks are sorted by path.
- Triple-quoted template strings and the inside of `/* */` comments are left as-is and not rearranged.
- Files or directories (recursive) are supported; `--check` only checks and uses exit code 1 to signal "formatting needed", while `--stdout` prints the result without writing it back.

### 10.9 Complete Cross-Platform Build Example

**Artifact form comparison**:

| Form | Command | Build dependency | Artifact contents | Use |
| --- | --- | --- | --- | --- |
| Source execution | rip run main.rp | rip | Interpreter + source | Development, hot updates |
| Subset native | rip build main.rp --native | rip (built-in compile component) | Scalar int subset code only | Compute-intensive validation |
| Full AOT (fallback) | rip build main.rp | rip + compatible runtime library | Whole-language native code + interpreter object code | Compatibility with old toolchains |
| Full AOT (clean, default) | rip build main.rp | rip + clean runtime library | Whole-language native code, no interpreter/VM, no source | Production delivery |

> The `rip` delivered to users already contains `toolchain/` (the built-in compile component) and `lib/rip/` (the AOT runtime library), so all the commands in the table above depend only on `rip` itself, with no separately installed compilation tools. See 10.11 for the development workflow of building the toolchain from source.

**1) Prepare the toolchain (identical on all three platforms)**

```bash
cd /path/to/rip
cargo build --release                                       # target/release/rip[.exe]
cargo build --release -p rp-core --target-dir target/aot     # clean host: interp feature disabled
```

**2) Build a clean AOT**

Windows (PowerShell):

```powershell
$env:RIP_AOT_DEPS = "target/aot/release/deps"
.\target\release\rip.exe build main.rp --aot
.\dist\main.exe --port 8080
```

Linux / macOS:

```bash
export RIP_AOT_DEPS=target/aot/release/deps
./target/release/rip build main.rp --aot
./dist/main --port 8080
```

- When `RIP_AOT_DEPS` is not set, `librp_core-*.rlib` is looked up in `deps/` next to `rip`; if it is not found, it falls back to `librp_runtime-*` and prints `note: ... interpreter included`.
- `RIP_AOT_DEPS` is an ordinary path; on Windows it may be written as `target/aot/release/deps`, with no need for an absolute path.

**3) Artifact verification**

```bash
# common to all three platforms: a clean AOT should not contain the VM or the language front end
grep -a "rp-runtime/src/vm.rs" dist/main   # no output (on Windows: rp-runtime\src\vm.rs)
grep -a "rp_parser" dist/main              # no output

# Linux
file dist/main
ldd dist/main

# macOS
file dist/main
otool -L dist/main

# Windows
dumpbin /headers dist\main.exe | findstr PE32
```

**4) Platform differences**

| Platform | Linker | Extra dependencies | Notes |
| --- | --- | --- | --- |
| Windows (MSVC) | System link.exe | None | The default rustup toolchain is sufficient |
| Windows (MinGW) | rust-lld | None | Already configured in the repository's .cargo/config.toml; avoids the GNU ld path issue with spaces |
| Linux | System cc | pkg-config, libssl-dev | tiberius uses native-tls |
| macOS | System clang | Xcode Command Line Tools | — |

- The host target (matching `host:` from `rustc -vV`) uses the local fast path; other triples use a cargo cross build: a temporary project path-depends on `crates/rp-core`, produces output in `<repo>/target/<triple>/release/`, and copies it to the delivery directory.
- Cross-compilation requires: `rustup target add <triple>`, the target linker (`.cargo/config.toml` or `RIP_TARGET_LINKER`), and a cross toolchain for C dependencies such as sqlite / openssl.
- The source tree location is resolved in the order `RIP_AOT_SOURCE` > the repository containing the executable > the repository root recorded at compile time; installation builds require `RIP_AOT_SOURCE`.
- `--native` subset compilation does not support cross-compilation and reports an error directly.

**5) Troubleshooting link errors**

`rip build --aot` calls the built-in compile component directly, bypassing the package manager. Native library search paths, `-lstatic=` / `-framework` flags, and link arguments declared by build scripts are collected automatically from `lib/rip/build/*/output` (on a development machine: `target/*/release/build/*/output`); for troubleshooting, setting `RIP_AOT_DEBUG=1` prints the actual compiler commands executed.

| Error | Common cause | Resolution |
| --- | --- | --- |
| cannot find -lssl / -lcrypto | OpenSSL development packages not installed on Linux | apt install pkg-config libssl-dev |
| ld: framework not found ... | Missing Command Line Tools / SDK on macOS | xcode-select --install |
| linker 'link.exe' not found | Missing MSVC build tools on Windows | Install VS Build Tools, or switch to the MinGW toolchain |
| cannot find -lsqlite3 | RIP_AOT_DEPS points to the wrong directory; build output missing | Remove RIP_AOT_DEPS and rerun `rip build` (the runtime library is built automatically) |
| unable to find library -lwinapi_* | The GNU ld path of MinGW contains spaces | Keep the rust-lld setting in the repository's .cargo/config.toml |
| Writing the artifact: Permission denied | The old binary is still running | Stop the old process before building |

**6) Delivery directory**

`rip build` delivery rules: `README.md`, the data directories declared by `[build] copy`, `migrations/`, `public/`/`assets/`/`static/`, and `lib/packages/` under the `typed` layout. Copying the whole directory is enough to run it; after the build finishes, a sibling `<dir>.zip` is also generated, which `--no-archive` disables.

Runtime errors from both `rip run` and `rip build` carry a full call stack (of the form `at inner (line 3)`): AOT-generated functions register a frame at entry and truncate by depth at exit, consistent with the interpreter's `call_decl` behavior. See 10.13 for post-deployment parameter tuning.

**7) Three-platform CI**

See [`.github/workflows/ci.yml`](../.github/workflows/ci.yml) in the repository for the complete configuration; the core steps:

```yaml
strategy:
  matrix:
    include:
      - { os: windows-latest, ext: .exe }
      - { os: ubuntu-latest,  ext: "" }
      - { os: macos-latest,   ext: "" }
steps:
  - uses: actions/checkout@v4
  - uses: dtolnay/rust-toolchain@stable
  - run: cargo build --release
  - run: cargo build --release -p rp-core --target-dir target/aot
  - env: { RIP_AOT_DEPS: target/aot/release/deps }
    shell: bash
    run: |
      ./target/release/rip${{ matrix.ext }} build examples/hello.rp --aot
      ./examples/dist/hello${{ matrix.ext }} | grep -q "Hello, Rip!"
      RIP_AOT_DEBUG=1 ./target/release/rip${{ matrix.ext }} build examples/hello.rp --aot 2>&1 \
        | grep -q "host   = rp_core"
      test -z "$(ls examples/dist/*.rs 2>/dev/null)"
```

CI also asserts that the AOT artifact contains no `mod ...` source, no `rp_parser` / `rp_lexer`, and no `rp-runtime/src/vm.rs`, and uploads the artifacts of all three platforms.

### 10.10 Complete Package Management Example

Below, a pure Rip package is built from scratch, covering "write → reference → test → precompiled distribution → lock → offline".

**1) Project structure**

```text
slugdemo/
├── main.rp
├── rip.toml
└── packages/
    └── text/
        └── slug.rp        # package path = text/slug
```

`rip.toml`:

```toml
[module]
name = "slugdemo"
version = "0.1.0"
entry = "main.rp"

[build]
copy = ["data"]        # copied only if data/ exists; otherwise a warning is printed and this line can be removed
```

`packages/text/slug.rp`:

```rp
mod text;

use std::dev;

pub func slug(input: string) -> string {
    let lowered: string = input.to_lower();
    return lowered.replace_all(" ", "-");
}

#[test]
func slug_lowercases_and_dashes() -> void {
    dev.assert(slug("Hello Rip World") == "hello-rip-world");
}
```

`main.rp`:

```rp
mod slugdemo;

use std::dev;
use text::slug;

func main() -> void {
    dev.log(slug("Hello Rip World"));
}
```

**2) Referencing and testing**

```bash
rip run main.rp          # [log] hello-rip-world
rip test .               # 1 passed, 0 failed
```

Key points:

- The path in `use text::slug;` is the relative path under `packages/` (without the extension).
- Third-party packages and project source are merged at compile time, and `pub func slug` is called directly by function name; `text.slug(...)` is not needed.
- Function names are globally unique within the same compilation unit, so two packages cannot export functions with the same name.

**3) Precompiled distribution `.rpa`**

```bash
rip pack packages/text                    # artifact: packages/text/text.rpa
cp packages/text/text.rpa ../other-app/packages/text.rpa
rm -rf packages/text                      # do not keep both .rp and .rpa in the same project
```

The consumer's directory:

```text
other-app/
├── main.rp                 # use text::slug;
├── rip.toml
└── packages/
    └── text.rpa            # path = text
```

- `.rpa` is a source archive with a `rip-package 1` header and `rip-version`, equivalent to `.rp`, and the two may be mixed.
- **Do not keep both forms of the same package**: `packages/text/slug.rp` together with `packages/text.rpa` defines `slug` twice and reports `function 'slug' is already defined`.

**4) Dependency locking and offline mode**

```bash
rip lock            # generate/update rip.lock
rip lock --check    # validate only (for CI)
rip run main.rp --offline
```

`rip.lock` example:

```toml
# generated by 'rip lock'; do not edit
version = 1

[[package]]
name = "text"
version = ""
source = "packages/text.rpa"
sha256 = "e6bf4a44ea253cf6e8b1edb948fa7a4a80a71a62d2a59dfe49fc9d149b850f67"
deps = []
```

- With `rip.lock` present, modified package contents, a missing package, or a newly added unrecorded package all produce an error suggesting that `rip lock` be run again.
- `--offline` requires `rip.lock` to exist and validate; `[registry] offline = true` is equivalent to always being offline.

**5) `[dependencies]` and `[registry]`**

```toml
[dependencies]
"text" = "^0.1.0"

[registry]
source = "https://rip.example.com"
offline = false
advisories = "https://rip.example.com/v1/advisories.json"   # optional; defaults to <source>/v1/advisories.json
```

- Package sources support local directories, `file://`, and `https://`. **Online sources are forced to use https**: only `localhost` / `127.0.0.1` / `::1` may use `http://` (for local development); any other `http://` source is rejected with an error to prevent man-in-the-middle replacement of package contents. When an **online package source** is configured, `rip lock` / `rip vendor` / `rip run` resolve constraints from `[dependencies]`, solve versions, download and verify `sha256`, and unpack into `packages/`; **downloads from remote sources must carry an index `sha256`, and a missing digest means the download is refused** (local directory sources are exempt). Offline mode (`--offline` or `[registry] offline = true`) uses only existing local packages and requires `rip.lock` to validate.
- **Default public registry**: when `[registry] source` is not written, `https://registry.rip-lang.dev` is used, so `rip search` / `rip info` / `rip add` / `rip lock` work out of the box. Priority: `--registry` > `[registry] source` > the environment variable `RIP_REGISTRY` > the public registry; when the network is unavailable, the error message includes the source currently in use and how to change it.
- `rip pub` does **not** fall back to the public registry: the publish target must be specified explicitly via `--registry`, `[registry] source`, or `RIP_REGISTRY`, to avoid accidental publishing.
- When `[dependencies]` are all vendored into `packages/` and satisfy their constraints, `ensure` does not go to the network (`rip run` / `rip build` / `rip lock` are never blocked by the registry); constraints are solved and packages fetched only when a package is missing or a version does not satisfy them.
- **Version solving**: bounded depth-first search with backtracking. A single version is chosen per package; candidates are tried in descending version order, falling back to earlier candidates on dependency conflicts. When there is no solution, the source of each constraint is reported (`rip.toml` or the specific version of an upstream package).
- **Registry index**: `v1/index/<name>` holds all versions of a single package; `v1/index.json` is the index directory for online lookup; archives live at `v1/packages/<name>/<version>.zip`, and the index records the `sha256`. When `rip pub` writes to a local registry, it rebuilds `v1/index.json` automatically.
- Versions in `[dependencies]` are `semver` constraints (`^0.1.0`, `~1.2`, `1.2.*`, `*`, etc.); `rip add <package>[@constraint]` writes the manifest and then fetches immediately (equivalent to running `rip lock` afterwards).
- `[registry] source` is written to the `registry` field of `rip.lock` to make the origin auditable.
- `rip search <keyword>` and `rip info <package>` inspect registry contents (`--registry` temporarily overrides the source, `--json` emits structured results).

**Registry directory layout**:

```text
registry/
└── v1/
    ├── index.json
    ├── advisories.json
    ├── index/
    │   └── acme.text          # one version per line: version, sha256, dependencies
    └── packages/
        └── acme.text/
            └── 1.2.0.zip
```

**Dependency auditing (`rip audit`)**: verifies that `rip.lock` and `packages/` are consistent, matches security advisories, and lists licenses.

- Structural problems (missing `rip.lock`, malformed content, package contents not matching the checksum, newly added unrecorded packages) always cause failure.
- Advisory source priority: `[registry] advisories` > the environment variable `RIP_ADVISORIES` > `<source>/v1/advisories.json`. Advisory fields are `id` / `package` / `requirements` (semver constraint) / `severity` (`low` / `moderate` / `high` / `critical`) / `title` / `url`; the file may be a bare array or `{"advisories": [...]}`.
- `--deny <LEVEL>` sets the failure threshold (default `high`; `none` means never failing because of advisories), `--json` emits machine-readable results, and `--licenses` prints the license of each package (taken from `[module] license` in the package's `rip.toml`).

**6) Common errors**

| Error | Cause | Resolution |
| --- | --- | --- |
| mod name 'x' must match directory name 'y' | The directory name does not match the mod declaration | Rename the directory or change the mod line |
| function 'slug' is already defined | The same package exists as both .rp and .rpa, or two packages export a function with the same name | Remove one of the two forms / rename the function |
| package "text/slug" is not available in this build | The use path does not match the packages/ structure | Fix the use path according to the relative path |
| rip.lock: ... has changed | Package contents changed but were not locked again | rip lock |
| unused import "..." | Applies only to std::*, rust::* | use of a third-party package does not produce this warning |

### 10.11 Distribution and Installation: Single-File Toolchain

Only two things are delivered to users: **the compiled `rip`** and **its bundled runtime library directory**. Users need to install no compilation tools, and do not need to see Rip's implementation source.

**1) Delivery directory structure**

```text
rip/
├── rip.exe                          command-line entry point (the compiled runtime)
├── toolchain/
│   ├── bin/                         built-in compile component and runtime DLLs
│   └── lib/rustlib/<host>/          runtime libraries and linker for the target platform
├── lib/rip/
│   ├── deps/                        AOT runtime library (librp_core-*.rlib and dependencies)
│   ├── build/                       build outputs of dependencies (native library search paths)
│   └── native/                      external native libraries (e.g. winapi import libraries)
└── README.md
```

- `rip` locates `toolchain/` and `lib/rip/` **relative to its own directory**, so the whole directory can be moved to any path with no environment variables needed.
- Each platform's `deps` and `toolchain` are platform-specific artifacts and cannot be mixed across platforms.

**2) Developer assembly (publishing from source)**

Windows:

```powershell
cargo build --release
cargo build --release -p rp-core --no-default-features --target-dir target/aot
powershell -ExecutionPolicy Bypass -File scripts/bundle-toolchain.ps1
# artifact: dist/rip/ (about 1.4 GB, containing the compile component and all runtime libraries)
```

Equivalent steps for Linux / macOS (copy manually using the same layout):

```bash
cargo build --release
cargo build --release -p rp-core --no-default-features --target-dir target/aot
SYSROOT=$(rustc --print sysroot)
HOST=$(rustc -vV | sed -n 's/^host: //p')
OUT=dist/rip
mkdir -p "$OUT/toolchain/bin" "$OUT/toolchain/lib/rustlib/$HOST" "$OUT/lib/rip/deps" "$OUT/lib/rip/build"
cp target/release/rip "$OUT/"
cp "$SYSROOT"/bin/rustc "$SYSROOT"/bin/*.so "$OUT/toolchain/bin/" 2>/dev/null || true
cp -R "$SYSROOT/lib/rustlib/$HOST/." "$OUT/toolchain/lib/rustlib/$HOST/"
cp -R target/aot/release/deps/. "$OUT/lib/rip/deps/"
cp -R target/aot/release/build/. "$OUT/lib/rip/build/" 2>/dev/null || true
```

**3) The user's perspective**

```bash
rip run main.rp                 # interpreted execution: source parsing runs on the compiled runtime
rip build main.rp               # native compilation: calls only rip's built-in components, no extra installation
```

- Both commands depend only on the `rip` directory itself; the compile component, linker, and runtime library for `rip build` are all inside the package.
- Crates needed for Rust interop (`use rust::...;`) are already statically linked into the runtime when `rip` is released, so users likewise need no Rust environment; for example, `rust/serde_json` can be used directly under both `rip run` and `rip build`.
- If a project needs a new Rust crate, the SDK publisher includes it when building `rip`; users obtain it by upgrading `rip`, with no need to install or compile anything themselves.
- Build intermediates (host source) are written only to the system temporary directory and deleted when compilation finishes, so **no implementation source ever appears in the delivery directory**.
- `RIP_TOOLCHAIN` / `RIP_AOT_DEPS` are only for troubleshooting or custom install locations; normal use requires no configuration.
- When a component is missing, the message is "please install the full version of rip" rather than a request to install an external toolchain.

### 10.12 Native Packages (Rust Plugins)

Native packages let Rip call capabilities implemented in Rust, **while users still need only `rip`**. Signatures come from `.rp.decl` and implementations come from a platform dynamic library (under AOT it can be statically linked directly).

**1) Directory convention**

```text
<root>/acme/tools/
├── package.decl      # interface declaration (the only source of signatures)
├── tools.dll         # Windows; Linux uses libtools.so, macOS uses libtools.dylib
└── libtools.a        # optional: static library (Rust staticlib) for static linking with --aot
```

The package path is the relative path, and code writes `use acme::tools;`. Search roots, in order:

1. `RIP_NATIVE_PATH` (multiple entries separated by the path separator; for troubleshooting/custom installs)
1. `<rip>/lib/rip/plugins`: native packages distributed with the toolchain
1. `<executable>/lib/native`: native packages distributed with the delivered artifact
1. `lib/rip/plugins` and `lib/native` under the current working directory—**searched only under the `rip` CLI (source execution and build)**; a packaged binary does not search the working directory, preventing external native packages from being loaded accidentally when started in a malicious directory

**2) Declaration file**

```rp
abi = 1
platform = "windows-x86_64"
sha256 = "…"                 // optional: dynamic library digest; if present, verification is enforced
prefix = "acme"              // optional: symbol prefix, used when multiple native packages coexist

pub func shout(text: string) -> string
pub func fail(msg: string) -> result<string, string>
```

- `abi` must match the current `rip` (version `1` in this release).
- `platform` is `<os>-<arch>`; `x64`/`amd64` are equivalent to `x86_64`.
- `prefix` allows only ASCII letters, digits, and `_`.
- Supported types are `int`/`float`/`string`/`bool`/`any`/`void`, `vec<T>`, `map<K,V>`, `option<T>`, `result<T,E>`, and `share<T>`.

**3) Plugin ABI**

The dynamic library exports four C symbols:

```c
uint32_t rp_native_abi(void);
const uint8_t *rp_native_decl(size_t *out_len);      // returns the same content as package.decl
uint8_t *rp_native_call(const uint8_t *name, size_t name_len,
                        const uint8_t *args, size_t args_len,
                        size_t *out_len);
void rp_native_free(uint8_t *ptr, size_t len);
```

- Arguments are a JSON array, and the return value is `{"ok": …}` or `{"err": "…"}`.
- When the declaration says `prefix = "acme"`, the four symbol names all become `rp_native_acme_abi` / `rp_native_acme_decl` / `rp_native_acme_call` / `rp_native_acme_free`.
- For functions declared to return `result<T,E>`, `err` becomes an `err(...)` value; for functions returning other types, `err` is raised as a runtime error.
- Panics must be caught by `catch_unwind` inside the plugin and must never cross the ABI.
- Validation at load time covers the ABI version, `platform`, `sha256`, and whether the plugin's built-in declaration matches `package.decl`.

**4) Two execution modes**

| Mode | Load timing | Delivery requirements |
| --- | --- | --- |
| rip run | Loaded from the search roots on first call | The native package directory is in place (the CLI's working directory or RIP_NATIVE_PATH) |
| rip build | Linked directly at compile time when a static library exists; loaded at runtime otherwise | Statically linked packages need no delivery; others are copied to <artifact>/lib/native/<package path> |

- **Static linking fast path**: when `lib<name>.a` (Linux/macOS/Windows-GNU) or `<name>.lib` (Windows-MSVC) exists in the package directory, `rip build` links it directly into the executable with `#[link(name = "…", kind = "static")]`; the artifact remains a single file and no `lib/native/` is generated; the ABI version is validated at startup, and signatures still come from `package.decl`. A plugin using `crate-type = ["cdylib", "staticlib"]` provides both forms at once.
- `.dll.a` / `.dll.lib` are dynamic library import libraries and are not treated as static libraries.
- **Coexistence of multiple native packages**: when `package.decl` specifies `prefix = "acme"`, the four ABI symbols carry the prefix, so any number of prefixed packages can be statically linked into one artifact at the same time.

Packages without a prefix share fixed symbol names, so only one of them can be statically linked into an artifact; the extras fall back to dynamic loading in package-path order, with a hint printed at build time.

- `rip build` copies only the **native packages actually `use`d by the program**, and `README.md` lists them one by one (statically linked ones are marked "statically linked").
- Native packages are binary files and are not affected by the "deliverables contain no source" rule; when no native package is used (or all are statically linked), the artifact remains a single file.

**5) Minimal plugin implementation**

```rust
const DECL: &str = "abi = 1\nplatform = \"windows-x86_64\"\n\npub func shout(text: string) -> string\n";

#[no_mangle]
pub extern "C" fn rp_native_abi() -> u32 { 1 }

#[no_mangle]
pub extern "C" fn rp_native_decl(out_len: *mut usize) -> *const u8 {
    unsafe { *out_len = DECL.len() };
    DECL.as_ptr()
}

#[no_mangle]
pub extern "C" fn rp_native_call(_: *const u8, _: usize, args: *const u8, args_len: usize, out_len: *mut usize) -> *mut u8 {
    let args: serde_json::Value = serde_json::from_slice(unsafe { std::slice::from_raw_parts(args, args_len) }).unwrap();
    let text = args.get(0).and_then(|v| v.as_str()).unwrap_or("");
    let body = serde_json::to_vec(&serde_json::json!({ "ok": text.to_uppercase() })).unwrap();
    let mut boxed = body.into_boxed_slice();
    unsafe { *out_len = boxed.len() };
    let pointer = boxed.as_mut_ptr();
    std::mem::forget(boxed);
    pointer
}

#[no_mangle]
pub extern "C" fn rp_native_free(pointer: *mut u8, len: usize) {
    unsafe { drop(Box::from_raw(std::ptr::slice_from_raw_parts_mut(pointer, len))) };
}
```

Build: `cargo build --release` (`[lib] crate-type = ["cdylib"]`; write `["cdylib", "staticlib"]` if AOT static linking is desired), then put the dynamic library (and optionally the static library) together with `package.decl` into the directory described above. To allow multiple packages to coexist, rename the four `#[no_mangle]` functions to prefixed names as described in the table above and write the same `prefix` in `package.decl`.

### 10.13 Runtime Configuration

At build time, `[server]`, `[cluster]`, `[db]`, and `[runtime]` from `rip.toml` are baked into the artifact. Adjusting these parameters after deployment does not require a rebuild.

**1) Configuration file**

Place `config/rip.toml` next to the executable:

```text
myapp/
├── myapp.exe
└── config/
    └── rip.toml
```

```toml
[runtime]
log = "chinese"      # english | chinese
log_level = "debug"  # trace | debug | info | warn | error
log_format = "json"  # text | json
log_file = "logs/app.log"

[server]
host = "0.0.0.0"
workers = 16
max_connections = 2048
max_body_bytes = 33554432
read_timeout_ms = 60000
health_path = "/healthz"
otlp_endpoint = "http://127.0.0.1:4318"   # leave empty to disable trace export
otlp_service = "myapp"
otlp_sample_ratio = 0.1

[cluster]
redis = "redis://127.0.0.1:6379/0"
task_lock = true

[db]
max_open = 32            # concurrently live connections allowed per DSN (idle + checked out)
max_idle = 8             # upper bound of cached idle connections per DSN
acquire_timeout_ms = 5000
idle_timeout_sec = 300
```

Lookup order (the first hit is used): the path given by `RIP_CONFIG` → `<executable dir>/config/rip.toml` → `./config/rip.toml`. UTF-8 with BOM is also recognized.

**2) Environment variables**

In container and orchestration environments, no file needs to be mounted:

| Variable | Overrides |
| --- | --- |
| RIP_SERVER_HOST | [server] host |
| RIP_SERVER_WORKERS | [server] workers |
| RIP_SERVER_MAX_CONNECTIONS | [server] max_connections |
| RIP_SERVER_MAX_BODY_BYTES | [server] max_body_bytes |
| RIP_CLUSTER_REDIS | [cluster] redis |
| RIP_CLUSTER_TASK_LOCK | [cluster] task_lock |
| RIP_DB_MAX_OPEN | [db] max_open |
| RIP_DB_MAX_IDLE | [db] max_idle |
| RIP_DB_ACQUIRE_TIMEOUT_MS | [db] acquire_timeout_ms |
| RIP_DB_IDLE_TIMEOUT_SEC | [db] idle_timeout_sec |
| RIP_LOG_LEVEL | [runtime] log_level |
| RIP_LOG_FORMAT | [runtime] log_format |
| RIP_LOG_FILE | [runtime] log_file |
| RIP_DISABLE_SHELL | When set, disables `process.run_shell` (an operations safety switch; calls return an error directly) |
| RIP_FS_DENY | Operations path-denial switch: a list of directory prefixes separated by the platform path separator (e.g. `/etc:/proc` or `C:\Windows;C:\Program Files`); if the path argument of any `std/fs` operation falls under a prefix, it is an immediate error. Snapshotted at process start, so code cannot bypass it |
| RIP_HTTP_BLOCK_PRIVATE | When set, `http.fetch` / `fetch_file` / `download` refuse outbound access to loopback, private network, link-local, unique-local, and multicast addresses (SSRF protection; off by default) |
| OTEL_EXPORTER_OTLP_ENDPOINT | [server] otlp_endpoint |
| OTEL_SERVICE_NAME | [server] otlp_service |
| OTEL_TRACES_SAMPLER_ARG | [server] otlp_sample_ratio |

**3) Priority**

`command-line arguments > environment variables > config/rip.toml > build-time embedded values`

Only `--port` / `--host` / `--cert` / `--key` can be overridden on the command line; all other items go through environment variables or the configuration file. Programs produced by `rip run` and `rip build` follow the same rules.

**4) Deployment isolation recommendations**

Rip's positioning is "code is trust": `std/fs` / `std/process` provide no language-level sandbox, and `RIP_FS_DENY` / `RIP_DISABLE_SHELL` are only in-process guardrails on the operations side (against misuse, not against malicious code); the real security boundary lies at the operating system level. For production deployment:

- Run the service process under a dedicated low-privilege system account or container, granting file permissions only to the application's own data directory;
- Set `RIP_FS_DENY` when the file access surface must be limited, and set `RIP_DISABLE_SHELL` when the shell escape hatch must be closed;
- Delivered artifacts load native packages only from the exe-relative directory and `RIP_NATIVE_PATH`, without searching the working directory; fill in `sha256` in native package declarations whenever possible.

---

## 11. Standard Library

### 11.1 Namespaces

| Namespace | Responsibility |
| --- | --- |
| std/sys | Processes, environment, signals, system information |
| std/fs | Files, directories, paths |
| std/time | Time, dates, timers, IANA time zones (see 11.14) |
| std/net | TCP / UDP |
| std/http | HTTP server / client / routing |
| std/ws | WebSocket |
| std/json | JSON encoding and decoding |
| std/db | Database connections, transactions, relational queries, metrics (see 11.12) |
| std/str | String utilities |
| std/vec | Sequence utilities |
| std/map | Dictionary utilities |
| std/crypto | Hashing, encoding, random numbers, password hashing, AES, RSA (see 11.6) |
| std/xml | XML parsing and serialization (see 11.7) |
| std/encode | URL / HTML / CSV / query string encoding and decoding (see 11.8) |
| std/task | Background tasks, scheduled task scheduling (see 11.5) |
| std/sync | Named mutexes, atomic integers, channels (see 11.5) |
| std/dev | Debugging, logging, breakpoints, timing (removed in release) |
| std/log | Leveled logging |
| std/redis | Redis client, distributed locks, cluster rate limiting (see 11.11) |
| std/cache | Process-level TTL + LRU cache (see 11.12) |
| std/queue | SQLite-based persistent task queue (see 11.12) |
| std/cli | Arguments, subcommands, interactive input (see 11.12) |
| std/llm | Unified large model invocation |
| std/zip | ZIP archive reading and writing (see 11.14) |
| std/mail | SMTP mail sending (see 11.14) |
| std/reflect | Runtime type information and dynamic dispatch (see 11.15) |

**There is no** **`std/tpl`**; HTML templates are provided later through packages.

### 11.2 HTTP (the Return Format Is Decided by `return`)

```rp
use std::http;

http.serve(port: int, handler: func(http.request) -> http.response) -> void;
http.router(routes: vec<http.route>) -> func(http.request) -> http.response;
http.get / post / put / delete / patch / options / head(path, handler);
http.static(prefix: string, dir: string) -> http.route;
http.save_file(dir: string, file: http.file) -> result<string, string>;
http.fetch(method: string, url: string, headers: map<string, string>, body: string)
    -> result<http.client_response, string>;
http.fetch_file(method: string, url: string, headers: map<string, string>, path: string)
    -> result<http.client_response, string>;
http.fetch_retry(method: string, url: string, headers: map<string, string>, body: string, attempts: int)
    -> result<http.client_response, string>;
http.chain(middlewares: vec<func(http.request) -> http.response>, handler: func(http.request) -> http.response)
    -> func(http.request) -> http.response;
http.group(prefix: string, middlewares: vec<...>, routes: vec<http.route>) -> http.route;
http.cors(origin: string) -> func(http.request) -> http.response;
http.security_headers() -> func(http.request) -> http.response;
http.with_headers(headers: map<string, string>) -> func(http.request) -> http.response;
http.rate_limit(rate: int, burst: int) -> func(http.request) -> http.response;
http.openapi(handler: func(http.request) -> http.response, title: string, version: string) -> string;

type http.client_response = [
    status: int,
    headers: map<string, string>,
    body: string
];

type http.request = [
    method: string,
    path: string,
    params: map<string, string>,
    query: map<string, string>,
    headers: map<string, string>,
    body: string,
    json: option<any>,
    form: map<string, string>,
    files: vec<http.file>,
    cookies: map<string, string>,
    client_ip: string,
    request_id: string,
    ctx: map<string, any>
];

type http.file = [
    field: string,
    filename: string,
    content_type: string,
    size: int,
    data_base64: string
];

type http.response = [
    status: int,
    headers: map<string, string>,
    body: string,
    json: option<any>,
    sse: option<func(func(http.sse_event) -> void) -> void>
];

type http.sse_event = [event: string, data: string];
```

**Core rules**:

- `http.response` is just an ordinary `ary`; **whatever the user `return`s is what is sent, and the framework imposes no format**.
- `body` field: returns the raw string, and **the framework sets no `Content-Type`**; users write `headers` themselves.
- `json` field: **optional convenience sugar**. When the user assigns an `ary` / `vec` / `map` to the `json` field, the framework serializes it automatically and sets `Content-Type: application/json`.
- `sse` field: used when the user explicitly chooses SSE. Response headers are sent first, events are flushed frame by frame, and it ends when the client disconnects; one connection occupies one interpreter thread, so raise `[server] workers` when there are many long connections.
- Choose exactly one of **`body`** **/** **`json`** **/** **`sse`**; exclusivity is checked at compile time.
- **Middleware**: `http.chain([mw...], handler)` wraps in order and is composed and cached on the first request; `mw` is `func(next handler) -> new handler`. `http.cors("*")` handles `OPTIONS` preflight automatically, `http.security_headers()` adds security headers, `http.with_headers(map)` appends fixed headers, and `http.rate_limit(rate, burst)` performs route-level rate limiting by client IP. `http.group(prefix, [mw...], [routes...])` lets routes in the group use relative paths, registering the whole group as one prefix route.
- **OpenAPI**: `http.openapi(router, title, version)` generates OpenAPI 3.0 JSON from the route table (paths, methods, `operationId`, path parameters); call it once after the route table is built.
- **Request context**: `req.ctx` is a `map<string, any>` written by middleware (e.g. the current user) and read by the handler; the same map is shared within one request.
- **Response compression**: text-like responses of 1 KiB or more are gzip-compressed automatically according to `Accept-Encoding`, with `Vary: Accept-Encoding` added; static files and SSE are not compressed. No handling is needed in business code.
- **Static assets**: `http.static` responses carry `ETag` / `Last-Modified` / `Cache-Control` and support `If-None-Match` (`304`) and single-part `Range` (`206` / `416`); files are streamed from disk, without occupying interpreter memory; HTML uses `no-cache`, and everything else is long-cached.
- **Sessions and validation**: `std/session` provides HMAC-SHA256-signed stateless cookies (`save` / `cookie` / `load` / `clear` / `csrf_token` / `csrf_check`), so multiple instances need no shared storage; `std/validate` turns field validation into one line and returns readable errors. `std/jwt` provides HS256 `sign` / `verify` (verification forces the algorithm to HS256, rejects `none`, and enforces `exp` by default, optionally accepting `aud` / `iss` for audience and issuer checks). Keys must come from environment variables or configuration. `Set-Cookie`'s `Secure` attribute is decided as follows: it is added automatically when the server enables HTTPS (`--cert` / `--key`); for plain HTTP deployment behind a trusted proxy (`trust_proxy = true`), passing the current request to `cookie(value, ttl, request)` and `clear(request)` decides whether to add `Secure` based on the rightmost value of `X-Forwarded-Proto` (without the request argument, the old behavior is kept and `Secure` is not added).
- **Templates**: `std/template.render(source, data)` escapes HTML by default and supports `if` / `unless` / `each` / comments; compiled results are cached per source in thread-local storage, suitable for server-side rendering.
- Returning HTML: `body` + `headers = ["Content-Type" = "text/html; charset=utf-8"]`.
- Returning JSON: use the `json` field (sugar), or `json.encode` it yourself and put it in `body` with hand-written headers (manual). **Both are allowed**; their semantics differ, so this is not a second spelling.
- `multipart/form-data` requests are parsed automatically: ordinary fields are merged into `form` and files go into `files`; `http.save_file(dir, file)` decodes `data_base64`, sanitizes the file name, and writes it into the directory (created automatically if missing), returning the on-disk path or `err(...)`. The directory argument is an ordinary string decided by the code; the runtime has no built-in upload directory.
- `http.fetch` is an outbound request: it supports `http` / `https` (system root certificate verification), any method and headers, and a string request body, returning `status` / `headers` / `body`. It is used to call third-party APIs, webhooks, etc.; target addresses and credentials are all supplied by the code, and the runtime imposes no allowlist or directory restriction. `http.fetch_file` behaves the same, but the request body is read directly from the file's raw bytes, suitable for uploading images, videos, and other binary content. `http.download(url, headers, path)` writes the raw response body bytes to a file and returns the byte count, suitable for downloading binary objects. The connection timeout is 30 seconds and the read/write timeout is 60 seconds; `HEAD` reads only response headers and not the body. The response body limit is 256 MiB (pre-checked via `Content-Length` plus a read cap); exceeding it returns `err(...)`, preventing a malicious peer's large response from exhausting memory. When the environment variable `RIP_HTTP_BLOCK_PRIVATE=1` is set, `http.fetch` / `fetch_file` / `download` refuse access to loopback, private network (RFC1918), link-local, unique-local, and multicast addresses (an SSRF protection switch, off by default to preserve existing intranet call behavior).
- **Object storage belongs to the Rip layer and is not built into the runtime**: S3 / OSS / COS are all public HTTP protocols plus signing specifications, so they can be implemented in Rip code or a third-party package using `http.fetch` / `http.fetch_file` / `http.download` along with `std/crypto` (`sha256`, `hmac_sha256_hex`, `sha256_file`) and `std/encode` (`url_encode`); the repository includes `examples/s3_demo/packages/s3` as a reference implementation (AWS SigV4, compatible with S3-compatible services such as MinIO / R2 / B2). The runtime provides only generic HTTP and cryptographic primitives, introducing no vendor SDK or protocol branch.
- `http.fetch_retry(..., attempts)` retries with a backoff of 100ms × attempt count on connection failure or a 5xx response, with `attempts` in the range 1..5; if it still fails, the last response (5xx) or `err(...)` is returned. It suits transient-failure scenarios such as calling cross-machine services or object storage; business errors (4xx) are not retried.

**Server runtime semantics (mandatory)**:

- **Concurrency**: plaintext HTTP connections are managed by tokio (keep-alive occupies no thread; the I/O thread count scales with CPU cores, limited to 4–16), and handler functions execute on a fixed number of interpreter threads (CPU cores by default, overridable with `[server] workers`); requests are dispatched to the idlest interpreter thread according to "remaining queue capacity", avoiding head-of-line blocking caused by a single slow route; response headers and body are merged into one write; TLS still uses one worker thread per connection. Each thread has its own interpreter, and `static` / globals are isolated per thread and **not shared across requests**; use Redis, a database, or files when state must be shared.
- **Connection reuse**: HTTP/1.1 keep-alive is supported; the connection is closed after idling beyond `[server] keep_alive_ms` or reaching `max_requests`.
- **Timeouts**: read and write timeouts are controlled by `[server] read_timeout_ms` / `write_timeout_ms`, preventing slow connections from occupying threads for long periods.
- **Rate limiting backpressure**: the connection queue limit is `max_connections`; beyond it a `503` is returned directly and the connection is closed; a request body exceeding `max_body_bytes` returns `413`; an overlong request line or headers returns `413`. Both `Content-Length` and `Transfer-Encoding: chunked` are supported.
- **In-process rate limiting**: `[server] rate_limit` enables a token bucket (refilled every second); exceeding it returns `429` + `Retry-After: 1` without entering the handler; with `rate_limit_per_ip = true`, buckets are per client IP, and `rip_http_rate_limited_total` records the count. When a global quota is needed across multiple instances, use `rate_limit` from `std/redis` instead.
- **Reverse proxy**: when deployed behind a trusted proxy, set `[server] trust_proxy = true`; the client IP is taken from the leftmost valid address in `X-Forwarded-For` (falling back to `X-Real-IP`), and access logs, rate limiting, and `req.client_ip` all take effect accordingly. For direct connections keep it `false`, otherwise clients can forge their IP. A safer approach is `trusted_proxies = "10.0.0.0/8,..."`: forwarding headers are trusted only for requests from those network ranges, and the real client is reconstructed hop by hop. When `trust_proxy = true` is enabled without configuring `trusted_proxies`, a warning is printed at startup noting that any client can forge `X-Forwarded-For`.
- **Fault isolation**: a runtime error in a single request affects only that request (returning `500`); a panic in a worker thread is caught and the thread continues serving subsequent connections.
- **Graceful shutdown**: after receiving SIGINT / SIGTERM (Ctrl+C on Windows), accepting stops, queued connections are processed to completion, and then the process exits; during shutdown `/readyz` returns `503`.
- **Built-in endpoints**: `GET /healthz` always returns `200 ok` (liveness probe); `GET /readyz` returns `200 ready` when not shutting down and `503` while shutting down; when `[server] metrics_path` is non-empty, that path returns metrics in Prometheus text format. All three paths are handled before user routes and can be changed or disabled in `[server]`.
- **Request ID**: when `X-Request-Id` is missing, `req-<time>-<sequence>` is generated automatically, written to `http.request.request_id`, and returned in the response headers; the JSON format of `std/log` includes this field automatically.
- **Metrics**: `http.metrics()` returns the same Prometheus text as the built-in endpoint, for custom exposure paths.

```rp
type http.request = [
    method: string,
    path: string,
    params: map<string, string>,
    query: map<string, string>,
    headers: map<string, string>,
    body: string,
    json: option<any>,
    form: map<string, string>,
    files: vec<http.file>,
    cookies: map<string, string>,
    client_ip: string,
    request_id: string
];
```

### 11.2.1 Route Annotations (Default Registration Method)

Functions with route annotations are **registered automatically** when `http.router` is built, without needing to be written again in an explicit route table:

```rp
#[get("/api/users")]
func users_index(read req: http.request) -> http.response {
    return http.response[status = 200, json = [items = []]];
}

#[post("/api/users")]
func users_create(read req: http.request) -> http.response {
    return http.response[status = 201, json = [id = 1]];
}

func main() -> void {
    let extra: vec<http.route> = [http.static("/", "public")];
    http.serve(8080, http.router(extra));
}
```

Rules:

- The signature of an annotated function is fixed as `func(read req: http.request) -> http.response`: exactly one `http.request` parameter, returning `http.response`. The parameter may omit `read` but cannot be `write`.
- The annotation provides only the method and path; request parsing and response assembly are done by the function itself.
- Database connections, configuration, and other things the handler needs are held by the application itself (e.g. a module-level `static`); the framework **implicitly injects** no extra parameters.
- Two annotations declaring the same `(method, path)` in one program are a compile error.
- Registration order: annotated routes first, then the routes passed explicitly to `http.router`. Hence when an annotation and an explicit route duplicate, the annotation matches first; the `/*` wildcard generated by `http.static` must be placed in explicit routes, otherwise it would shadow annotated routes.
- Explicit route tables (`http.get(path, handler)` and so on) remain fully usable; without annotations behavior is the same as before. The two approaches may be mixed.
- When there are no explicit routes, pass an empty table: `let extra: vec<http.route> = []; http.serve(8080, http.router(extra));`.
- The bind address defaults to `127.0.0.1` (the development default, reachable only from the local machine); `rip run --host H` / `rip dev --host H` can change it to `0.0.0.0`, a local IP, or a locally resolvable domain name, and the startup log prints the actually configured address.

### 11.2.2 HTTPS / TLS

`http.serve` supports both plaintext HTTP and HTTPS; TLS is injected by the CLI and is not written into Rip source:

```powershell
rip run main.rp --cert server.pem --key server.key --port 443
```

- `--cert` is a PEM certificate chain (it may contain multiple certificates, the first being the site certificate), and `--key` is the corresponding PEM private key (PKCS#8 or PKCS#1).
- Both must be provided together; when only one is given, `http.serve` reports the runtime error `HTTPS requires both --cert and --key`.
- Once enabled, the startup log uses `https://`, and the port, routes, static assets, and upload logic of `http.serve` are completely unchanged.
- The certificate and private key are read only from the CLI and do not participate in `rip build` packaging; at deployment, put the PEM files at the desired path on the target machine and point to them with `--cert` / `--key`.
- The TLS version is negotiated by the runtime (TLS 1.2 / 1.3), and client certificates are not verified (no mTLS).

### 11.3 String Methods

```rp
s.len() -> int
s.at(i: int) -> result<string, string>
s.slice(start: int, end: int) -> result<string, string>
s.first() / s.last() -> string
s.contains(sub: string) -> bool
s.starts_with(pre: string) -> bool
s.ends_with(suf: string) -> bool
s.index_of(sub: string) -> option<int>
s.last_index_of(sub: string) -> option<int>
s.to_upper() / s.to_lower() -> string
s.trim() / s.trim_start() / s.trim_end() -> string
s.replace(old, new) / s.replace_all(old, new) -> string
s.split(sep: string) -> vec<string>
s.split_lines() -> vec<string>
s.chars() -> vec<string>
s.bytes() -> vec<u8>
s.reverse() -> string
s.filter(pred: func(string) -> bool) -> string
s.remove_whitespace() -> string
s.keep_chars(chars: string) / s.remove_chars(chars: string) -> string
s.to_int() -> result<int, string>
s.to_float() -> result<float, string>
s.to_bool() -> result<bool, string>
s.pad_start(n: int, pad: string) / s.pad_end(n: int, pad: string) -> string
s.repeat(n: int) -> string
s.join(items: vec<string>) -> string
s.normalize(form: string) -> result<string, string>   // nfc / nfd / nfkc / nfkd
s.graphemes() -> vec<string>                          // grapheme clusters (user-visible characters)
s.grapheme_len() -> int
s.width() -> int                                      // terminal display width
```

### 11.4 Other Libraries (Core Methods)

**`std/llm`** (four methods, no aliases):

```rp
type llm.message = [role: string, content: string];
type llm.options = [model: string, temperature: float, max_tokens: int, top_p: float, stream: bool];
type llm.response = [content: string, model: string, prompt_tokens: int, completion_tokens: int, finish_reason: string];

llm.new_client(config: llm.config) -> llm.client;
client.chat(msgs: vec<llm.message>, opts: llm.options) -> result<llm.response, string>;
client.chat_stream(msgs, opts, on_chunk: func(string) -> void) -> result<llm.response, string>;
client.chat_json(msgs, opts, target: type) -> result<any, string>;
```

**`std/cli`**: `args` / `command` / `flag` / `option` / `positionals`, see 11.12.

**`std/dev`**: `log` / `log_json` / `breakpoint` / `assert` / `time_start` / `time_end` / `trace`, removed at compile time under `rip build --release`.

**`std/log`**: `debug` / `info` / `warn` / `error` / `set_level` / `set_format` / `set_file`.

- After `set_level("debug" | "info" | "warn" | "error")`, lower-level logs are no longer emitted (default `info`).
- `set_format("text" | "json")`: `text` produces `time [level] message`; `json` produces one object per line, `{"time","level","message"[,"request_id"]}`, which is convenient for log collection.
- `set_file(path) -> result<bool, string>`: also appends to a file (the directory must already exist), returning `ok(true)` or `err(reason)`; console output is retained.

### 11.5 Scheduled Tasks and Concurrency (`std/task` + `std/sync`)

Rip's concurrency model is **thread-level isolation**: each task runs in its own thread with independent interpreter state, and threads share no Rip variables and no `vec` / `map` / `ary`.

**Scheduled tasks** are declared with annotations and scheduled automatically at program startup:

```rp
#[interval("1m")]
func session_cleanup() -> void {
    // runs once per minute
}

#[interval("500ms")]
func poll_queue() -> void {
    // runs every 500 milliseconds
}
```

Rules:

- `#[interval("...")]` may be attached only to a module-level parameterless `func`; parameters, an unknown attribute name, or an invalid interval are all compile errors.
- Interval format: a positive integer + `ms` (milliseconds) / `s` / `m` / `h` / `d`, e.g. `"100ms"`, `"30s"`, `"5m"`, `"1h"`, `"1d"`; it must be greater than 0.
- Startup order: all scheduled tasks start before `main` begins executing; when `main` returns the process ends and the tasks terminate with it. A long-running service blocks `main` in `http.serve`, so the tasks keep running.
- A task's first execution happens after one interval; it does not run immediately at startup.
- An error inside a task does not terminate the process; it only prints `[task] function_name:line: error: ...`.

**One-shot background tasks** use `std/task`:

```rp
use std::task;

task.spawn("send_report") -> bool        // run a parameterless function asynchronously; immediately returns whether it started
task.sleep(ms: int) -> void              // put the current thread to sleep
```

**Async functions (`async func` / `await`)**:

```rp
async func load(id: int) -> user {
    // executes in an independent runtime
    return fetch_user(id);
}

func handle(id: int) -> void {
    // start several tasks first, then await as needed: the tasks truly run in parallel.
    let a: task<user> = load(1);
    let b: task<user> = load(2);
    let first: user = await a;
    let second: user = await b;
    print(first.name + second.name);
}
```

Rules:

- `async` may modify only module-level free functions; methods, `main`, and functions carrying attributes (route / interval / test) cannot be async.
- Calling an async function **immediately** hands the task to the scheduler and returns `task<T>`, where `T` is the function's return type. The task starts running at the call site and is not deferred until `await`.
- `await` accepts only `task<T>` and returns `T`; using it on a non-task value is a compile error. A single task can be awaited only once (the ownership check rejects a second await).
- A task that is never awaited keeps running in the background and its result is discarded; always await when the result is needed.
- Task functions run in an **independent runtime**: they cannot see the caller's local variables, module-level `static`, or closures, and can interact only through parameters, return values, and external resources such as databases, files, and `std/sync`.
- Parameters and return values are copied across threads; `func` / closures and `task` cannot cross a task boundary (a compile-time or runtime error).
- A failed awaited task re-raises the original error (including the function stack) to the caller, behaving exactly like a synchronous call.
- The scheduler is a process-level bounded queue plus fixed worker threads (4 × CPU cores, 8..=64); when the queue is full it executes directly on the calling thread, providing backpressure. `await` first waits for a completion notification (about 200µs), allowing the task to finish in parallel on a scheduling thread; only if it is still incomplete at the timeout does it execute queue tasks itself, so nested `await` never deadlocks from worker exhaustion.
- Task threads reuse an already-initialized runtime: before each task, globals, borrowed database / Redis connections, network handles, the defer stack, and the request context are cleared, and module-level variables are reinitialized; the isolation semantics match creating a new runtime, but the runtime construction cost is saved.
- The debugger tracks only the thread that created the task; task threads execute independently and do not hit breakpoints set by the caller.

```rp
use std::time;

async func nap(ms: int) -> int {
    time.sleep(ms);
    return ms;
}

func main() -> void {
    let start: int = time.timestamp_ms();
    let a: task<int> = nap(400);
    let b: task<int> = nap(400);
    let total: int = await a + await b;
    // parallel execution: about 400ms in total, not 800ms serially.
    print(total);
    print(time.timestamp_ms() - start);
}
```

**Cross-thread sharing (`std/sync`)**:

Rip values (`vec` / `map` / `ary` / closures) use single-threaded reference counting and cannot cross threads. Shared state across threads uses process-level named primitives:

```rp
use std::sync;

sync.mutex(name: string) -> sync.mutex
sync.atomic(name: string, initial: int) -> sync.atomic
sync.channel(name: string, capacity: int) -> sync.channel
sync.exists(name: string) -> bool
sync.remove(name: string) -> bool
```

Rules:

- **Sharing by name**: within one process, the same name yields the same object; HTTP worker threads, `parallel` branches, `task.spawn`, and `#[interval]` can all see it.
- The name must be a fixed constant; do not build it from request parameters (the registry keeps entries per name permanently, and dynamic names grow without bound).
- **The same name cannot change type**: creating it as a mutex and later using it as a channel reports a runtime error.
- Handles are ordinary values and can be placed in `vec` / `map` / `ary`; calling methods requires no `let mut` (the object itself is process-wide shared).
- `sync.remove` should be called only when no other thread is using the name (e.g. test teardown); after removal, the same name creates a new object, and old handles still point to the old object.

**Mutex**:

```text
mutex.lock() -> void
mutex.try_lock() -> bool
mutex.lock_timeout(ms: int) -> bool
mutex.unlock() -> void
mutex.is_locked() -> bool
mutex.name() -> string
```

- `lock` blocks until acquired; `try_lock` returns immediately; `lock_timeout` waits at most `ms`.
- The owner is not recorded, and any thread can `unlock`. Inside a critical section, releasing with `defer` is recommended:

```rp
let mutex: sync.mutex = sync.mutex("report_lock");

func write_report(text: string) -> result<void, string> {
    mutex.lock();
    defer mutex.unlock();     // released even on early return / error
    fs.write_file("report.txt", text)?;
    return ok;
}
```

**Atomic integers**:

```text
atomic.load() -> int
atomic.store(v: int) -> void
atomic.add(v: int) -> int      // returns the old value before the addition
atomic.sub(v: int) -> int      // returns the old value before the subtraction
atomic.swap(v: int) -> int     // returns the old value
atomic.compare_swap(old: int, new: int) -> bool
atomic.name() -> string
```

- All are `SeqCst` atomic operations, suitable for counters, ID allocation, and task completion flags.
- `initial` takes effect only at first creation; calling again with the same name does not reset it.

**Channel**:

```text
channel.send(value: any) -> void
channel.try_send(value: any) -> bool
channel.send_timeout(value: any, ms: int) -> bool
channel.recv() -> option<any>
channel.try_recv() -> option<any>
channel.recv_timeout(ms: int) -> option<any>
channel.close() -> void
channel.is_closed() -> bool
channel.len() -> int
channel.capacity() -> int
channel.name() -> string
```

- Bounded queue: capacity `1..1000000`; when full, `send` blocks, `try_send` returns `false`, and `send_timeout` waits at most `ms`.
- `recv` blocks until a value is available or the channel is closed; `try_recv` returns immediately; `recv_timeout` waits at most `ms`.
- After closing: queued values can still be drained, after which `recv` returns `none` immediately; `send` reports an error, and `try_send` / `send_timeout` return `false`.
- The channel transfers a **deep copy** of the value (preserving integer width, `option` / `result`, `ary` class name, and `map` key type), not a shared reference; function values cannot enter a channel.
- `option<any>` is narrowed with `.as_int()` / `.as_string()` and so on, consistent with the `json.decode` convention.

```rp
// producer: a background task
func worker() -> void {
    let jobs: sync.channel = sync.channel("jobs", 64);
    let done: sync.atomic = sync.atomic("jobs_done", 0);
    jobs.send(42);
    done.add(1);
}

// consumer: the main thread polls with a timeout and is never blocked forever
task.spawn("worker");
let jobs: sync.channel = sync.channel("jobs", 64);
match jobs.recv_timeout(1000) {
    some(value) => { dev.log(value.as_int().unwrap_or(0).to_string()); }
    none => { dev.log("no message within 1 second"); }
}
```

**Timeouts and cancellation**:

- All blocking operations (`lock` / `lock_timeout` / `send` / `send_timeout` / `recv` / `recv_timeout`) check the HTTP handler's deadline every 20ms; a request timeout interrupts immediately and returns 504, so a worker thread is never stuck waiting for a lock or a channel.
- Background tasks have no deadline; use the `*_timeout` variants or `try_*` to control them yourself and avoid indefinite blocking.
- Threads still share no Rip variables; only `std/sync` primitives and process-level external resources (cache, queue, Redis, database) are shared.

**Working with `parallel`**: each statement in `parallel` is an independent branch that cannot see outer local variables; obtain handles to same-named primitives inside the branch to aggregate results.

```rp
parallel {
    sync.atomic("jobs_done", 0).add(1);
    sync.atomic("jobs_done", 0).add(1);
}
let done: sync.atomic = sync.atomic("jobs_done", 0);
// done.load() == 2
```

**Multi-instance deployment**: scheduled tasks start on every machine and repeat by default. After configuring `[cluster] redis` + `task_lock = true`, each trigger first elects a leader via a Redis lease, and only the instance that acquires `rip:task:<function name>` executes; see 11.11 for details.

### 11.6 Encryption, Encoding, and Password Hashing (`std/crypto`)

```rp
use std::crypto;

crypto.md5(text: string) -> string
crypto.sha1(text: string) -> string
crypto.sha256(text: string) -> string
crypto.sha512(text: string) -> string
crypto.hmac_sha256(key: string, text: string) -> string
crypto.hmac_sha256_hex(key_hex: string, text: string) -> result<string, string>
crypto.sha256_file(path: string) -> result<string, string>
crypto.base64_encode(text: string) -> string
crypto.base64_decode(text: string) -> result<string, string>
crypto.hex_encode(text: string) -> string
crypto.hex_decode(text: string) -> result<string, string>
crypto.random_hex(bytes: int) -> result<string, string>
crypto.password_hash(password: string) -> result<string, string>
crypto.password_verify(password: string, hashed: string) -> bool

crypto.aes_encrypt(text: string, key: string) -> result<string, string>
crypto.aes_decrypt(cipher: string, key: string) -> result<string, string>
crypto.rsa_generate(bits: int) -> result<map<string, string>, string>
crypto.rsa_sign(text: string, private_key_pem: string) -> result<string, string>
crypto.rsa_verify(text: string, signature: string, public_key_pem: string) -> bool
crypto.rsa_encrypt(text: string, public_key_pem: string) -> result<string, string>
crypto.rsa_decrypt(cipher: string, private_key_pem: string) -> result<string, string>
```

Rules:

- Digest functions return a **lowercase hexadecimal** string; `base64_*` uses standard Base64 (with `=` padding).
- `base64_decode` / `hex_decode` return `err("invalid ... input")` for invalid input instead of terminating the process.
- `random_hex(n)` returns the hexadecimal string (length `2n`) of `n` bytes of cryptographic randomness; `n` must be in `1..=1024`, and values out of range return `err(...)`.
- `password_hash` returns `ript$iterations$salt$digest`: PBKDF2-HMAC-SHA256 (210000 iterations) with a 16-byte random salt, so the same password yields a different result each time; the iteration count is written into the hash string, so raising it later does not affect verification of old data.
- `password_verify(password, hashed)` reads the iteration count and salt from the string and recomputes, using a constant-time comparison; it accepts only the `ript$` format, and old formats, invalid strings, abnormal iteration counts, and mismatched passwords all return `false` without raising an error.
- Password fields store only the result of `password_hash`, and verification always uses `password_verify`; do not concatenate `sha256` comparisons yourself. Use the release build of `rip` (`cargo build --release`) in production: the debug build is unoptimized and each hash takes several seconds, while the release build takes about 100–150ms.
- `aes_encrypt` / `aes_decrypt` use AES-256-GCM. The current format (V2): the key is derived via PBKDF2-HMAC-SHA256 (210000 iterations, with a fresh 16-byte salt generated for each encryption) into 32 bytes, and the ciphertext is `RIPV2:base64(salt || 12-byte random nonce || ciphertext || 16-byte authentication tag)`; `aes_decrypt` also accepts the old unprefixed format (key derived directly with SHA-256), so existing data needs no migration, while new data is always V2. A wrong key or tampered ciphertext makes decryption return `err(...)`.
- `rsa_*`: `rsa_sign` / `rsa_verify` use RSA PKCS#1 v1.5 signatures (internally fixed to a SHA-256 digest, with a Base64 result); `rsa_encrypt` / `rsa_decrypt` use **RSA-OAEP (SHA-256)** rather than PKCS#1 v1.5 encryption, which is vulnerable to Bleichenbacher attacks (old v1.5 ciphertexts cannot be decrypted by the current runtime). `rsa_generate(bits)` generates a `bits`-bit key pair (`1024..=8192` and a multiple of 8), returning `private_key_pem` (PKCS#8) and `public_key_pem` (SPKI).
- The V2 format of `aes_encrypt` has built-in PBKDF2 derivation, so even weak passphrases can be used safely (at the cost of roughly a hundred extra milliseconds per encryption); old unprefixed ciphertexts are still decrypted with direct SHA-256 derivation, and that path is **suitable only for high-entropy keys** (such as `random_hex(32)` or keys injected from configuration).
- Private / public key PEM accepts both PKCS#8 / PKCS#1 (public keys as SPKI / PKCS#1), for interoperability with tools such as OpenSSL.
- This package handles only data encryption and signing; HTTPS certificate loading and the TLS handshake are provided by the runtime through `--cert` / `--key` (see 11.2.2), and Rip source never touches TLS details beyond the private key.

### 11.7 XML (`std/xml`)

```rp
use std::xml;

xml.parse(text: string) -> result<xml.node, string>
xml.stringify(node: xml.node) -> string
xml.pretty(node: xml.node) -> string
xml.name(node: xml.node) -> string
xml.text(node: xml.node) -> string
xml.attrs(node: xml.node) -> map<string, string>
xml.children(node: xml.node) -> vec<xml.node>
xml.find(node: xml.node, name: string) -> option<xml.node>
xml.find_all(node: xml.node, name: string) -> vec<xml.node>
xml.escape(text: string) -> string
xml.unescape(text: string) -> string
```

Rules:

- `xml.node` is a runtime type; its fields are read through the accessor functions above, not directly via `.name` / `.attrs`.
- Parsing supports the XML declaration, comments, DOCTYPE, CDATA, self-closing tags, named entities (`&amp;` `&lt;` `&gt;` `&quot;` `&apos;`), and `&#NN;` / `&#xNN;`.
- Mismatched tags, unquoted attribute values, an empty document, or content after the root node return `err(...)`.
- `xml.text` returns the element's direct text (with leading and trailing whitespace trimmed) without concatenating child text; `find` / `find_all` search depth-first by tag name.
- `xml.stringify` emits compact single-line output, and `xml.pretty` emits multi-line output indented with two spaces.

### 11.8 Encoding (`std/encode`)

```rp
use std::encode;

encode.url_encode(text: string) -> string
encode.url_decode(text: string) -> result<string, string>
encode.html_escape(text: string) -> string
encode.html_unescape(text: string) -> string
encode.csv_encode(rows: vec<vec<string>>) -> string
encode.csv_parse(text: string) -> vec<vec<string>>
encode.query_parse(text: string) -> map<string, string>
encode.query_stringify(query: map<string, string>) -> string
```

Rules:

- `url_encode` uses RFC 3986 percent-encoding, leaving only `A-Z a-z 0-9 - _ . ~` unescaped; `url_decode` recognizes `%XX` and `+` (space), and invalid escapes return `err(...)`.
- HTML escaping shares the same rules as `xml.escape` (`& < > " '`).
- CSV uses `\r\n` line endings by default; fields containing commas, quotes, or newlines are quoted automatically and `"` is written as `""`; `csv_parse` supports quoted fields, escaped quotes, and CRLF/LF.
- `query_parse` parses `a=1&b=2` into a `map<string, string>`, with later values overwriting earlier ones for the same key; `query_stringify` outputs keys in sorted order with URL encoding applied.

### 11.9 TCP / UDP (`std/net`)

```rp
use std::net;

net.tcp_connect(host: string, port: int, timeout_ms: int) -> result<net.conn, string>
net.tcp_send(conn: net.conn, data: string) -> result<int, string>
net.tcp_recv(conn: net.conn, max_bytes: int, timeout_ms: int) -> result<string, string>
net.tcp_recv_line(conn: net.conn, max_bytes: int, timeout_ms: int) -> result<string, string>
net.tcp_close(conn: net.conn) -> result<bool, string>

net.tcp_listen(host: string, port: int) -> result<net.listener, string>
net.tcp_accept(listener: net.listener, timeout_ms: int) -> result<net.conn, string>
net.tcp_listener_close(listener: net.listener) -> result<bool, string>

net.udp_bind(host: string, port: int) -> result<net.socket, string>
net.udp_send(socket: net.socket, host: string, port: int, data: string) -> result<int, string>
net.udp_recv(socket: net.socket, max_bytes: int, timeout_ms: int) -> result<net.packet, string>
net.udp_close(socket: net.socket) -> result<bool, string>
```

Rules:

- `timeout_ms` of `0` means wait indefinitely; `tcp_accept` / `udp_recv` return `err("timeout")` on timeout, and `err("connection closed")` when the peer closes normally.
- `net.conn`, `net.listener`, and `net.socket` are opaque runtime handles that can be passed only to functions of this package; using them after closing returns `err(...)`.
- `tcp_recv_line` reads up to `\n` (stripping a trailing `\r`) and returns a truncated result when `max_bytes` is exceeded; `tcp_recv` reads at most `max_bytes` per call.
- `net.packet` fields: `data`, `host`, `port`.
- This package is a synchronous blocking API; on the server side, `task.spawn` can move the accept loop into a separate thread.

### 11.10 WebSocket (`std/ws`)

```rp
use std::ws;

ws.serve(port: int, handler: func(ws.conn) -> void) -> void
ws.connect(url: string, timeout_ms: int) -> result<ws.conn, string>
ws.send(conn: ws.conn, text: string) -> result<int, string>
ws.recv(conn: ws.conn, timeout_ms: int) -> result<string, string>
ws.ping(conn: ws.conn) -> result<bool, string>
ws.close(conn: ws.conn) -> result<bool, string>
```

Rules:

- `ws.serve` listens on `127.0.0.1` (or `--host`) and completes the handshake; `handler` is called once per connection and the connection is closed after the handler returns; inside the handler, `ws.recv` / `ws.send` provide bidirectional communication.
- **Handshake safety**: handshake reads and writes time out after 10 seconds, so slow connections cannot occupy the service; when a request carries `Origin` (initiated by a browser) and `Origin` is not same-origin with `Host`, a `403` is returned, preventing cross-site WebSocket hijacking; non-browser clients (without `Origin`) are unaffected.
- Clients use `ws.connect(url, timeout_ms)` and support both `ws://` and `wss://`: `wss://` completes the TLS handshake with rustls plus the system root certificate store (verifying the certificate chain and host name), with port 443 by default (80 for `ws://`); server-side `ws.serve` still listens only in plaintext, and HTTPS sites are terminated by the `[server]` TLS configuration.
- **Mask validation**: received frames are validated by role per RFC 6455—the server rejects unmasked frames and the client rejects masked frames, and a violation is answered with `close 1002` (protocol error) followed by disconnection; spec-compliant browsers and standard library peers are unaffected.
- Only text frames (UTF-8) are handled; binary frames are decoded as text; `ping` is answered with `pong` automatically, and after receiving `close`, `recv` returns `err("connection closed")`.
- The per-frame limit is 16 MiB; `ws.conn` is an opaque handle, and further operations after closing return `err(...)`.

### 11.11 Redis and Cluster Coordination (`std/redis`)

```rp
use std::redis;

redis.connect(url: string) -> result<redis.conn, string>
redis.lock(conn: redis.conn, key: string, ttl_ms: int) -> result<option<string>, string>
redis.unlock(conn: redis.conn, key: string, token: string) -> result<bool, string>
redis.rate_limit(conn: redis.conn, key: string, limit: int, window_ms: int)
    -> result<bool, string>

// redis.conn methods
conn.ping() -> result<string, string>
conn.get(key) -> result<option<string>, string>
conn.set(key, value) -> result<bool, string>
conn.setex(key, seconds, value) -> result<bool, string>
conn.setnx(key, value) -> result<bool, string>
conn.del(key) -> result<int, string>
conn.exists(key) -> result<bool, string>
conn.expire(key, seconds) -> result<bool, string>
conn.incr(key) / decr(key) -> result<int, string>
conn.hget(key, field) -> result<option<string>, string>
conn.hset(key, field, value) -> result<bool, string>
conn.hdel(key, field) -> result<int, string>
conn.lpush(key, value) / rpush(key, value) -> result<int, string>
conn.lpop(key) / rpop(key) -> result<option<string>, string>
conn.llen(key) -> result<int, string>
conn.eval(script, keys: vec<string>, args: vec<string>) -> result<any, string>
conn.close() -> void
```

Rules:

- URLs accept only `redis://` and `rediss://`, in the form `redis://[user:password@]host[:port][/db]`; `rediss://` performs TLS verification with the system root certificates.
- Connections belong to the current interpreter (thread) and are not shared across threads; the connect timeout is 5 seconds and the read/write timeout is 10 seconds, and a timeout returns `err(...)` rather than blocking the request thread.
- `redis.connect` transparently reuses a process-level idle connection pool by URL: when connecting to the same URL again, an idle connection is lent out first (validated with `PING` before lending, and expired or broken connections are discarded and the search continues); clean connections are returned to the pool by `conn.close()` or at task end, while connections with failed reads/writes or unread replies are discarded. Each URL caches at most 8 idle connections, and connections idle beyond 300 seconds are evicted on the next lend; lending and returning never block and do not limit the total number of outstanding lends.
- `redis.lock` acquires a lease with `SET key token NX PX ttl`: success returns `some(token)`, and a lease held by someone else returns `none`; `redis.unlock` compares the token in Lua and then deletes, avoiding deleting another party's lease by mistake. The lease expires automatically, so a crash of the holder never causes a permanent deadlock.
- `redis.rate_limit` is a fixed-window counter: the first `limit` calls within the window return `true`, and any beyond that return `false`.
- Cluster scheduled tasks: with the address in `[cluster] redis` of `rip.toml` and `task_lock = true` enabling leader election, each `#[interval]` task acquires the `rip:task:<function name>` lease before every trigger, and only the instance that acquires it executes. The lease duration is twice the interval (at least 10 seconds); when Redis is unavailable it degrades to local execution so that tasks are not interrupted.
- Sessions, cache, distributed locks, and cluster rate limiting all build on this foundation; the runtime keeps no cross-instance state in the local process.

### 11.12 Collection Utilities, CLI, Process Cache, Persistent Queue, and Database Relations

**`std/str` / `std/vec` / `std/map`**: side-effect-free functional interfaces; when in-place modification is needed, still use the built-in methods (`.push` / `.reverse` / `.set`).

```rp
std/str: split(text, sep) / lines(text) / join(items, sep)
         trim / trim_start / trim_end / lower / upper
         contains / starts_with / ends_with
         replace(text, from, to) / slice(text, start, end)
         repeat(text, n) / pad_start(text, width, pad) / pad_end(...)
         index_of(text, sub) / len(text) / is_empty(text)
         split_once(text, sep) -> option<vec<string>>

std/vec: len / is_empty / contains / index_of
         first / last              -> option<any>
         sum(items)                -> int (saturating arithmetic)
         range(n) / range_between(start, end) -> vec<int>

std/map: keys / values / len / has / get
         merge(a, b) / without(m, key)
```

- When the separator of `split` is the empty string, it splits by character; `slice` counts Unicode characters and clamps out-of-range values automatically.
- When the result of `repeat` exceeds 32 MiB, an error is returned, preventing a single call from exhausting memory.
- `merge` returns a new map, with `b` overriding `a`; none of the collection functions operate in place.

**`std/cli`**:

```rp
cli.args() -> vec<string>                     // raw arguments (excluding the program name)
cli.command() -> string                       // the first positional argument
cli.positionals() -> vec<string>              // positional arguments with all options removed
cli.flag(name) -> bool                        // --name / --name=... / -n
cli.option(name, default) -> string           // falls back to the default value
```

**`std/cache`**: a process-level TTL + LRU cache shared across worker threads, storing only `string`.

```rp
cache.set(key, value, ttl_ms) -> void         // ttl_ms = 0 means never expires
cache.get(key) -> option<string>
cache.delete(key) -> bool
cache.clear() -> void
cache.len() -> int
cache.configure(max_entries) -> int           // 16..=1_000_000; returns the effective value
cache.stats() -> map<string, int>             // hits / misses / evictions / entries
```

- LRU is implemented with "access sequence number + ordered index", so hits, writes, and evictions are all O(log n) with no full scan.
- Encode structured data with `json.encode` before storing it, and decode with `json.decode` after reading it.
- The cache does not span processes: when shared state is needed across multiple instances, use `std/redis`.

**`std/queue`**: a persistent task queue based on SQLite (WAL + `busy_timeout`; the `rip_jobs` table is created automatically).

```rp
type queue.job = [id: int, queue: string, payload: string, attempts: int];

queue.enqueue(dsn, queue, payload, delay_ms) -> result<int, string>
queue.reserve(dsn, queue, lease_ms) -> result<option<queue.job>, string>
queue.ack(dsn, id) -> result<bool, string>
queue.fail(dsn, id, message) -> result<string, string>   // returns the new state: pending / failed
queue.release_expired(dsn) -> result<int, string>
queue.stats(dsn, queue) -> result<map<string, int>, string>   // pending / running / failed
```

- The DSN syntax is the same as `std/db` (`sqlite:storage/q.db`).
- At-least-once delivery: `reserve` selects a runnable task within a transaction and writes a lease; after the lease expires, `release_expired` puts it back on the queue, so a process crash loses no tasks; handlers must be idempotent.
- `fail` reschedules with exponential backoff of 1s / 2s / 4s / 8s, and after 5 attempts marks it `failed` while retaining `last_error`.

**`std/db` relational queries and metrics**:

```rp
db.has_many(conn, child_table, foreign_key_column, parent_id) -> result<vec<map<string, any>>, string>
db.belongs_to(conn, parent_table, id) -> result<option<map<string, any>>, string>
db.belongs_to_by(conn, parent_table, unique_column, value) -> result<option<map<string, any>>, string>
```

**`std/db` connection methods**:

```rp
conn.execute(sql, params) -> result<int, string>
conn.execute_many(sql, rows: vec<vec<any>>) -> result<int, string>
conn.query(sql, params) -> result<vec<map<string, any>>, string>
conn.query_one(sql, params) -> result<option<map<string, any>>, string>
conn.row_json(sql, params) -> result<string, string>
conn.last_insert_id() -> int
conn.begin() / conn.commit() / conn.rollback() -> result<int, string>
conn.close() -> void
```

- `execute_many` binds multiple parameter sets (one row each) to a single statement: outside an explicit transaction it wraps the batch in a transaction automatically, rolling the whole batch back and returning an error if any row fails, and returning the sum of affected rows on full success. Import-style scenarios should use it instead of calling `execute` one by one in a loop.
- `row_json` is the fast path for JSON APIs: it takes the first row of the query result and serializes it directly into a JSON object string, skipping construction of the `map` value tree; with no result row it returns `Err("row not found")`. The key order and values are byte-for-byte identical to `json.encode(query_one(...))` (keys in lexicographic order), so the response body can be written directly into `http.response`'s `body`.
- Calling `execute_many` inside a transaction already opened by `.begin()` does not nest another transaction; the boundary is still decided by the caller's `commit` / `rollback`.
- Table and column names go through identifier validation and are quoted per dialect, so SQL injection is not possible; condition values still use `?` binding.
- Every `execute` / `query` increments `rip_db_queries_total`, `rip_db_errors_total`, and `rip_db_duration_ms_total`; cache hits increment `rip_cache_hits_total` / `rip_cache_misses_total` / `rip_cache_evictions_total`, all visible at the `[server] metrics_path` endpoint.
- Postgres DSNs support `sslmode` (as the URL query parameter `?sslmode=require` or the keyword form `sslmode=require`): `disable` (default, plaintext), `require` (encrypted only, no server certificate verification), `verify-ca` / `verify-full` (verify the certificate chain and host name with the system root certificate store; rustls always verifies the host name, so `verify-ca` is stricter than libpq). `allow` / `prefer` rely on falling back to a plaintext reconnect and are not supported, returning an error.
- MSSQL URL-form DSNs support the `encrypt` / `trustcert` parameters (`?encrypt=true&trustcert=false` or `;encrypt=false`; key names are case-insensitive and ignore underscores, and `TrustServerCertificate` is also accepted): `encrypt` defaults to `true` (TLS enforced), and `false` disables encryption; `trustcert` defaults to `true` (trust the server certificate directly, consistent with historical behavior), and `false` switches to verification with the system root certificate store. ADO-string forms (`Server=...;Encrypt=...;TrustServerCertificate=...`) follow the semantics of the connection string itself.

### 11.13 Observability (Tracing, Logs, Metrics)

**Tracing**: configuring `[server] otlp_endpoint` enables OTLP/HTTP JSON export.

- Each HTTP request produces one server span: its name is `method route-pattern`, and its attributes include `http.method` / `http.target` / `http.route` / `http.status_code` / `http.client_ip` / `duration_ms`; `5xx` is recorded as an error status.
- Database `execute` / `query` calls appear as child spans under the current request span; the SSE span covers the entire push process.
- The trace id and span id of an inbound `traceparent` are reused (so distributed traces are not broken), and a new trace id is generated when there is no upstream; outbound `http.fetch` / `http.download` automatically add a new `traceparent`.
- Sampling is decided deterministically by trace id, so all spans of the same trace get the same result; the queue limit is 4096, and when full the oldest are dropped and counted in `rip_trace_spans_dropped_total`.
- A failed export puts the whole batch back on the queue for retry; when the Collector is unavailable for a long time, the queue limit still caps memory usage.

**Logs**: the `json` format of `std/log` automatically includes `request_id` and `trace_id` within a request context, so it can be aligned with tracing directly.

- After `log.set_file(path)`, calling `log.set_rotate(max_bytes, keep)` enables rotation: when a single file exceeds `max_bytes`, files are renamed to `.1`, `.2`, … in order, `keep` files are retained, and the original file is reopened and appended to.
- Rotation state belongs to the current interpreter; each HTTP worker thread holds its own copy, and when writing to the same path each thread rotates independently (use a Collector / log agent when centralized collection into a single file is needed).
- Access logs (`[server] access_log`, enabled by default) are appended to memory by the request thread only, and a background thread writes whole batches to stdout every 100ms: a slow stdout never holds requests up; the buffer limit is 16MB, and once exceeded, new logs are dropped and counted in `rip_access_log_dropped_total`.

**Metrics**: `[server] metrics_path` exposes HTTP, rate limiting, database, cache, and tracing counters; see 11.12 and 10.13 for the naming.

**Engineering tooling**:

- `rip test --coverage [--coverage-min N]`: statement line coverage; both the interpreter and bytecode execution paths are instrumented, and when disabled the bytecode gains no instructions.
- `rip bench`: runs `#[bench]` functions and outputs `min` / `mean` / `p95` / `max` and ops/s.
- `rip fmt [file|dir] [--check] [--stdout]`: unifies indentation and blank lines; `--check` suits CI gates.
- `rip new <name> [--template http|cli|empty]`: generates `rip.toml`, the entry file, `.gitignore`, and the CI templates `.github/workflows/ci.yml` and `.gitlab-ci.yml`; the project name also serves as the package name. CI runs `rip check` / `rip test` / `rip build --no-archive` in order and, when dependencies are present, uncommenting `rip lock --check` and `rip audit --deny high`.
- `rip doc [file] [-o <dir>] [--json]`: generates single-file HTML API documentation from the AST and `///` comments in source files (types, enums, interfaces, global functions, method ownership, and signatures); `--json` outputs the same model for integration with other documentation sites.
- `rip search <keyword> [--registry URL] [--json]`: searches package sources (package name, version, description); both local directories and HTTP package sources are supported.
- `rip info <package> [--registry URL] [--json]`: lists all versions, dependencies, and checksums of a package.
- `rip audit [--deny LEVEL] [--json] [--licenses]`: dependency locking, security advisory, and license auditing; see Section 5 of 10.10.
- `rip profile [file] [--top N] [--json]`: runs the program and outputs per-function call counts, total time, self time, and percentage; covers closures and built-in module calls, and merges calls from multiple threads.
- `rip dap [--port N]`: starts the DAP debug adapter; it supports breakpoints, stepping (next / stepIn / stepOut), pause, call stack, Locals / Globals, variable name evaluation, and disconnect. Program output is relayed through DAP `output` events and does not pollute the protocol; `--port` is for services with heavy output.
- A debugging session disables the bytecode fast path and checks breakpoints statement by statement; breakpoint line numbers are counted against the entry source file.
- Editor integration lives in `editors/vscode/`: `.rp` syntax highlighting (TextMate), LSP (`rip lsp`), and DAP (`rip dap`) debug configuration; the `rip.executable` setting specifies the toolchain path, and debugging defaults to `program = "${file}"`.

### 11.14 Time Zones, Unicode, ZIP, and Mail

**Time zones (`std/time`)**: an IANA time zone database (with daylight saving rules) is built in and does not depend on the system tzdata.

```rp
time.zone_names() -> vec<string>
time.local_zone() -> string
time.offset(timestamp_ms: int, zone: string) -> result<int, string>
time.format_zone(timestamp_ms: int, pattern: string, zone: string) -> result<string, string>
time.parse_zone(text: string, pattern: string, zone: string) -> result<int, string>
```

- `zone` is an IANA name (`Asia/Shanghai`, `America/New_York`) and is case-insensitive; an unknown zone returns `err`.
- `offset` returns the offset in seconds from UTC at that instant, with daylight saving included (New York in July is `-14400`).
- The format of `format_zone` / `parse_zone` matches `time.format` (strftime style), and `%:z` outputs `+08:00`.
- For repeated local times caused by a daylight saving fall-back, the **earlier** occurrence is used; times skipped by the spring forward return `err`.
- `time.format` / `time.parse` remain shortcuts for the "process local time zone"; always use `*_zone` when determinism is needed.

**Unicode (`std/str`)**:

```rp
str.normalize(text: string, form: string) -> result<string, string>  // nfc / nfd / nfkc / nfkd
str.graphemes(text: string) -> vec<string>
str.grapheme_len(text: string) -> int
str.width(text: string) -> int
```

- `len` / `slice` / `at` count by **code point**; `grapheme_len` / `graphemes` count by **grapheme cluster** (emoji and combining accents are not split apart).
- `width` is the terminal display width: East Asian full-width characters count as 2 and combining marks count as 0, for aligning tables and logs.
- The `nfkc` form of `normalize` is commonly used to fold full-width characters to half-width before searching / deduplicating; an unknown form returns `err`.
- Same-named methods: `s.normalize(form)` / `s.graphemes()` / `s.grapheme_len()` / `s.width()`.

**ZIP (`std/zip`)**:

```rp
type zip.entry = [
    name: string,
    size: int,
    compressed_size: int,
    crc32: int,
    is_dir: bool
];

zip.create(path: string, files: map<string, string>) -> result<int, string>
zip.add(path: string, files: map<string, string>) -> result<int, string>
zip.list(path: string) -> result<vec<zip.entry>, string>
zip.extract(path: string, dir: string) -> result<int, string>
zip.read_text(path: string, entry: string) -> result<string, string>
zip.read_base64(path: string, entry: string) -> result<string, string>
zip.contains(path: string, entry: string) -> result<bool, string>
```

- The compression algorithm is fixed to deflate; entry names are always separated by `/` and written as UTF-8.
- Entry names must be relative paths: absolute paths, drive letters, and `..` are always rejected (zip-slip protection).
- Extraction and read limits: 256 MiB per entry and 2 GiB per call; exceeding them returns `err` (zip-bomb protection).
- `read_text` requires the content to be valid UTF-8; for binary content use `read_base64` to retrieve a base64 string.
- `add` appends to an existing archive; when the file does not exist it is equivalent to `create`. For duplicate entry names, the last write wins.

**Mail (`std/mail`)**: a synchronous SMTP client that reuses the runtime's bundled rustls for TLS.

```rp
type mail.attachment = [
    filename: string,
    path: string,
    content_base64: string,
    content_type: string
];

type mail.config = [
    host: string, port: int, tls: string,
    username: string, password: string,
    from: string, from_name: string,
    to: vec<string>, cc: vec<string>, bcc: vec<string>, reply_to: string,
    subject: string, text: string, html: string,
    attachments: vec<mail.attachment>,
    timeout_ms: int
];

mail.config() -> mail.config          // localhost:25 / tls "none" / 30s
mail.attachment() -> mail.attachment
mail.send(config: mail.config) -> result<string, string>
```

```rp
use std::mail;

func send_daily() -> result<string, string> {
    let mut cfg: mail.config = mail.config();
    cfg.host = "smtp.example.com";
    cfg.port = 587;
    cfg.tls = "starttls";
    cfg.username = "bot@example.com";
    cfg.password = "secret";
    cfg.from = "bot@example.com";
    cfg.to = ["ops@example.com"];
    cfg.subject = "Daily report";
    cfg.text = "Plain text body";
    cfg.html = "<p>HTML body</p>";
    return mail.send(cfg);
}
```

- Start from the defaults returned by `mail.config()` and then override field by field; field types are checked by the compiler.
- `tls`: `"none"` (plaintext) / `"starttls"` (587, default port as configured) / `"ssl"` (465, implicit TLS).
- An empty `username` means no authentication; `password` is used only when `username` is set.
- When both `text` and `html` are present, a `multipart/alternative` is sent; with attachments the outer part is `multipart/mixed`.
- Attachment content: a non-empty `content_base64` takes precedence, otherwise `path` is read; `filename` is required.
- Headers are encoded per RFC 2047 and the body is UTF-8; `timeout_ms` limits a single socket operation, and `0` means no timeout.
- `mail.send` is a blocking call: in an HTTP handler, run it inside a background function via `task.spawn`.

### 11.15 Reflection and Dynamic Dispatch (`std/reflect`)

Static types are preferred. Reflection is used only in scenarios that genuinely need to read struct fields at runtime or call methods by name, such as logging, serialization, and plugin bridging.

```rp
reflect.type_name(value: any) -> string
reflect.kind(value: any) -> string
reflect.fields(value: any) -> vec<string>
reflect.has(value: any, name: string) -> bool
reflect.get(value: any, name: string) -> option<any>
reflect.variant(value: any) -> option<string>
reflect.invoke(value: any, method: string, args: vec<any>) -> result<any, string>
reflect.is(value: any, name: string) -> bool
```

- `type_name` returns the concrete type name: structs and enums return the declared name (such as `user` and `status`), and everything else returns the built-in name (`int`, `string`, `vec`, `map`, `option`, `result`, `func`, `none`, `null`).
- `kind` returns the category name: structs and enums are uniformly `"struct"`, and everything else matches `type_name`.
- `fields` returns visible field names: for structs in declaration order (excluding the internal `#variant`), for `map` the string keys, and for `vec` `"0"`, `"1"`, ….
- `get` supports struct fields, `map` string keys, and `vec` numeric indexes; it returns `none` when absent. `has` is the boolean form of `get`.
- `variant` returns the variant name only for enum values and `none` otherwise; fieldless variants work the same way.
- `invoke` calls a method of the same name by name (with the same semantics as `.` calls) and returns `ok(value)` on success; a missing method, mismatched argument types, or a runtime error all return `err(message)` without terminating the program.
- `is` checks whether `type_name` or `kind` equals the given name, for plugins that dispatch by type.
- Reflection does not change type checking: parameter positions accept any value (the signature in the library is `any`) and the return value is `any`, so values still require explicit conversion (see Section 12); user code does not need to and should not write `any`.

```rp
use std::reflect;

type user = [name: string, age: int];

func describe(u: user) -> string {
    return reflect.type_name(u) + " " + reflect.fields(u).to_string();
}

func trim_text(text: string) -> string {
    match reflect.invoke(text, "trim", []) {
        ok(v) => return v.as_string().unwrap_or(""),
        err(_) => return "",
    }
}
```

---

## 12. Rules for the `any` Type

- `any` **is allowed only** **in the value type of** **`map<string, any>`** and at JSON decoding boundaries (`json.decode` / `req.json`).
- Extracting a value from `any` requires explicit conversion: `.as_string()` / `.as_int()` / `.as_float()` / `.as_bool()` / `.as_vec()` / `.as_map()`, which return `option<T>`.
- `any` does not participate in compile-time type checking and loses static safety; it is an explicit escape hatch.

- Except for `map<string, any>`, **`any`** **is forbidden in any position**.

---

# Part Two: Implementation Specification

## 13. Project Structure

```text
rip/
├── Cargo.toml                    # workspace
├── crates/
│   ├── rp-lexer/                 # lexing
│   ├── rp-parser/                # parsing → AST
│   ├── rp-ast/                   # AST definitions
│   ├── rp-types/                 # type system + type checking
│   ├── rp-ownership/             # ownership / borrow / lifetime checking
│   ├── rp-runtime/               # runtime (value system, bytecode VM, tree-walking interpreter, heap, builtins, HTTP/DB/net)
│   ├── rp-core/                  # clean host (rp-runtime with interp disabled; linked by AOT artifacts)
│   ├── rp-rust-interop/          # Rust package interop
│   ├── rp-cli/                   # command-line tool rip (including AOT code generation)
│   ├── rp-lsp/                   # language server
│   └── rp-dap/                   # debug adapter
├── examples/ tests/ demo/ doc/
```

## 14. Compilation Pipeline

```text
source .rp
  ↓
[Lexer] → token stream
  ↓
[Parser] → AST
  ↓
[Type Checker] → typed AST, reports type errors
  ↓
[Ownership Checker] → borrow/lifetime/Send-Sync checks, inserts drop/move/borrow points
  ↓
┌─────────────────────┬─────────────────────┐
│ Interpreted mode    │ AOT mode            │
│ AST → Bytecode      │ AST → LLVM IR       │
│ VM execution        │ LLVM → ELF/EXE      │
└─────────────────────┴─────────────────────┘
```

**Key constraint**: interpreted mode must also **pass the full front-end checks first** (types + ownership); checking while running is not allowed. The check results are passed to both backends as intermediate information.

**Current implementation**: interpreted mode attempts to compile each function into stack-based bytecode (the scalar subset only); functions that cannot be compiled fall back entirely to the tree-walking interpreter, and both share the type-check results plus the same operation and call implementations. `RIP_NO_VM=1` forces fully interpreted execution, for behavior comparison. AOT provides two paths: `rip build --native` translates the scalar int subset into Rust and compiles it with `rustc -O` (fully standalone, with no runtime support library); `rip build --aot` translates the full syntax into Rust and statically links `rp-runtime`, and the artifact contains neither source nor the language front end.

## 15. Dependency Crates

| Purpose | crate |
| --- | --- |
| Lexing | logos (or hand-written) |
| Parsing | hand-written recursive descent (recommended) |
| LLVM backend | inkwell |
| Cranelift alternative | cranelift-codegen |
| Hash tables | hashbrown |
| HTTP | hyper / axum (internal to the runtime) |
| JSON | serde_json (internal to the runtime) |
| LSP | tower-lsp |
| Logging | tracing |
| Errors | thiserror / anyhow (internal) |
| Time zones | chrono-tz (IANA database) + iana-time-zone |
| Unicode | unicode-normalization / unicode-segmentation / unicode-width |
| ZIP | zip (deflate) |
| Mail | lettre (SMTP + rustls) |

## 16. Hard Implementation Constraints

1. **Single canonical form**: the same semantics allow only one syntax; aliases, sugar, and second spellings are not allowed.
1. **No inference**: all types are explicit; the only exception is `for` / `match` destructuring.
1. **Interpreted mode checks before executing**: checking while running is not allowed.
1. **Reference counting + cycle collection**: containers are released immediately with `Rc`, and cycles where closures and containers reference each other are cleaned up by the runtime's cycle collector; a full tracing GC is not provided. The heap limit is controlled by `[server] max_heap_mb`, and requests that exceed it end with a runtime error.
1. **No** **`unsafe`** **exposure**: no raw pointers and no unsafe at the user level.
1. **No eval**: executing string code at runtime is forbidden.
1. **`ary`** **fields are closed**: fields cannot be added at runtime.
1. **Indexing is forbidden**: all access goes through the dot.
1. **HTTP format is decided by** **`return`**: exactly one of `body` / `json` / `sse`, and the framework does not enforce a format.
1. **Rust packages are called only through Rip syntax**: Rust-specific syntax is not exposed, and the type mapping is fixed.
1. **Naming convention checks**: violations are warnings, escalated to errors by `--deny-warnings`.
1. **No shadowing**: variable names are unique within the same scope.

---

# Part Three: Test Cases

## 17. `hello.rp`

```rp
mod examples;

use std::dev;

func main() -> void {
    let msg: string = "Hello, Rip!";
    dev.log(msg);
    print(msg);
}
```

Expected output: `Hello, Rip!`

## 18. `types.rp`

```rp
mod examples;

use std::dev;

type address = [city: string, zip: string];
type user = [
    id: int,
    name: string,
    age: int,
    flags: u8,
    perms: u16,
    color: u32,
    tags: vec<string>,
    scores: map<string, int>,
    address: address
];

const MAX_AGE: int = 150;
static APP_NAME: string = "types_demo";

func main() -> void {
    let a: int = 42;
    let b: int = 42;
    let c: u8 = 0b1010;
    let d: u16 = 0o755;
    let e: u32 = 0xFF;
    let sum: int = a + b + c.to_int() + d.to_int() + e.to_int();
    dev.log("sum = " + sum.to_string());

    let s: string = "hello";
    let ch: string = 'a';
    dev.log(s + ch);

    let u: user = [
        id = 1,
        name = "Zhang San",
        age = 25,
        flags = 0b1010,
        perms = 0o755,
        color = 0xFF00FF,
        tags = ["a", "b"],
        scores = ["math" = 90, "english" = 85],
        address = [city = "Beijing", zip = "100000"]
    ];

    dev.log("u.name = " + u.name);
    dev.log("u.address.city = " + u.address.city);
    dev.log("u.color.to_int = " + u.color.to_int().to_string());
}
```

## 19. `ownership.rp`

```rp
mod examples;

use std::dev;

type user = [id: int, name: string];

func greet(read u: user) -> string {
    return "Hi " + u.name;
}

func rename(write u: user, n: string) -> void {
    u.name = n;
}

func consume(u: user) -> int {
    return u.id;
}

func main() -> void {
    let mut u: user = [id = 1, name = "Zhang San"];

    let msg: string = greet(u);
    dev.log(msg);

    rename(u, "Li Si");
    dev.log("renamed: " + u.name);

    let id: int = consume(u);
    dev.log("id = " + id.to_string());

    // dev.log(u.name);   // compile error: use after move
}
```

## 20. `copy_vs_move.rp`

```rp
mod examples;

use std::dev;

func take_int(n: int) -> int { return n * 2; }
func take_string(s: string) -> int { return s.len(); }

func main() -> void {
    let a: int = 10;
    let b: int = take_int(a);
    dev.log("a still = " + a.to_string());     // Copy: a is still usable

    let s: string = "hello";
    let len: int = take_string(s);
    dev.log("len = " + len.to_string());
    // dev.log(s);                              // Move: s is no longer valid
}
```

## 21. `data_structures.rp`

```rp
mod examples;

use std::dev;

func main() -> void {
    // vec: homogeneous declared up front + shorthand
    let mut nums: vec<int> = [1, 4, 3];
    nums.sort(func(a: int, b: int) -> int { return a - b; });
    nums.push(5);
    dev.log("nums.0 = " + nums.0.to_string());
    dev.log("nums.at(1) = " + nums.at(1).unwrap_or(0).to_string());

    // vec: homogeneous declared up front + explicit
    let nums2: vec<int> = [:int = 1, :int = 2];

    // vec: heterogeneous declared up front
    let pair: vec<string, int> = ["a", 1];
    dev.log("pair.0 = " + pair.0);

    // vec: heterogeneous defined inline
    let mixed: vec = [:int = 1, :string = "a", :bool = true];
    dev.log("mixed.len = " + mixed.len().to_string());

    // vec: reverse order
    let mut xs: vec<int> = [1, 2, 3];
    xs.reverse();
    dev.log("xs.0 = " + xs.0.to_string());     // 3

    // map
    let mut scores: map<string, int> = [];
    scores.set("Zhang San", 90);
    scores.set("Li Si", 85);
    dev.log("scores.len = " + scores.len().to_string());
    dev.log("Zhang San = " + scores.get("Zhang San").unwrap_or(0).to_string());

    for k in scores.keys() {
        dev.log(k + " = " + scores.get(k).unwrap_or(0).to_string());
    }
}
```

## 22. `control.rp`

```rp
mod examples;

use std::dev;

func main() -> void {
    let score: int = 85;
    if score >= 90 {
        dev.log("A");
    } else if score >= 80 {
        dev.log("B");
    } else {
        dev.log("C");
    }

    let mut i: int = 0;
    while i < 3 {
        dev.log("while i = " + i.to_string());
        i = i + 1;
    }

    let mut j: int = 0;
    loop {
        if j >= 3 { break; }
        dev.log("loop j = " + j.to_string());
        j = j + 1;
    }

    let tags: vec<string> = ["a", "b", "c"];
    for t in tags {
        dev.log("tag = " + t);
    }

    cache.set("greeting", "hi", 60);
    while let some(value) = cache.get("greeting") => {
        dev.log("while let value = " + value);
        cache.delete("greeting");
    }

    let mut k: int = 0;
    while k < 10 {
        k = k + 1;
        if k == 2 { continue; }
        if k == 5 { break; }
        dev.log("k = " + k.to_string());
    }

    let opt: option<int> = some(42);
    match opt {
        some(v) => { dev.log("some: " + v.to_string()); }
        none => { dev.log("none"); }
    }
}
```

## 23. `http_server.rp`

```rp
mod examples;

use std::http;
use std::json;
use std::dev;
use std::time;

type user = [id: int, name: string, age: int, tags: vec<string>, created_at: string];

func main() -> void {
    let mut users: map<string, user> = [
        "1" = [id = 1, name = "Zhang San", age = 25,
               tags = ["admin"], created_at = time.now().format("YYYY-MM-DD")]
    ];

    dev.log("listening on :8080");

    http.serve(8080, http.router([
        http.get("/", func(req: http.request) -> http.response {
            return http.response[status = 200, json = [message = "Hello, Rip!"]];
        }),

        http.get("/text", func(req: http.request) -> http.response {
            return http.response[
                status = 200,
                headers = ["Content-Type" = "text/plain; charset=utf-8"],
                body = "Hello, Rip!"
            ];
        }),

        http.get("/html", func(req: http.request) -> http.response {
            let html: string = "<h1>Hello, Rip!</h1>";
            return http.response[
                status = 200,
                headers = ["Content-Type" = "text/html; charset=utf-8"],
                body = html
            ];
        }),

        http.get("/users/:id", func(req: http.request) -> http.response {
            let id: string = req.params.get("id").unwrap_or("0");
            match users.get(id) {
                some(u) => { return http.response[status = 200, json = u]; }
                none => { return http.response[status = 404, json = [error = "not found"]]; }
            }
        })
    ]));
}
```

Verification:

- `curl localhost:8080/` returns JSON with `Content-Type: application/json`
- `curl localhost:8080/text` returns plain text with `Content-Type: text/plain`
- `curl localhost:8080/html` returns HTML with `Content-Type: text/html`

## 24. `rust_interop.rp`

```rp
mod examples;

use rust::serde_json as json;
use std::dev;

type user = [id: int, name: string];

func main() -> void {
    let u: user = [id = 1, name = "Zhang San"];
    let encoded: result<string, string> = json.to_string(u);
    match encoded {
        ok(s) => { dev.log("json = " + s); }
        err(e) => { dev.log("err: " + e); }
    }
}
```

## 25. `llm_demo.rp`

```rp
mod examples;

use std::llm;
use std::dev;
use std::sys as sys;

func main() -> void {
    let client: llm.client = llm.new_client([
        provider = "openai",
        base_url = "https://api.openai.com/v1",
        api_key = sys.env_get("OPENAI_KEY").unwrap_or(""),
        timeout_ms = 60000
    ]);

    let msgs: vec<llm.message> = [
        [role = "user", content = "Introduce Rip in one sentence"]
    ];
    let resp: result<llm.response, string> = client.chat(msgs, [
        model = "gpt-4o-mini",
        temperature = 0.7,
        max_tokens = 256,
        top_p = 1.0,
        stream = false
    ]);
    match resp {
        ok(r) => { dev.log(r.content); }
        err(e) => { dev.log("err: " + e); }
    }
}
```

---

# Part Four: Delivery Checklist

Implementation is complete when all of the following hold:

1. `rip run hello.rp` prints `Hello, Rip!`
1. `rip run types.rp` passes all types (`int` / `i8`–`i64` / `u8`–`u64`, with quote forms distinguished)
1. `rip run ownership.rp` passes; uncommenting the line reports use-after-move
1. `rip run copy_vs_move.rp` passes
1. `rip run data_structures.rp` passes (the three `vec` spellings, `map` add/delete/lookup, `.sort` / `.reverse`)
1. `rip run control.rp` passes all control flow
1. `rip dev http_server.rp`: `/` returns JSON, `/text` returns plain text, `/html` returns HTML
1. `rip run rust_interop.rp` passes, with `use rust::serde_json;` called exactly like an ordinary package
1. `rip build http_server.rp` produces `dist/`: a single ELF/EXE (statically linking the runtime and Rust dependencies) + a generated README (application name, version number, how to start) + the runtime data directories declared by `[build] copy`; the whole directory runs on a machine without `rip`
1. All `.rp` files comply with: no indexing, no inference, no class, no shadowing, no `std/tpl`, and no Rust syntax leakage; loops use only `loop` / `while` / `while let` / `for ... in`, and `=>` appears only in `match` arms and `while let`
1. Naming convention checks are active, with violations reported as warnings
1. `use std::sys as sys;` aliasing works; `use rust::xxx as xxx;` aliasing works
1. `package foo;` / `import "std/http";` must be treated as syntax errors: neither word is a keyword, and no compatibility path is kept

---

## Summary in One Paragraph

**Rip (`.rp`, tool `rip`, manifest `rip.toml`) is a strongly typed, no inference, class-free, no-GC, Rust-ownership-semantics server/Web language with Go's structure and PHP's ease of use. Its three data structures are `ary` (fixed fields), `vec` (ordered indexed sequence), and `map` (dynamic key-value dictionary); variables are `let` / `let mut` / `const` / `static`, all types are explicit, shadowing is forbidden, and naming is `snake_case` (constants `SCREAMING_SNAKE_CASE`); integers are `int` (= i64) / `float` (= f64) / fixed-width `i8`–`i64` / `u8`–`u64`; parameter modifiers are `read` / `write`, Copy types copy and Move types transfer by default; the HTTP return format is decided by `return`, choosing exactly one of `body` / `json` / `sse`; Rust packages are imported with `use rust::xxx;` and called entirely with Rip syntax. The implementation proceeds Lexer → Parser → type checking → ownership checking → dual backends (bytecode VM / LLVM AOT), with interpreted and compiled modes sharing the same front-end checks; completion means the 12-item delivery checklist in Part Four passes item by item.**
