# Rip for VS Code

Official [VS Code](https://code.visualstudio.com/) extension for the **Rip** language (`.rp`) — a statically typed, zero-inference language with Rust-style ownership.

## Features

- **Syntax highlighting** — keywords, primitive and generic types, template strings (`"""..."""` with `${}`), attributes (`#[get]`, `#[test]`, `#[interval]`), method declarations and `std::` module paths.
- **Diagnostics** — full lexer → parser → type checker → ownership analysis on every change.
- **IntelliSense** — completion (locals, functions, types, imports, builtin members), hover signatures, go-to-definition, references, document highlight.
- **Signature help** — parameter hints while typing `(` and `,` for both project functions and standard-library members.
- **Inlay hints** — parameter names shown at call sites.
- **Outline & folding** — document symbols (mod/func/type/interface/enum/const), brace-block and block-comment folding.
- **Workspace symbols** — search every `.rp` file in the workspace with `Ctrl/Cmd + T`.
- **Debugging** — DAP support via `rip dap` (line breakpoints, stepping, variables).
- **Snippets** — `func`, `main`, `type`, `interface`, `enum`, `if`, `for`, `while`, `matchopt`, `matchres`, `get`, `serve`, `test`, `interval`, `async`, `defer`, `method`.

## Requirements

The extension launches `rip lsp` and `rip dap`, so the Rip toolchain must be installed and on `PATH`:

```bash
cargo build --release -p rp-cli   # produces target/release/rip
rip --version
```

If `rip` lives elsewhere, set:

```json
{
  "rip.executable": "C:/tools/rip/rip.exe"
}
```

## Getting started

```rp
mod app;

use std::dev;

func main() -> void {
    let msg: string = "Hello, Rip!";
    dev.log(msg);
}
```

Then run `rip run main.rp`, or press `F5` after adding a `launch.json` with `"type": "rip"`.

## Extension settings

| Setting | Default | Description |
| --- | --- | --- |
| `rip.executable` | `rip` | Path to the `rip` binary that provides the `lsp` and `dap` subcommands. |

## Building the extension locally

```bash
cd editors/vscode
npm install
npx vsce package
code --install-extension rip-lang-0.1.0.vsix
```

## Links

- [Rip language specification](../../doc/RipLang-level.md)
- [Language manual & standard library](../../doc/RipLang-help.md)
- [Examples](../../examples)

## License

MIT
