# Rip

**中文** | [English](README.en.md)

Rip 是一门面向服务端与 Web 的强类型语言：零类型推断、无 `class`、引用计数 + 环回收，保留 Rust 的所有权语义；同一份源码既能解释执行，也能编译成不依赖运行时的原生二进制。

[![version](https://img.shields.io/badge/version-0.1.0-blue.svg)](#)
[![platform](https://img.shields.io/badge/platform-windows%20x64%20%7C%20linux-lightgrey.svg)](#安装)
[![runtime](https://img.shields.io/badge/runtime-interpreter%20%2B%20AOT-green.svg)](#两种执行模式)

---

## 简介

Rip 的核心原则是**唯一写法**：同一语义只允许一种语法，不允许别名与第二写法。这让代码在不同人、不同项目之间保持完全一致的可读性，也让工具链（格式化、静态检查、LSP、AOT 编译器）不必为同义写法做分支。

语言实现对使用者只暴露两种交付形态：

| 形态 | 内容 | 使用者需要 |
| --- | --- | --- |
| 运行时二进制 | `rip` 命令行 + 随包运行库 | 直接运行、打包、原生编译 |
| 运行时源码 | Rust workspace 源码 | 仅工具链维护者，需自行 `cargo build` |

## 特性

**语言**

- 强类型、零推断：所有类型显式声明，不猜测
- 无 `class`：以 `struct` + `type` 别名 + 函数与模块组织代码
- 所有权语义完整保留：`move` / `copy` / 借用检查在编译期给出诊断
- 内存模型：容器为 `Rc<RefCell<…>>`，无环时立即释放；环由线程内环回收器按分配间隔自动处理
- 统一的错误处理：`?` 与 `ok` / `err`、`some` / `none` 参与 `match`
- 注解驱动：`#[get]` 等路由注解、`#[interval]` 定时任务、`#[test]` / `#[bench]` 测试基准

**运行时**

- 双执行路径：纯标量函数编译为栈式字节码，含容器 / 闭包 / 模式匹配的函数回退树遍历解释器，两者语义一致
- 事件驱动 HTTP 服务端采用 thread-per-core 架构：acceptor 分发连接，此后读写与处理全程在单线程内联完成，每请求零跨线程交接
- 内置数据库驱动（MySQL / PostgreSQL / SQLite / SQL Server）、TLS、WebSocket、定时任务、邮件、压缩、加密等标准库
- 可观测性：`RIP_STAGE_TIMING` 阶段耗时、`RIP_STAGE_CPU` 线程 CPU 归因、`rip profile` 函数级采样、`sys.heap_usage()` 堆统计

**工具链**

- `rip run` / `rip dev`：源码直接执行，`dev` 带热重载
- `rip build`：完整 AOT，覆盖全部语法，产物为原生二进制且不含 Rip 源码
- `rip test` / `rip bench` / `rip profile` / `rip check` / `rip fmt` / `rip doc`
- `rip lsp` / `rip dap`：编辑器语言服务与调试适配器
- 包管理：`rip add` / `rip lock` / `rip pack` / `rip pub` / `rip search` / `rip audit`
- 数据库迁移：`rip migrate up|down|status`

## 安装

### Windows（已打包，开箱即用）

Windows x64 运行时已打包为自包含安装包，内置编译器、链接器与 AOT 运行时库，机器上**不需要**安装 Rust / cargo / 任何编译工具：

1. 从 [Releases](https://github.com/LeePillar/rip-lang/releases/tag/v0.1.0) 下载 `rip-0.1.0-windows-x64.zip` 并解压
2. 双击解压目录内的 `install.cmd`，脚本会把该目录写入**当前用户 PATH**（无需管理员权限）
3. 新开一个终端，执行 `rip doctor` 自检

只在当前会话使用（不改系统设置）：

```powershell
. .\env.ps1
rip version
```

卸载：`.\install.ps1 -Uninstall`。

### Linux（从源码自行构建运行时）

仓库目前只提供 Windows 预编译包。Linux 上可直接用源码构建运行时：

```bash
# 需要本机 Rust 工具链（rustc + cargo）
cargo build --release -p rp-cli      # 产出 target/release/rip
./target/release/rip doctor
```

`rip build` 会调用本机 rustc 完成原生编译。若要像 Windows 包那样分发「自带编译器的独立运行时」，可按 `scripts/bundle-toolchain.ps1` 的目录结构自行组装：`rip.exe` 同级放置 `toolchain/`（rustc 与目标平台运行库）、`lib/rip/deps/`（AOT 运行时库 `rp-core` 及其依赖）、`lib/rip/build/`、`lib/rip/native/`，运行时通过 `RIP_TOOLCHAIN` 与 `RIP_AOT_DEPS` 定位。

## 快速开始

```bash
rip new demo --template http     # 创建项目骨架（http / cli / empty）
cd demo
rip run main.rp                  # 运行时 + 源码：解释执行
rip build main.rp -o dist        # AOT：编译为原生二进制，产物在 dist\
```

一个最小的 Rip 程序：

```rip
mod hello;

use std::dev;

func main() -> void {
    let msg: string = "Hello, Rip!";
    dev.log(msg);
    print(msg);
}
```

一个 HTTP 服务（`#[get]` 注解注册路由）：

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

更多示例见 `examples/`（所有权、泛型、闭包、异步、数据库、HTTP、时区、压缩等），完整应用见 `demo/`（前后端分离的用户管理示例）。

## 两种执行模式

| 方式 | 命令 | 目标机器依赖 | 适用场景 |
| --- | --- | --- | --- |
| 运行时 + 源码 | `rip run` / `rip dev` | 需安装 `rip` | 开发、运维热更新 |
| 原生二进制（AOT） | `rip build` | 无需运行时，产物不含源码 | 交付部署 |

两种模式的语义、诊断与退出码一致，`rip test --diff <目录>` 可逐文件比对两者输出。

`rip build` 默认链接关闭了 `interp` feature 的 `rp-core`，产物不包含 AST 解释器与字节码 VM；找不到 `rp-core` 时回退链接完整 `rp-runtime` 并给出提示。

## 目录结构

```text
rip/
├── crates/
│   ├── rp-lexer/           词法分析
│   ├── rp-ast/             AST 定义
│   ├── rp-parser/          语法分析
│   ├── rp-types/           类型系统
│   ├── rp-ownership/       所有权与借用检查
│   ├── rp-core/            值模型与运行时（AOT 链接目标）
│   ├── rp-runtime/         解释器、字节码 VM、HTTP 服务端、标准库
│   ├── rp-cli/             rip 命令行
│   ├── rp-lsp/             语言服务
│   ├── rp-dap/             调试适配器
│   └── rp-rust-interop/    Rust 原生扩展接入
├── demo/                   前后端分离的完整示例（Vue 3 + Rip 服务端）
├── doc/                    语言规范与手册（中英文）
├── examples/               语言特性示例
├── scripts/                打包脚本
└── tests/                  语言级回归用例
```

## 命令行

| 命令 | 说明 |
| --- | --- |
| `rip run [file.rp]` | 运行源码，可加 `--host` / `--port` / `--cert` / `--key` |
| `rip dev <file.rp>` | 开发服务：启动并热重载 |
| `rip build <file.rp>` | 编译为原生二进制并生成 `dist.zip` |
| `rip test [dir\|file]` | 运行 `#[test]` 函数，支持 `--filter` / `--jobs` / `--coverage` |
| `rip bench` / `rip profile` | 性能基准 / 函数级耗时采样 |
| `rip check` / `rip fmt` / `rip doc` | 静态检查 / 格式化 / 生成 API 文档 |
| `rip new` / `rip add` / `rip lock` | 项目骨架 / 依赖管理 / 依赖锁定 |
| `rip pack` / `rip pub` / `rip search` / `rip audit` | 包编译 / 发布 / 检索 / 审计 |
| `rip migrate up\|down\|status` | 数据库迁移 |
| `rip lsp` / `rip dap` | 语言服务 / 调试适配器 |
| `rip repl` / `rip doctor` / `rip version` | 交互式解释器 / 环境自检 / 版本信息 |

## 文档

| 文档 | 中文 | English |
| --- | --- | --- |
| 语言开发规范 | [RipLang-level.md](doc/RipLang-level.md) · [HTML](doc/RipLang-level.html) | [RipLang-level.en.md](doc/RipLang-level.en.md) · [HTML](doc/RipLang-level.en.html) |
| 语言手册 | [RipLang-help.md](doc/RipLang-help.md) · [HTML](doc/RipLang-help.html) | [RipLang-help.en.md](doc/RipLang-help.en.md) · [HTML](doc/RipLang-help.en.html) |
| 示例目录规范 | [demo/README.md](demo/README.md) | [demo/README.en.md](demo/README.en.md) |

## 环境变量

```text
RIP_TOOLCHAIN        安装目录（默认取 rip 可执行文件所在目录）
RIP_AOT_DEPS         AOT 运行时库目录（默认 lib/rip/deps）
RIP_STAGE_TIMING=1   输出每 20000 请求的分阶段耗时
RIP_STAGE_CPU=1      输出线程 CPU 的阶段归因
RIP_NO_VM=1          关闭字节码路径，强制解释器执行（用于对比诊断）
RIP_HTTP_DEBUG       输出连接与任务轨迹
```

## 状态

当前版本 `0.1.0`，语言规范见 `doc/RipLang-level.md`（v1.3）。语言与运行时仍在演进，涉及语法、类型与标准库的变更以规范文档为准。
