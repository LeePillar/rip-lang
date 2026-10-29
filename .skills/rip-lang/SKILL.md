---
name: rip-lang
description: Write, review, and debug Rip (.rp) code and rip.toml packages with this repo's rip CLI and stdlib. Use when creating or editing Rip source, commands, HTTP apps, or answering Rip syntax questions. Not for Rust crate internals.
---

# Rip 语言开发技能

Rip 是强类型、**零类型推断**、无 `class`、保留 Rust 所有权语义的服务端/Web 语言。生成 Rip 代码时，先遵守下面的「唯一写法」硬规则，再查具体 API。

## 1. 黄金规则（写错必编译失败）

1. **所有绑定、参数、返回值必须显式写类型**，没有推断（`for`/`match` 绑定是唯一例外）。
2. **同一语义只有一种写法**：不写 `fn`/`def`/`function`，不用 `as` 转换，不用 `:=`，不用 `import`，不用 `v[0]` 下标。
3. 语句以 `;` 结尾；分支/循环/函数体必须用 `{}`。
4. 变量默认只读，`let mut` 才可变；**禁止 shadowing**（同作用域同名重复声明报错）。
5. `string`/`ary`/`vec`/`map`/`option`/`result` 是 Move 类型，赋值或传参后原变量失效；需要保留用 `.clone()`。
6. 每个 `.rp` 文件首行 `mod <目录名>;`，模块名必须等于文件所在目录名。
7. 类型名、变量名、函数名一律 `snake_case`；常量 `SCREAMING_SNAKE_CASE`。
8. 不要发明 API：只能使用本技能、`doc/RipLang-help.md`、`examples/` 中存在的标准库函数。

## 2. 命令速查

```bash
rip new demo --template http     # 骨架：http / cli / empty
rip run main.rp                  # 解释执行
rip run main.rp --host 0.0.0.0 --port 8080
rip dev main.rp                  # 热重载
rip build main.rp -o dist        # AOT 原生二进制（产物不含源码）
rip check [路径]                 # 静态检查
rip fmt [路径]                   # 格式化
rip test [路径] [--filter X]     # 跑 #[test]
rip bench / rip profile          # 基准 / 性能采样
rip add <包> / rip lock          # 依赖
rip doc                          # 生成 API 文档
```

## 3. 最小程序

```rp
mod app;

use std::dev;

func main() -> void {
    let msg: string = "Hello, Rip!";
    dev.log(msg);
    print(msg);
}
```

- 函数可定义在 `main` 之后，运行前统一注册。
- 不支持重载、默认参数、可变参数。

## 4. 类型与绑定

```rp
let 名称: 类型 = 值;        let mut 名称: 类型 = 值;
const 名称: 类型 = 值;      static 名称: 类型 = 值;   // const/static 只能在模块级
```

| 类别 | 类型 |
| --- | --- |
| 整数 | `int`(=i64)、`i8 i16 i32 i64`、`u8 u16 u32 u64`，字面量后缀 `42i32` / `7u8` |
| 其他基础 | `float`(=f64)、`string`、`bool`、`void` |
| 复合 | `vec<T>`、`vec<T1, T2>`、`vec`、`map<K, V>`、`option<T>`、`result<T, E>`、`task<T>`、`share<T>`、`func(A) -> B` |

- 进制前缀只表示进制：`0xFF`、`0o755`、`0b1010`；类型由后缀决定。
- **不同整数类型不能直接运算**，先 `.to_int()` / `.to_u8()` 等显式转换。
- 整数溢出 panic，不回绕；零除 panic。
- 类型声明三种等价写法：`type` / `struct` / `obj`，固定形式 `关键字 名 = 类型`。

```rp
type user = [id: int, name: string, tags: vec<string>];
type status = enum { active, pending, banned(reason: string) };
type shape = interface { func area() -> float }
```

递归类型必须经容器间接自引用：`type node = [value: int, children: vec<node>];`。

## 5. 三种数据结构

