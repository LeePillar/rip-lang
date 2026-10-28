# Rip 文档

**中文** | [English](README.en.md)

本目录存放 Rip 的语言规范与使用手册，中英文各一份，任意页面顶部都可互相跳转。

| 文档 | 内容 | 中文 | English |
| --- | --- | --- | --- |
| 语言开发规范 | 给编译器实现者的完整规范：词法、语法、类型、所有权、模块、标准库、AOT 与目录规范 | [Markdown](RipLang-level.md) · [HTML](RipLang-level.html) | [Markdown](RipLang-level.en.md) · [HTML](RipLang-level.en.html) |
| 语言手册 | 面向使用者的语法与标准库速查 | [Markdown](RipLang-help.md) · [HTML](RipLang-help.html) | [Markdown](RipLang-help.en.md) · [HTML](RipLang-help.en.html) |

## 阅读顺序

1. 先用 [语言手册](RipLang-help.md) 跑通语法与标准库的日常用法；
2. 需要确认语义边界（类型规则、所有权、错误处理、模块与包）时查 [语言开发规范](RipLang-level.md)；
3. 目录结构与工程组织以规范中的目录章节与 [示例说明](../demo/README.md) 为准。

## 约定

- 规范是唯一权威：示例脚本服从规范，规范不服从示例。
- HTML 版适合浏览器阅读与全文搜索；Markdown 版适合在编辑器、终端与代码评审中查看。
- 两种格式内容一致，更新时需同步。
