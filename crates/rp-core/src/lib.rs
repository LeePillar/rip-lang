//! Rip 核心宿主（AOT 专用）。
//!
//! 本 crate 是 `rp-runtime` 关闭 `interp` feature 后的形态：只保留值模型、
//! 标准库、HTTP/DB/net 与原生函数表，**不含** AST 解释器和字节码 VM。
//!
//! `rip build --aot` 生成的原生程序链接本 crate，因此产物里没有解释器代码。
//! 解释执行（`rip run` / `rip build` 打包）仍使用 `rp-runtime`。

pub use rp_runtime::*;