```rp
// ary：固定字段（类型用 :，字面量用 =）
let mut u: user = [id = 1, name = "张三", tags = ["a"]];
u.name = "李四";                 // 仅 let mut，字段集合编译期封闭

// vec：有序序列
let v: vec<int> = [1, 2, 3];
let hetero: vec<string, int> = ["a", 1];
let empty: vec<int> = [];
let first: int = v.0;            // 编译期位置
let item: result<int, string> = v.at(0);   // 运行期位置

// map：动态键值
let mut m: map<string, int> = [];
m.set("a", 1);
let got: option<int> = m.get("a");
```

- 访问一律点号/方法：禁止 `u["name"]`、`v[0]`、`m["k"]`。
- `map` 的 K 只能是 `string`/`int*`/`bool`。
- 异构 `vec<T1,T2>` 定长，不能 push/pop/insert/remove/sort/reverse。

常用内建方法：

```rp
v.len() v.is_empty() v.push(x) v.pop() v.insert(i, x) v.remove(i) v.clear()
v.reverse() v.sort(func(a: T, b: T) -> int) v.map(f) v.filter(f) v.reduce(f, init)
v.slice(a, b) v.concat(o) v.unique() v.flatten() v.join(sep) v.chunk(n) v.zip(o)
m.set(k,v) m.get(k) m.has(k) m.delete(k) m.keys() m.values() m.items() m.merge(o)
option: some/none + .unwrap_or(x) .is_some() .is_none() .unwrap()
result: ok/err + .unwrap_or(x) .is_ok() .is_err() .unwrap() .unwrap_err()
```

## 6. 所有权与借用

| 类别 | 类型 | 行为 |
| --- | --- | --- |
| Copy | 所有整数、`float`、`bool` | 复制，原变量可用 |
| Move | `string`、`ary`、`vec`、`map`、`option`、`result`、`func`、枚举 | 转移，原变量失效 |

```rp
func greet(read u: user) -> string { return "Hi " + u.name; }   // 等价 u: &user
func rename(write u: user, n: string) -> void { u.name = n; }   // 等价 u: &mut user
func consume(u: user) -> int { return u.id; }                    // 默认 move
```

- `read`/`write` 只用于参数；`&T`/`&mut T` 是位置等价写法，不可混用。
- `write` 实参必须是 `let mut` 变量；临时值不能传给 `write`。
- 同一时刻「多个 read」或「一个 write」，不可混用；禁止部分移动；无裸指针/无 unsafe。
- 返回引用只能来自唯一输入引用；用户不写生命周期。

## 7. 函数、方法、接口、泛型、闭包

```rp
func add(a: int, b: int) -> int { return a + b; }

func user.display(read u: user) -> string { return u.name; }        // 方法：类型.方法
func user.rename(write u: user, n: string) -> void { u.name = n; }
u.display(); u.rename("李四");

type shape = interface { func area() -> float };                    // 隐式实现，无需 impl
func circle.area(read c: circle) -> float { return 3.14 * c.radius * c.radius; }

func identity<T>(x: T) -> T { return x; }                           // 泛型
func total<S: shape>(read items: vec<S>) -> float { ... }           // 接口约束

let double: func(int) -> int = func(x: int) -> int { return x * 2; };  // 闭包必须显式类型
self func hash_password(raw: string) -> string { ... }              // 仅包内可见
```

- 方法接收者显式写 `read`/`write`，不引入 `self`/`this`；方法名在类型内唯一。
- 泛型运行期类型擦除；同名函数不重载。
- `pub` 导出；默认模块内私有。

## 8. 控制流与匹配

```rp
if x >= 90 { } elif x >= 80 { } else if x >= 60 { } else { }

loop { break; }
while cond { }
while let some(v) = cache.get("k") => { cache.delete("k"); }        // => 引出循环体
for item in list { }        // vec → 元素；map → 键

match res {
    ok(v) => { print(v); }
    err(e) => { log.error(e); }
}
match opt { some(x) => { }, none => { } }
match s {
    status.active => { }
    status.banned[reason: string] => { print(reason); }
}
```

- `match` 必须穷尽（option/result/枚举）；分支用 `=>`，绑定/赋值用 `=`。
- 枚举字段只能通过 `match` 取出，不能 `s.reason`。
- `?` 只能出现在返回 `result` 的函数内，对 `err` 立即向上返回。

