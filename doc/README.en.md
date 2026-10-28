# Rip Documentation

[中文](README.md) | **English**

This directory holds the Rip language specification and the user manual. Both are available in Chinese and English, and every page can switch languages from its header.

| Document | Contents | 中文 | English |
| --- | --- | --- | --- |
| Language specification | The complete specification for compiler implementers: lexing, syntax, types, ownership, modules, standard library, AOT, project layout | [Markdown](RipLang-level.md) · [HTML](RipLang-level.html) | [Markdown](RipLang-level.en.md) · [HTML](RipLang-level.en.html) |
| Language manual | Syntax and standard library reference for users | [Markdown](RipLang-help.md) · [HTML](RipLang-help.html) | [Markdown](RipLang-help.en.md) · [HTML](RipLang-help.en.html) |

## Reading order

1. Start with the [language manual](RipLang-help.en.md) to get syntax and everyday standard library usage working;
2. Consult the [language specification](RipLang-level.en.md) when you need to confirm semantic boundaries (type rules, ownership, error handling, modules and packages);
3. Use the layout chapter of the specification and the [demo notes](../demo/README.en.md) as the reference for project structure and engineering organisation.

## Conventions

- The specification is the single source of truth: example scripts follow the specification, never the other way round.
- The HTML editions suit browser reading and full-text search; the Markdown editions suit editors, terminals, and code review.
- Both formats carry identical content and must be updated together.
