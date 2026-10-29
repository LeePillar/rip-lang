# Changelog

All notable changes to the Rip VS Code extension are documented here.
The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

## [0.1.0] - 2026-10-29

### Added

- Syntax highlighting for template strings with `${}` interpolation, attributes, method declarations, `std::`/`rust::` module paths and all primitive/generic types.
- Language server features: hover, definition, completion, references, document highlight, signature help.
- Inlay hints that show parameter names at call sites.
- Document symbols (outline) and folding ranges for brace blocks and block comments.
- Workspace symbol search across all `.rp` files.
- 17 code snippets for common Rip constructs (functions, types, routes, tests, timers, async, defer).
- `[rip]` editor defaults: 4-space indentation, no word-based suggestions.
- Bundle metadata: icon, repository/homepage/bugs links, keywords, `.vscodeignore`, README, CHANGELOG and LICENSE.

### Changed

- Removed the redundant `onLanguage:rip` activation event; activation is derived from `contributes.languages` on VS Code 1.74+.
- Language server document selector now matches by language only, so untitled and virtual `rip` documents are served too.

### Fixed

- Block comments, `onEnterRules`, `wordPattern` and `indentationRules` were missing from the language configuration.
- Type keywords removed from the language (`int2`, `int8`, `int16`, `char`) were still highlighted as builtin types.