## 9. 注解（只写在模块级 func 之前，每行一个）

```rp
#[get("/users/:id")]      // get/post/put/delete/patch/options/head
func show(read req: http.request) -> http.response { ... }

#[interval("5m")]         // ms / s / m / h / d
#[test]                   // 无入参、返回 void，rip test 执行
#[bench]                  // 无入参、返回 void，rip bench 执行
```

路由函数签名固定为 `func(read req: http.request) -> http.response`；路由注解自动注册，静态资源等显式路由传给 `http.router`。

## 10. 并发

```rp
async func load(id: int) -> user { return fetch_user(id); }

func main() -> void {
    let a: task<user> = load(1);
    let b: task<user> = load(2);
    let x: user = await a;
}
```

- `async` 只能修饰模块级自由函数；调用立即返回 `task<T>`，`await` 取结果，同一任务只能等待一次。
- 线程级隔离：任务看不到调用方变量，参数/返回值按值复制。
- `share<T>` 用于跨线程共享；`std/task` + `#[interval]` 做后台任务。

## 11. 标准库入口

完整签名见 [references/stdlib.md](references/stdlib.md)。最常用：

```rp
use std::http;   use std::json;   use std::time;   use std::fs;
use std::sys;    use std::db;     use std::log;    use std::llm;
use std::task;   use std::sync;   use std::crypto; use std::encode;
```

三种调用形式：`add(1, 2)`、`http.serve(8080, handler)`、`v.push(3)`。`use` 只导入模块，不能逐项导入；别名用 `use std::sys as sys;`。

## 12. 项目结构

```text
demo/
├── main.rp
├── app/              # 目录名 = mod 名
│   └── users.rp      # 首行 mod app;
├── packages/         # 第三方 Rip 包
│   └── utils/http.rp # use utils::http;
└── rip.toml
```

```toml
[module]
name = "demo"
version = "0.1.0"

[dependencies]
# utils = "^1.0"
```

- `mod foo;` 不加载子模块文件；项目按目录递归收集 `.rp`。
- 包内源码不校验「模块名 = 目录名」，包身份以 `packages/` 下路径为准。

## 13. 常见错误 → 修正

| 错误写法 | 正确写法 |
| --- | --- |
| `let x = 1;` | `let x: int = 1;` |
| `x = 1;`（首次声明） | `let x: int = 1;` |
| `count := 0` | `let mut count: int = 0;` |
| `v[0]` / `u["f"]` / `m["k"]` | `v.at(0)` / `u.f` / `m.get("k")` |
| `a as int` | `a.to_int()` |
| `11.to_i16()` | 先 `let x: int = 11;` 再 `x.to_i16()` |
| `func f(x: int) { }` | `func f(x: int) -> void { }` |
| `import "std/http";` | `use std::http;` |
| `let x: int = 1; let x: int = 2;` | 改名，禁止 shadowing |
| `u.age + u8_val` | 统一类型后再运算 |
| `defer 1 + 2;` | `defer f(...)` 只能是调用 |
| `class ...` / `impl ...` | `type ...` + `func 类型.方法(...)` |
| 字符串插值 `"a${x}"` | `"a" + x.to_string()` 或 `"""...${x}..."""` |

## 14. 交付前自检

1. 每个文件首行 `mod` 与目录名一致；同目录所有文件模块名一致。
2. 所有 `let`/参数/返回值都有类型；语句带 `;`。
3. 没有下标访问、`as` 转换、`import`、`class`、`fn`。
4. Move 类型是否被二次使用（需要则 `.clone()`）。
5. `match` 覆盖了 option/result/枚举的全部变体。
6. HTTP：路由函数签名正确，`body`/`json`/`sse` 只写一个。
7. 运行 `rip check`，再 `rip fmt`。

## 15. 参考资料（工作区根目录）

- `doc/RipLang-level.md`：语言开发规范 v1.3（语法/类型/所有权的唯一权威）
- `doc/RipLang-help.md`：语言手册与标准库 API
- `examples/`：所有权、泛型、闭包、异步、HTTP、数据库、时区、压缩、反射等可运行样例
- `tests/`：语言级回归用例；`demo/`：完整前后端示例
