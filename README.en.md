# Rip

[中文](README.md) | **English**

Rip is a strongly typed language for server-side and web development: no type inference, no `class`, reference counting with cycle collection, and Rust-style ownership semantics. A single source file can be interpreted directly or compiled into a native binary that needs no runtime.

[![version](https://img.shields.io/badge/version-0.1.0-blue.svg)](#)
[![platform](https://img.shields.io/badge/platform-windows%20x64%20%7C%20linux-lightgrey.svg)](#install)
[![runtime](https://img.shields.io/badge/runtime-interpreter%20%2B%20AOT-green.svg)](#two-execution-modes)

---

## Overview

Rip's core principle is a **single canonical form**: one meaning, one syntax — no aliases, no sugar, no second spelling. That keeps code identical in shape across people and projects, and frees the toolchain (formatter, static checker, LSP, AOT compiler) from handling synonymous constructs.

Only two delivery forms are exposed to users:

| Form | Contents | What users need |
| --- | --- | --- |
| Runtime binary | `rip` command line plus bundled runtime libraries | Run, package, native-compile directly |
| Runtime source | Rust workspace source | Toolchain maintainers only, build with `cargo` |

## Features

**Language**

- Strong typing, zero inference: every type is written down, never guessed
- No `class`: code is organised with `struct`, `type` aliases, functions and modules
- Full ownership semantics: `move` / `copy` / borrow checking produce compile-time diagnostics
- Memory model: containers are `Rc<RefCell<…>>` and are released immediately when acyclic; cycles are handled by a per-thread cycle collector at allocation intervals
- Uniform error handling: `?` together with `ok` / `err` and `some` / `none`, consumed by `match`
- Annotation driven: `#[get]`-style route annotations, `#[interval]` scheduled tasks, `#[test]` / `#[bench]`

**Runtime**

- Two execution paths: pure scalar functions compile to stack bytecode; functions using containers, closures or pattern matching fall back to the tree-walking interpreter, with identical semantics
- Event-driven HTTP server on a thread-per-core architecture: an acceptor dispatches connections, after which reads, handler execution and writes stay inline on that thread — zero cross-thread hand-off per request
- Built-in database drivers (MySQL / PostgreSQL / SQLite / SQL Server), TLS, WebSocket, scheduled tasks, mail, compression, crypto and more
- Observability: `RIP_STAGE_TIMING` stage timings, `RIP_STAGE_CPU` per-stage thread CPU attribution, `rip profile` function sampling, `sys.heap_usage()` heap statistics

**Toolchain**

- `rip run` / `rip dev`: run source directly; `dev` adds hot reload
- `rip build`: full AOT covering the whole language; the binary contains no Rip source
- `rip test` / `rip bench` / `rip profile` / `rip check` / `rip fmt` / `rip doc`
- `rip lsp` / `rip dap`: language server and debug adapter
- Packages: `rip add` / `rip lock` / `rip pack` / `rip pub` / `rip search` / `rip audit`
- Migrations: `rip migrate up|down|status`

## Install

### Windows (prebuilt, self-contained)

The Windows x64 runtime ships as a self-contained package: compiler, linker and AOT runtime libraries are bundled, so **no** Rust / cargo / build tools are required:

1. Download `rip-0.1.0-windows-x64.zip` from [Releases](https://github.com/LeePillar/rip-lang/releases/tag/v0.1.0) and extract it
2. Run `install.cmd` inside the extracted folder — it appends that folder to the **current user PATH** (no administrator rights needed)
3. Open a new terminal and run `rip doctor`

Current session only (no system changes):

```powershell
. .\env.ps1
rip version
```

Uninstall: `.\install.ps1 -Uninstall`.

### Linux (build the runtime from source)

Only the Windows package is published today. On Linux, build the runtime from source:

```bash
# requires a local Rust toolchain (rustc + cargo)
cargo build --release -p rp-cli      # produces target/release/rip
./target/release/rip doctor
```

`rip build` invokes the local rustc for native compilation. To distribute a standalone runtime with a bundled compiler like the Windows package, mirror the layout produced by `scripts/bundle-toolchain.ps1`: place `toolchain/` (rustc and target libraries), `lib/rip/deps/` (AOT runtime library `rp-core` and its dependencies), `lib/rip/build/` and `lib/rip/native/` next to the `rip` executable; the runtime locates them via `RIP_TOOLCHAIN` and `RIP_AOT_DEPS`.

## Quick start

```bash
rip new demo --template http     # scaffold a project (http / cli / empty)
cd demo
rip run main.rp                  # runtime + source: interpreted
rip build main.rp -o dist        # AOT: native binary in dist\
```

A minimal Rip program:

```rip
mod hello;

use std::dev;

func main() -> void {
    let msg: string = "Hello, Rip!";
    dev.log(msg);
    print(msg);
}
```

An HTTP service (routes registered with `#[get]`):

```rip
mod app;

use std::http;

#[get("/")]
func index(req: http.request) -> http.response {
    return http.response[status = 200, json = [message = "Hello, Rip!"]];
}

func main() -> void {
    http.serve("0.0.0.0", 8080);
}
```

More samples live in `examples/` (ownership, generics, closures, async, database, HTTP, time zones, compression), and a complete application in `demo/` (a decoupled Vue 3 frontend and Rip backend).

## Two execution modes

| Mode | Command | Target machine needs | Use case |
| --- | --- | --- | --- |
| Runtime + source | `rip run` / `rip dev` | `rip` installed | Development, hot updates in operations |
| Native binary (AOT) | `rip build` | No runtime, no source | Delivery and deployment |

Both modes share semantics, diagnostics and exit codes; `rip test --diff <dir>` compares their output file by file.

`rip build` links `rp-core` with the `interp` feature disabled, so the binary carries neither the AST interpreter nor the bytecode VM. If `rp-core` is unavailable, it falls back to the full `rp-runtime` and prints a notice.

## Repository layout

```text
rip/
├── crates/
│   ├── rp-lexer/           lexer
│   ├── rp-ast/             AST definitions
│   ├── rp-parser/          parser
│   ├── rp-types/           type system
│   ├── rp-ownership/       ownership and borrow checking
│   ├── rp-core/            value model and runtime (AOT link target)
│   ├── rp-runtime/         interpreter, bytecode VM, HTTP server, standard library
│   ├── rp-cli/             rip command line
│   ├── rp-lsp/             language server
│   ├── rp-dap/             debug adapter
│   └── rp-rust-interop/    Rust native extension bridge
├── demo/                   full example (Vue 3 frontend + Rip backend)
├── doc/                    language specification and handbook (Chinese and English)
├── examples/               language feature samples
├── scripts/                packaging scripts
└── tests/                  language-level regression cases
```

## Command line

| Command | Description |
| --- | --- |
| `rip run [file.rp]` | Run source; accepts `--host` / `--port` / `--cert` / `--key` |
| `rip dev <file.rp>` | Development server with hot reload |
| `rip build <file.rp>` | Compile to a native binary and produce `dist.zip` |
| `rip test [dir\|file]` | Run `#[test]` functions; supports `--filter` / `--jobs` / `--coverage` |
| `rip bench` / `rip profile` | Benchmarks / function-level timing samples |
| `rip check` / `rip fmt` / `rip doc` | Static checks / formatting / API docs |
| `rip new` / `rip add` / `rip lock` | Scaffolding / dependencies / dependency locking |
| `rip pack` / `rip pub` / `rip search` / `rip audit` | Package build / publish / search / audit |
| `rip migrate up\|down\|status` | Database migrations |
| `rip lsp` / `rip dap` | Language server / debug adapter |
| `rip repl` / `rip doctor` / `rip version` | REPL / environment self-check / version |

## Documentation

| Document | 中文 | English |
| --- | --- | --- |
| Language specification | [RipLang-level.md](doc/RipLang-level.md) · [HTML](doc/RipLang-level.html) | [RipLang-level.en.md](doc/RipLang-level.en.md) · [HTML](doc/RipLang-level.en.html) |
| Language handbook | [RipLang-help.md](doc/RipLang-help.md) · [HTML](doc/RipLang-help.html) | [RipLang-help.en.md](doc/RipLang-help.en.md) · [HTML](doc/RipLang-help.en.html) |
| Example conventions | [demo/README.md](demo/README.md) | [demo/README.en.md](demo/README.en.md) |

## Environment variables

```text
RIP_TOOLCHAIN        installation directory (defaults to the directory containing the rip binary)
RIP_AOT_DEPS         AOT runtime library directory (defaults to lib/rip/deps)
RIP_STAGE_TIMING=1   print per-stage timings every 20000 requests
RIP_STAGE_CPU=1      print per-stage thread CPU attribution
RIP_NO_VM=1          disable the bytecode path and force the interpreter (diagnostics)
RIP_HTTP_DEBUG       print connection and task traces
```

## Status

Current version `0.1.0`; the language specification is `doc/RipLang-level.md` (v1.3). The language and runtime are still evolving — syntax, type and standard-library changes follow the specification.
