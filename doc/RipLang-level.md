# Rip 语言开发规范 v1.3

> 本文档是给编译器实现者的完整规范。Rip 是一门强类型、零推断、无 class、引用计数加环回收、保留 Rust 所有权语义的服务端/Web 语言，后缀 `.rp`，工具 `rip`，包清单 `rip.toml`。 **核心原则：唯一写法**。同一语义只允许一种语法，禁止别名、糖、第二写法（规范显式列出的便利糖除外）。 **实现目标**：解释器 + AOT 编译器双模式，宿主 Rust。 **权威边界（强制）**：本仓库的交付物是 Rust 实现的 Rip 运行时与工具链（发布编译产物或 Rust 源码）。Rip 是面向所有人的通用语言，不是某一个项目的内部脚本语言。`demo/` 只是用 Rip 写的验证示例，**示例服从语言规范，语言规范不服从示例**——任何为了让某个 demo 跑通而修改词法、语法、类型、所有权或标准库语义的做法都禁止。

---

## 0. 交付形态与示例边界

### 0.1 对外发布

发布物只有两种合法形态，除此之外不发布：

| 形态 | 内容 | 使用者需要 |
| --- | --- | --- |
| 运行时二进制 | 用 Rust 编译出的 rip 及其随包运行库目录 | 直接用它运行 / 打包 / 原生编译（见 10.11） |
| 运行时源码 | Rust workspace 源码 | 仅工具链维护者，需 cargo build 得到 rip |

语言规范、语法手册、目录规范属于文档交付，随运行时分发。

### 0.2 两种执行方式

| 方式 | 命令 | 依赖 |
| --- | --- | --- |
| 源码直接执行 | rip run、rip dev | 目标机器必须安装 rip 运行时 |
| 原生二进制（AOT） | rip build | 产物为原生可执行文件，目标机器不需要安装运行时，也不含 Rip 源码 |

- 源码执行用于开发与运维热更新；原生二进制用于交付部署。
- `rip build` 默认执行完整 AOT：把全部 Rip 语法翻译成宿主程序，链接随 `rip` 分发的运行时库，再由内置编译组件生成原生二进制。
- `--aot` 参数保留为兼容旧脚本，与缺省行为相同。
- 两种方式语义必须一致：同一份源码在 `run` 与 `build` 下的行为、诊断、退出码相同；`rip test --diff <目录>` 会自动逐文件比对。
- 曾经的「解释器 + 内嵌源码」单文件产物已移除：它既不比源码执行更方便，也不如原生二进制轻量，属于两头都不占优的中间态。

**执行模型**：运行时由「字节码快速路径 + 树遍历解释器」组成。

- 纯标量函数（局部变量、算术/比较/逻辑、`if`/`for`、函数调用、模块与方法调用）编译成栈式字节码执行，变量按槽位访问，不再做名字哈希查找。
- 含结构体、`vec`/`map`、`match`、闭包、全局变量、`?` 等语法的函数整体回退到解释器；两种路径共用同一套运算、调用与错误语义，`#[test]` 与 `demo/server` 回归用例两者输出必须一致。
- 字节码编译期优化：字面量常量折叠、按作用域回收并复用局部槽位、`int` 槽位专用指令（槽位 op 常量、槽位自更新、条件直跳）、热点表达式融合（`(x op 常量) op y`、`表达式 % 常量` 直接写回）、已编译函数用下标直接调用；溢出/除零/类型不匹配仍回落到统一实现并给出同样的错误。
- 字节码只编译一次：编译结果随 `RuntimeSeed` 以 `Arc` 共享，HTTP 工作线程、`parallel` 分支、`task.spawn` 与并行测试各自新建运行时都不再重复编译；指令里的字符串同样用 `Arc`，因此编译产物可跨线程复用。
- 内存模型：容器是 `Rc<RefCell<…>>`，无环时立即释放；「容器 → 闭包 → 捕获的容器」这类环由线程内的环回收器用试验删除法找出无外部引用者并清空（请求 / 测试结束时按分配间隔自动触发，也可用 `sys.gc()` 手动触发）。`sys.heap_usage()` 返回计数分配器统计的堆占用；`[server] max_heap_mb` 设置进程堆上限，超限先回收、仍超限则当前请求失败。
- 排障开关：设置 `RIP_NO_VM=1` 可完全关闭字节码路径，强制使用解释器对比行为。

**实验性原生编译（`rip build --native`）**：

- 把「标量 int 子集」翻译成宿主程序，再调用内置编译组件生成原生可执行文件；中间源码只写**当前项目根目录下的 `.tmp/`**，编译结束即删除（目录空了也一并删除），不会出现在交付目录。
- 支持范围：`int`/`void` 参数与返回值、局部 `int` 变量、算术/比较/逻辑、`if`/`else`、`for` 的两种形式、`return`/`break`/`continue`、函数直接调用（含递归）、字符串字面量与 `+` 拼接、`int.to_string()`、`print`（单个 int/string）、int 常量。
- 超出范围（结构体、`vec`/`map`、`match`、闭包、模块调用、`db`/`http`、可变全局等）会明确报错并提示改用 `rip build`，**不会**生成语义不一致的产物。
- 该路径仍在实验阶段，适合计算密集型任务与性能验证；业务应用（数据库、HTTP）使用 `rip build` 的完整 AOT。

**完整 AOT（`rip build` 默认）**：

- 把**全部 Rip 语法**翻译成宿主程序（值模型、运算、调用统一使用运行时 `Value`），链接宿主支持库后由内置编译组件生成原生二进制；不经过解释器，也不内嵌 Rip 源码。
- 覆盖范围：函数/递归、闭包与捕获、`struct`/`vec`/`map`、`match`（`some`/`none`/`ok`/`err`/结构体）、`loop`/`while`/`while let`/`for-in`、`?`、模块与第三方包、全部 `std/*`。
- 原生调度：生成的二进制会附带函数表、注解路由表（`#[get]` 等）与 `#[interval]` 定时任务表，HTTP 工作线程、`task.spawn`、`ws.serve` 按名字重建，行为与解释模式一致；`rip.toml` 的 `[runtime]`、`[server]`、`[cluster]` 在构建期写入产物。
- 命令行：生成的二进制接受 `--port` / `--host` / `--cert` / `--key`，与 `rip run` 相同。
- 构建依赖：使用者机器**不需要**安装任何编译工具；编译组件、链接器与运行时库随 `rip` 分发（见 10.11）。Windows 下 MinGW 目标用工具链自带的 `rust-lld`，MSVC 目标用系统 `link.exe`。
- **纯净产物**：默认链接 `rp-core`（关闭 `interp` feature 的 `rp-runtime`），产物不含 AST 解释器与字节码 VM；找不到 `rp-core` 时回退链接 `rp-runtime` 并打印提示。构建方式见 10.9。
- 与 `--native` 的区别：`--native` 只覆盖标量 int 子集、产物完全独立；`rip build` 覆盖全语言、产物仍需链接宿主支持库（已静态链接进二进制）。

**平台规则（强制）**：

| 平台 | 源码 + rip 运行时 | rip build 产物 |
| --- | --- | --- |
| Windows | 支持 | .exe（PE 格式） |
| Linux | 支持 | 无扩展名可执行文件（ELF） |
| macOS | 支持 | 无扩展名可执行文件（Mach-O） |
| 其它 Unix | 支持（Rust 目标存在时） | 该平台本地可执行格式 |

- 产物格式遵循目标平台本地标准，不引入自定义容器或脚本包装。
- 缺省构建当前平台；`rip build --target <triple>`（或 `[build] target`）可交叉编译。交叉编译需要目标标准库、链接器与 C 依赖工具链，见 10.13 的构建依赖表。
- 目标 triple 与宿主相同的仍走本机快速路径（直接调用内置编译器），不经过 cargo。

**打包目录规则（强制）**：`rip build` 不是只产出一个二进制，而是产出一个自包含的交付目录（默认 `dist/`），交付时整个目录拷贝即可运行。目录内必须包含：

| 内容 | 来源 | 说明 |
| --- | --- | --- |
| <name>[.exe] | 入口 + 全部 .rp 合并 | 名称取 [build] name，缺省 [module] name，再缺省入口文件名 |
| README.md | 自动生成 | 必须写明应用名称、版本号（取 [module] version）、平台与启动方式 |
| [build] copy 声明的路径 | 项目内同路径 | 运行期数据（数据库、上传文件等）由项目声明，递归复制，保证交付后数据不丢；构建不得清空已有数据；日志与临时文件不复制。工具链不预设 storage/、uploads/ 等目录名 |
| migrations/ | 项目目录 | 存在时原样复制 |
| 静态资源 | public/、assets/、static/、[build] assets | 按目录名复制到交付目录；前后端分离项目不应声明前端目录 |

输出目录与可执行名可通过配置或命令行指定，优先级从高到低：

| 项 | 命令行 | rip.toml | 缺省 |
| --- | --- | --- | --- |
| 输出目录 | -o <dir> | [build] dir | dist/ |
| 可执行名 | --name <name> | [build] name | [module] name |
| 随包数据 | 无 | [build] copy | 空（不复制任何运行期目录） |

`[build] copy` 与 `[compile] exclude` 都是自由配置，不绑定任何目录名。示例：

```toml
[build]
copy = ["storage", "uploads"]   # 目录或单个文件，相对项目根
[compile]
exclude = ["storage", "uploads", "data/db"]
```

`dist/`、`build/` 不参与编译单元。

### 0.3 示例（`demo/`）边界

`demo/` 是语言与工具链的**消费方**，不是定义方：

1. 示例只能用当前规范已定义的能力；缺什么先走语言演进，不能在示例里造特例。
1. 禁止为示例新增关键字、别名、隐式转换、语法糖，或放宽类型/所有权检查。
1. 禁止为示例在运行时里加"只对某目录 / 某文件名生效"的特判。
1. 示例发现规范缺陷时的顺序是：**先改本规范 → 再改实现与测试 → 最后改示例**，三步都留记录。
1. 示例代码不得成为标准库语义的事实标准；标准库行为以第 11 节为准。
1. demo 可以自由选择目录组织与命名，但不得反过来要求修改本规范第 10 节或 `demo/README.md` 中的项目目录结构规范。

---

## 1. 语言定位

| 项 | 定义 |
| --- | --- |
| 名称 | Rip（锐） |
| 后缀 | .rp |
| 工具 | rip |
| 包清单 | rip.toml |
| 定位 | 服务端 / Web 语言 |
| 风格 | Go 结构 + PHP 易用 + Rust 安全 |
| 类型 | 强类型，零推断，全显式 |
| 内存 | 引用计数 + 环回收，Rust 所有权语义，用户不写 & / 'a |
| 类 | 无 class，ary + 自由函数 + 结构化类型 |
| 执行 | 解释模式（开发）+ AOT 模式（部署） |
| Rust 互操作 | 支持，只按 Rip 语法调用 |
| 模板 / GUI | 不内置，通过 Rust 包扩展 |

---

## 2. 词法

### 2.1 标识符

```text
[a-zA-Z_][a-zA-Z0-9_]*
```

区分大小写，禁止 `$` 前缀。

### 2.2 关键字（全量）

```text
let  mut  const  static  self  func  async  await  return  defer
if  elif  else  for  in  loop  while  match  parallel  break  continue
mod  use  as  pub  type  struct  obj  interface  enum
read  write  share
true  false  null
ok  err  some  none
```

关键字不可用作变量名、函数名、类型名或模块名。唯一例外：`.` 之后的成员名允许与关键字同名（标准库已有 `http.static`），按成员名处理，不得据此扩展到其它位置。

`package` 与 `import` 已从语言中移除：它们不是关键字，也不存在对应声明，写在源码里只会得到语法错误，没有兼容路径。

### 2.3 字面量

| 类型 | 写法 | 示例 |
| --- | --- | --- |
| 整数 | 十进制，可选类型后缀 | 42、1_000、42i32、7u8 |
| 整数（十六进制） | 前缀 `0x`，可选后缀 | 0xFF、0xFFu8、0xdead_beefu32 |
| 整数（八进制） | 前缀 `0o`，可选后缀 | 0o755、0o755u16 |
| 整数（二进制） | 前缀 `0b`，可选后缀 | 0b1010、0b1010u8 |
| float | 带小数点或指数，可选后缀 `f64` | 3.14、1e9、2.0f64 |
| string | `"` 或 `'` | "hello"、'a'（`'` 必须恰好一个 Unicode 标量） |
| bool | 关键字 | true / false |
| null | 关键字 | 仅 option<T> 可用 |

**规则**（与 Rust 一致）：

- **前缀只表示进制，不决定类型**；类型由后缀给出（`u8`/`u16`/`u32`/`u64`/`i8`/`i16`/`i32`/`i64`）。
- 不带后缀的整数是 `int`（= `i64`），不带后缀的小数是 `float`（= `f64`）。
- 后缀与数值必须匹配（`256u8` 编译报错）；`int2` / `int8` / `int16` 不再是类型。
- 数字之间允许 `_` 分隔，便于阅读。

**引号规则**：

- `"..."` 任意长度字符串，支持转义 `\n` `\t` `\"` `\\`。
- `'...'` **必须恰好一个 Unicode 标量**，`''` 和 `'ab'` 编译报错。
- 单字符类型仍是 `string`，不引入 `char` 类型。
- `"""..."""` 多行模板：原样保留换行与缩进，内容里的 `${表达式}` 求值后拼接；`\${` 表示字面量 `${`。

插值在解析阶段展开成普通字符串拼接（`"a" + x.to_string() + "b"`），源码执行与原生编译行为一致。

- 普通双引号字符串不做插值，保持原有语义。

### 2.4 符号职责

| 符号 | 唯一用途 |
| --- | --- |
| : | 名称 → 类型 |
| :: | `use` 路径分段 |
| = | 变量绑定 / 赋值（`let`、赋值、`while let` 的模式绑定） |
| => | 匹配分支：左边模式匹配 → 右边执行（`match` 分支、`while let` 条件后） |
| -> | 返回类型 |
| ; | 语句结尾 |
| . | 点号访问 / 调用 |
| ? | result 错误传播（仅 result 函数内） |
| # | 属性引入（仅 #[...]，见 2.6） |

**不存在** **`:=`、`|>`、`as`（作类型转换时）。`as`** **仅用于** **`use ... as`** **别名。`=>` 只出现在上述两处，其他地方用 `=`。**

### 2.5 注释

```rp
// 单行
/* 多行 */
```

### 2.6 属性（注解）

属性是**语言级元数据**，不是注释：

```rp
#[名称]
#[名称("字符串", "字符串")]
```

规则：

- 只能写在模块级 `func` 声明之前，一个函数可以有多个属性，每个属性独占一行。
- 属性名是标识符；参数只能是一个或多个双引号字符串。
- 属性在编译期解析进 AST，不参与运行期求值，也不能作为普通表达式使用。
- 属性名必须属于标准库已定义的集合，未定义属性名是编译错误。
  - HTTP 路由：`#[get(...)]`、`#[post(...)]`、`#[put(...)]`、`#[delete(...)]`、`#[patch(...)]`、`#[options(...)]`、`#[head(...)]`。语义见 11.2.1。
  - 后台任务：`#[interval("1s")]`；时间单位支持 `ms`、`s`、`m`、`h`。
  - 测试：`#[test]`。
- 路由属性必须恰好一个字符串参数，且路径以 `/` 开头。
- `#[test]` 不接受参数；被测函数不能有入参，返回类型必须是 `void`。函数体用 `dev.assert` 断言，由 `rip test` 执行（见 10.7）。

---

## 3. 类型系统

### 3.1 整数与浮点（Rust 语义）

| 类型 | 说明 |
| --- | --- |
| int | 有符号 64 位，等价 `i64`，默认整数类型 |
| float | 64 位浮点，等价 `f64`，默认浮点类型 |
| i8 / i16 / i32 / i64 | 固定位宽有符号整数 |
| u8 / u16 / u32 / u64 | 固定位宽无符号整数 |

**规则**：

- 进制用前缀（`0x` / `0o` / `0b`），类型用后缀（`42i32`、`7u8`）；不带后缀的整数是 `int`。
- `int2` / `int8` / `int16` 不再是类型，字面量前缀也不再决定类型。
- 不同整数类型之间**没有隐式转换**，混用编译报错；同类型运算按该类型位宽计算，溢出 panic（不回绕），且**在运算处**就报错——中间结果同样受检查，不等赋值时才兜底。
- 从字面量初始化时，后缀类型必须能装下数值（`256u8` 编译报错）。

```rp
let a: int = 42;          // int = i64
let b: u8 = 0xFF;         // 前缀只是进制
let c: u16 = 0o755;
let d: i32 = 42i32;       // 显式后缀
let e: u64 = 1_000_000u64;

let f: int = b.to_int();  // 显式转换
let g: u8 = a.to_u8();    // 截断
```

**方法**：`.to_int(base)`（= `.to_i64(base)`）/ `.to_i8(base)` / `.to_i16(base)` / `.to_i32(base)` / `.to_i64(base)` / `.to_u8(base)` / `.to_u16(base)` / `.to_u32(base)` / `.to_u64(base)`（`base` 是进制，2..36，缺省 10；所有整数转换方法都支持）/ `.to_float()` / `.to_string()` / `.to_string_base(base)` / `.abs()` / `.min(o)` / `.max(o)` / `.pow(e)` / `.mod(m)` / `.clamp(lo, hi)`。

**转换语义（与 Rust `as` 一致）**：窄化按模 2ⁿ 截断，`u*` 按无符号解释、`i*` 按补码符号扩展；float → 整数饱和到目标范围；整数 → float 取最近可表示值。

**两种写法（所有整数转换方法一致）**：

- **接收者必须是变量**：转换方法不能直接接字面量。`11.to_i16()`、`"11".to_int(2)` 都是编译错误，先 `let` 绑定再转换。
- **不带参数**：`.to_i8()` / `.to_i16()` / `.to_u16()` … 只做位宽转换，数值保持不变（按 Rust `as` 截断 / 符号扩展）。
- **带参数**：`.to_i8(16)` / `.to_i16(8)` … 先按 `base`（2..36）把接收者的十进制写法重新解释，再做位宽转换；缺省 10 等同于不带参数。

```rp
let x: i8 = -5;
let a: i16 = x.to_i16();       // -5：只转位宽
let b: u8 = x.to_u8();         // 251：按 Rust as 截断

let y: i8 = 55;
let c: i16 = y.to_i16(8);      // 45：位宽 + 8 进制
let d: i8 = y.to_i8(16);       // 85：位宽 + 16 进制
let e: i16 = y.to_i16(10);     // 55：10 进制 = 不带参数
let f: i16 = y.to_i16(32);     // 33："55" 在 32 进制下

let text: string = "ff";
let hex: result<u8, string> = text.to_u8(16);    // ok(255)
let text2: string = "755";
let oct: result<u16, string> = text2.to_u16(8);  // ok(493)
let text3: string = "300";
let bad: result<u8, string> = text3.to_u8();     // err("integer out of range for u8")
```

- 字符串接收者：按进制解析文本，越界返回 `err`；数值接收者：重新解释后按 Rust `as` 截断到目标位宽；数字在该进制下非法（如 `y.to_i8(2)` 且 y = 55）或进制越界（`base must be 2..36`）运行期报错。

### 3.2 其他基础类型

| 类型 | 说明 |
| --- | --- |
| float | 64 位浮点（= `f64`） |
| string | UTF-8 不可变 |
| bool | 布尔 |
| void | 仅作返回类型；`()` 是它的等价写法 |

### 3.3 复合类型

| 类型 | 语法 | 对应 Rust | 说明 |
| --- | --- | --- | --- |
| ary / struct | [name: T, age: T] | struct | 固定字段命名结构；声明可写 `type` / `struct` / `obj` |
| vec | vec<T> / vec<T1, T2> / vec | Vec<T> | 有序隐式数字 key |
| map | map<K, V> | HashMap<K,V> | 动态键值字典 |
| option | option<T> | Option<T> | some(T) / none |
| result | result<T, E> | Result<T,E> | ok(T) / err(E) |
| share | share<T> | Arc<T> | 跨线程共享，T 必须 Send + Sync |
| task | task<T> | 任务句柄 | `async` 调用的结果，用 `await` 取值（见 11.5） |
| func | func(A, B) -> C | 函数指针/闭包 | 函数类型 |
| enum | type x = enum { ... } | Rust enum | 命名变体，`match` 穷尽检查（见 3.4.1） |

### 3.4 命名类型

```rp
type user = [name: string, age: int, tags: vec<string>];
struct user = [name: string, age: int, tags: vec<string>];   // 与 type 等价
obj user = [name: string, age: int, tags: vec<string>];      // 与 type 等价
```

规则：

- 类型声明固定三种写法：`type` / `struct` / `obj`，三者完全等价、可互换，不允许扩展第四种关键字。
- 声明形式固定为 `关键字 名 = 类型`；`struct` / `obj` 只是别名，语义与 `type` 一模一样。
- 可嵌套引用已定义类型。
- 同模块内类型名唯一（同一文件重复定义直接报错）。
- 类型定义运行期不可修改。

**递归类型**允许自引用；实践中必须经 `vec` / `option` / `result` / `map` 等容器间接自引用，才能构造出有限值：

```rp
type node = [value: int, children: vec<node>];   // 合法：经 vec 间接自引用
type tree = [root: node];                        // 合法：链式引用
type a = b;                                      // 编译报错：纯别名环，永远解析不出具体类型
type b = a;
```

规则：

- 展开别名时用「展开栈」检测：某别名已经在栈上就停止展开，因此递归类型只展开一层，不会指数膨胀，也不会栈溢出。
- 纯别名环（只经过 `type x = y` 边走成环）没有容器打断，静态报 `type 'x' is a recursive alias and never resolves`。
- 字段类型直接就是自身的写法（`type bad = [self_ref: bad]`）能通过类型检查，但构造不出有限值，等价于恒为空；需要递归数据时一律经 `vec` / `option` / `map` 间接自引用。
- 两个递归类型只要结构同构就可以互相赋值：`same_type` 在完全相等之外，还会做一次深度上限 32 的结构比较，避免「展开层数不同」被误判为类型不符。

### 3.4.1 枚举（ADT）

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

规则：

- 变体之间用 `,` 分隔，允许尾随逗号；无字段的变体只写名字。
- 变体名在同一枚举内唯一；枚举名与 `type` / `interface` 共用命名空间，重名直接报错。
- 构造：`status.active`（无字段）、`status.banned[reason = "spam"]`（有字段；字段必须写全，类型必须匹配）。
- 枚举是 Move 类型；需要两份时用 `.clone()`。
- 枚举可以定义方法：`func status.blocked(read s: status) -> bool { ... }`，调用为 `s.blocked()`。
- 枚举可以递归引用自身或其他枚举，字段类型同样做显式类型检查。
- 枚举值没有直接字段访问：读取字段必须 `match`（见 9.3），保证按变体取字段。
- JSON 序列化为 `{"variant": "banned", "reason": "spam"}`；无字段变体只有 `variant`。

### 3.5 禁止推断

**任何位置必须显式类型**。唯一例外：`for item in xs` 和 `match` 分支绑定，类型由容器/模式解构得出，不属于推断。

```rp
// 合法
let count: int = 0;
let name: string = "a";

// 非法（编译报错）
count = 0;
count := 0;
tags = ["a"];
func f(x) -> int { return x; }
func f(x: int) { return; }
```

---

## 4. 变量定义

### 4.1 四种绑定（唯一写法）

```text
let 变量名: 类型 = 值;
let mut 变量名: 类型 = 值;
const 常量名: 类型 = 值;
static 静态变量名: 类型 = 值;
```

规则：

- 必须写 `let` / `let mut` / `const` / `static` 之一，**无裸声明**。
- **类型必须写**，无推断。
- **语句以** **`;`** **结尾**。
- `const` 与 `static` 必须是模块级，不能在函数内定义。
- `const` 编译期求值；`static` 运行期初始化，固定地址。
- `static mut` 需 `unsafe` 块，**用户层禁用**。

```rp
const MAX_SIZE: int = 1024;
static GLOBAL_COUNT: int = 0;

func main() -> void {
    let count: int = 0;
    let mut total: int = 0;
    total = total + 1;
    // count = 1;   // 编译报错：count 不可变
}
```

### 4.2 只读变量规则

- `let` 变量**只能读**，不能赋值。
- 要"改变量"，只能创建新变量，或用 `let mut`。

```rp
let x: int = 1;
let y: int = x + 1;     // 用新变量承载结果
// x = 2;               // 编译报错

let mut z: int = 1;
z = z + 1;              // 合法
```

### 4.3 禁止 shadowing

同一作用域内变量名唯一，重复声明编译报错。

```rp
let x: int = 1;
let x: int = 2;         // 编译报错：x 已定义
```

### 4.4 命名规范（强制）

| 类别 | 规范 | 示例 |
| --- | --- | --- |
| 变量 / 函数 / 模块 / 包 | snake_case | user_name、get_user |
| 常量 / 静态 | SCREAMING_SNAKE_CASE | MAX_SIZE、GLOBAL_COUNT |
| 类型（type） | snake_case | user、http_request |

**禁止**：匈牙利命名（`iCount`、`strName`、`pUser`）、驼峰变量名（`userName`、`getUser`）。违规输出 `rustc` 风格 warning，`--deny-warnings` 时升级为 error。

---

## 5. 三种数据结构

### 5.1 `ary`（固定字段命名结构）

**类型定义**（字段用 `:`）：

```rp
type address = [city: string, zip: string];
type user = [id: int, name: string, address: address];
```

**字面量**（字段用 `=`，不重复写类型）：

```rp
let mut u: user = [
    id = 1,
    name = "张三",
    address = [city = "北京", zip = "100000"]
];
```

**访问**：只用点号。

```rp
u.name                  // 读
u.address.city          // 嵌套读
u.name = "李四";         // 写，u 必须 let mut
```

**规则**：

- 字段名 key，点号访问，禁止 `u["name"]`、`u[0]`。
- 字段集合编译期封闭，运行期只能改值不能加字段。
- 要加字段：改 `type` 定义重编译，或定义新 type + 新变量（旧变量 `.clone()` 搬值）。
- 动态增长：把可变部分设计成 `vec` / `map` 字段，往字段里 `push` / `set`。

### 5.2 `vec`（有序隐式数字 key 序列）

**类型**：`vec<T>`（同构）、`vec<T1, T2>`（异构定长）、`vec`（内部定义）。

**三种合法写法**：

```rp
// 提前定义同构 + 简写
let v1: vec<int> = [1, 2, 3];

// 提前定义同构 + 显式
let v2: vec<int> = [:int = 1, :int = 2];

// 提前定义异构 + 简写
let v3: vec<string, int> = ["a", 1];

// 提前定义异构 + 显式
let v4: vec<string, int> = [:string = 'a', :int = 1];

// 内部定义（元素自带类型）
let v5: vec = [:int = 1, :string = "a", :bool = true];

// 空 vec，类型由声明确定
let v6: vec<int> = [];
```

**非法**：

```rp
let bad1: vec = [1, "a"];           // vec 空类型不能配简写
let bad2: vec<int> = [1, "a"];      // 同构声明，第二元素类型不符
let bad3: vec<string, int> = [1, "a"];  // 简写默认类型与声明不符
```

**访问**：

```rp
v.0                 // 编译期常量位置
v.at(i)             // 运行时位置，返回 result<T, string>
```

**方法**：

```rp
v.len() / v.is_empty()
v.push(x) / v.pop() / v.insert(i, x) / v.remove(i) / v.clear()
v.reverse()                            // 倒序，key 重排
v.sort(cmp: func(T, T) -> int)         // 排序，key 重排
v.map(f) / v.filter(f) / v.reduce(f, init) / v.find(f)
v.any(f) / v.all(f) / v.count(f)
v.slice(start, end) / v.concat(other) / v.unique() / v.flatten()
v.join(sep)                            // 元素是 string 时
v.chunk(n) / v.zip(other)
```

**规则**：

- key 始终有序 `0,1,2...`，不暴露。
- 同构 `vec<T>` 可 `push` / `pop` / `insert` / `remove` / `sort` / `reverse`。
- 异构 `vec<T1, T2>` 定长，结构修改方法编译报错，只能读。
- 禁止 `v[0]` 下标。
- 字面量简写 `[1, 2, 3]` 的元素默认类型：`0` → `int`，`"a"` → `string`，`'a'` → `string`，`1.5` → `float`，`true` → `bool`。

### 5.3 `map`（动态键值字典）

```rp
let mut scores: map<string, int> = [];
scores.set("张三", 90);
scores.get("张三");          // option<int>
scores.has("张三");          // bool
scores.delete("张三");       // option<int>
scores.len();
scores.keys();               // vec<string>
scores.values();             // vec<int>
scores.items();              // vec<[key: string, value: int]>
scores.merge(other);
scores.filter(func(k: K, v: V) -> bool);

let config: map<string, string> = ["name" = "张三", "city" = "北京"];
```

**规则**：

- `K` 必须可哈希（`string` / `int*` / `bool`）。
- 禁止 `scores["张三"]`，只用 `.get` / `.set` / `.has` / `.delete`。
- `.get` 返回 `option<V>`。
- 底层 HashMap，无序。

### 5.4 三者边界

| 需求 | 用 |
| --- | --- |
| 固定字段，编译期已知 | ary |
| 有序索引序列 | vec |
| 动态 key 字典 | map |

**不混用，不隐式互转。**

---

## 6. Copy 类型 vs Move 类型

### 6.1 分类

| 类别 | 类型 | 传参/赋值行为 |
| --- | --- | --- |
| Copy | int / i8 / i16 / i32 / i64 / u8 / u16 / u32 / u64 / float / bool | 复制，原变量仍可用 |
| Move | string / ary / vec / map / option / result / share / func | 默认所有权转移，原变量失效 |

### 6.2 示例

```rp
// Copy：赋值/传参不失效
let a: int = 1;
let b: int = a;
dev.log(a.to_string());    // a 仍可用

// Move：赋值即转移
let s: string = "hello";
let t: string = s;
// dev.log(s);             // 编译报错：s 已失效

let u: user = [id = 1, name = "张三"];
let v: user = u;
// dev.log(u.name);        // 编译报错：u 已失效
```

### 6.3 `.clone()`

**Move 类型必须提供** **`.clone()`** **方法**，返回同类型副本。用于"定义新变量搬值"场景避免 move。

```rp
let u: user = [id = 1, name = "张三"];
let u2: user = u.clone();     // u 仍可用
```

---

## 7. 函数与所有权

### 7.1 函数定义（唯一写法）

```text
func 名(参数, ...) -> 返回类型 {
    语句;
    return 值;
}
```

```rp
func add(a: int, b: int) -> int {
    return a + b;
}

func log(msg: string) -> void {
    // 无返回值写 -> void
}
```

规则：参数必须写类型；返回类型必须写，无返回写 `-> void`；不引入 `fn` / `def` / `function` / 箭头函数。

### 7.1.1 包内可见性 `self`

函数默认对外可见。加 `self` 修饰符后，只能在**声明它的包内**调用：

```rp
self func hash_password(raw: string) -> string {
    return raw;   // 仅供本包内部使用
}
```

规则：

- 写法唯一：`self func 名(...)`，`self` 在 `pub` 之前的位置上，不能与 `pub` 同时出现。
- `self func` 只能被同一个包（同一目录）内的函数调用；跨包调用、把函数当作值传递、作为回调传入都报编译错误。
- 检查发生在编译期，`rip check` / `rip run` / `rip build` 一致生效。
- 包边界以函数所在文件的 `mod` 声明为准；项目模式下每个目录是一个包。
- 未加 `self` 的函数保持当前的全局可见行为，`pub` 可写可不写。

### 7.2 参数修饰符（`read` / `write`，可用 `&T` / `&mut T` 写法）

| 写法 | 底层 Rust | 语义 |
| --- | --- | --- |
| 不写 | T | Copy 复制 / Move 转移 |
| read 名: T 或 名: &T | &T | 只读访问，原变量保留 |
| write 名: T 或 名: &mut T | &mut T | 可写访问，原变量保留，实参须 let mut |

规则：

- 参数修饰符只保留 `read` / `write` 两个关键字；`&T` / `&mut T` 是它们的位置等价写法，编译结果完全相同。
- 同一个参数只能用一种写法，`read u: &user` 这种混写直接报错。
- `&` / `&mut` 只出现在参数类型位置；`let x: &T` 不是合法类型。
- `mut` 单独出现只用于 `let mut`，**不作参数修饰符**（`&mut T` 里的 `mut` 是写法的一部分）。
- 生命周期仍由编译器推断，用户不写 `'a`。

```rp
func greet(read u: user) -> string {        // 等价于 u: &user
    return "Hi " + u.name;
}

func rename(write u: user, n: string) -> void {   // 等价于 u: &mut user
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

func consume(u: user) -> int {       // 默认，u move 进来
    return u.id;
}
```

### 7.3 调用

```rp
let mut u: user = [id = 1, name = "张三", address = [city = "北京", zip = "1"]];

let msg: string = greet(u);          // read，u 保留
rename(u, "李四");                    // write，u 保留且被改
let id: int = consume(u);             // 默认，u move，之后失效
```

### 7.4 生命周期

- 用户永不写 `'a`。
- 返回引用只能来自**唯一输入引用**：

```rp
func get_name(read u: user) -> read string {
    return u.name;
}
```

- 多输入引用返引用 → 编译报错，必须改签名。
- 编译器用 NLL + 收紧规则推断，推断不出直接报错。

### 7.5 所有权规则

| 规则 | 说明 |
| --- | --- |
| 默认不可变 | let 不可变，let mut 可变 |
| Copy/Move | Copy 复制，Move 转移 |
| 借用 | 同一时刻多个 read 或一个 write，不可混用 |
| 禁止部分移动 | ary 不拆字段 move |
| 禁止 shadowing | 同作用域变量名唯一 |
| 作用域 drop | 离开作用域自动释放，按 Rust 规则 |
| 无裸指针 / 无 unsafe | 用户层禁用 |
| 引用计数 + 环回收 | 容器即时释放；闭包与容器互指的环由周期性环回收器处理（sys.gc） |
| 跨线程 | 只用 share<T>，等价 Arc<T> |

### 7.6 闭包

```rp
let double: func(int) -> int = func(x: int) -> int {
    return x * 2;
};
```

规则：参数和返回类型必须写；捕获按借用规则自动处理；闭包类型必须显式。

### 7.7 泛型函数

类型参数写在函数名后的 `<...>` 里，可以被参数类型、返回类型和容器类型引用：

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

let n: int = identity(1);              // T 推断为 int
let s: string = identity<string>("a"); // 显式类型实参
```

规则：

- **推断或显式**：`identity(1)` 从实参推断；`identity<string>("a")` 显式指定，数量必须与声明的类型参数一致。
- **约束**：`func total<S: shape>(read items: vec<S>) -> float`，约束名必须是接口。函数体内 `S` 的方法按该接口解析（`item.area()`），调用点会检查实参类型是否实现该接口。
- 声明了却推断不出绑定（只出现在返回位置）的类型参数按 `any` 处理，不做隐式 `any` 到具体类型的赋值。
- **运行期类型擦除**：不做单态化，泛型函数在解释器 / 字节码 / AOT 里都只有一份实现，没有额外的运行期开销。
- 函数体内的类型参数在**没有约束**时是不透明类型：只能整体传递（作为普通参数、容器元素、返回值、`some` / `ok` 的载荷），不能调用方法、不能与字面量运算；需要这些操作时加接口约束，或改写为具体类型。
- 泛型函数不重载：同名函数无论类型参数是否不同都算重复定义，需要时换名字。
- `rip build` 的 AOT 产物同样支持泛型函数：类型参数只参与编译期检查，运行期不存在。

### 7.8 方法与接口

**方法**：`func 类型.方法(...)`，第一个参数是接收者，只能是 `read` 或 `write`。

```rp
type user = [id: int, name: string];

pub func user.display(read u: user) -> string {
    return u.name;
}

func user.rename(write u: user, name: string) -> void {
    u.name = name;
}

let mut u: user = [id = 1, name = "张三"];
u.display();          // 静态分派到 user.display(u)
u.rename("李四");     // write 接收者要求 u 是 let mut
```

规则：

- 完整名称是 `类型.方法`（如 `user.display`），在函数表里全局唯一；方法名在同一类型内唯一。
- 接收者必须显式写出（`read u: user` / `write u: user`），不引入 `self` / `this`。
- `write` 接收者要求接收者是可变变量或字段链；临时值（`make().rename(...)`）直接报错。
- 方法不能带注解、不能是泛型，也不能用 `self` 修饰。
- 方法可以 `pub`；类型的可见性仍由 `pub type` 决定。
- 枚举同样可以挂方法：`func status.blocked(read s: status) -> bool`，调用 `s.blocked()`。

**接口**：`type 名称 = interface { ... }`，只写方法签名，接收者固定为接口自身。

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
    dev.log(describe(s));      // 运行时按类名分派到 circle / square 的方法
}
```

规则：

- **隐式实现**：具体类型拥有接口要求的同名同签名方法即视为实现，不需要 `impl` 声明。
- 签名必须完全一致：参数类型、返回类型与接收者模式（`write` 对应 `write`）。
- 接口值可以放进 `vec`、作为参数/返回值、作为 `ary` 字段；调用时按运行时类名分派。
- 构造接口值用命名构造器（`circle[radius = 1.0]`）或已声明为具体类型的变量，运行时据此记录类名。
- 只有用户命名类型（`type x = [...]`）能实现接口；内建类型（`string` / `int` 等）不参与。
- 接口与 `type` 共用命名空间，重名直接报错；不能给接口写方法实现（`func shape.x` 报错）。

### 7.9 defer / 资源释放

`defer 调用;` 在**当前函数退出时**执行这次调用，用于关连接、解锁、清理临时资源。

```rp
func read_file(path: string) -> result<string, string> {
    let conn: conn = open(path);
    defer conn.close();          // 参数在 defer 处就地求值
    let content: string = conn.read()?;
    return ok(content);
}
```

规则：

- **只接受调用表达式**：`defer f(...)` / `defer x.m(...)` / `defer 模块.f(...)`；`defer 1 + 2;` 是语法错误。
- **实参在 defer 语句处立即求值**，函数退出时只执行调用本身（Go 语义）。因此 `defer f(x)` 捕获的是当时的值，之后改 `x` 不影响。
- **后进先出**：同一函数内多次 `defer` 按登记顺序的逆序执行。
- **所有退出路径都执行**：正常返回、`return 值`、`?` 传播、`return err(...)`、运行时错误都会触发当前帧的 `defer`。
- **按函数帧隔离**：`defer` 属于登记它的函数帧；内层函数返回时只执行内层登记的项。
- 返回值直接丢弃：`defer` 的调用返回值不参与任何表达式。
- `defer` 只能在函数体内使用：顶层与 `parallel` 分支里写 `defer` 报编译错误。
- `defer` 自身出错时，该错误覆盖函数原本的退出结果。

```rp
func demo() -> void {
    defer print("first");        // 最后执行
    defer print("second");       // 先执行
    print("body");
}
// 输出：body / second / first
```

---

## 8. 运算符

### 8.1 算术（同类型）

```text
+  -  *  /  %
```

规则：操作数必须同类型，结果同类型；不同整数类型禁止直接运算，必须 `.to_intX()` 转换；零除 panic。

### 8.2 比较（同类型，返回 `bool`）

```text
==  !=  <  >  <=  >=
```

### 8.3 逻辑（`bool`）

```text
&&  ||  !
```

### 8.4 字符串拼接

```rp
"a" + "b"       // "ab"
```

仅 `string + string`，无隐式转换。

### 8.5 类型转换

**唯一写法：目标类型方法**：

```rp
let a: u8 = 0b1010;
let b: int = a.to_int();

let s: string = "42";
let n: result<int, string> = s.to_int();
```

**禁止** **`as`** **作类型转换**。

---

## 9. 控制流

### 9.1 判断

```text
if 条件 { ... } else if 条件 { ... } else { ... }
if 条件 { ... } elif 条件 { ... } else { ... }
```

`elif` 与 `else if` 完全等价，两种写法都支持、可以混用；单行分支统一用 `elif` 更紧凑。

条件必须 `bool`，无 truthy/falsy，括号可省略。

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

### 9.2 循环（四种形态，规则同 Rust）

**形态 1：`loop`** —— 无条件循环，只能靠 `break` 退出。

```rp
let mut count: int = 0;
loop {
    if count >= 10 { break; }
    count = count + 1;
}
```

**形态 2：`while`** —— 条件是 `bool`，每轮先判断再执行。

```rp
let mut count: int = 0;
while count < 10 {
    count = count + 1;
}
```

**形态 3：`while let`** —— 每轮对表达式求值并匹配模式，匹配成立才执行循环体，不成立则结束循环。`=` 绑定模式，`=>` 引出循环体。

```rp
while let some(value) = cache.get("key") => {
    dev.log(value);
    cache.delete("key");
}
```

**形态 4：`for ... in`** —— 遍历 `vec`（元素）或 `map`（键）。

```rp
for item in tags {
    // item 类型由 tags 元素类型解构
}

for key in scores.keys() {
    // key: string
}
```

规则：

- `break` / `continue` 只作用于最内层循环；`continue` 回到条件判断（`for-in` 回到下一次迭代）。
- 循环条件必须是 `bool`，无 truthy / falsy。
- 没有 `do-while`，也没有 C 风格三段式 `for`。
- 循环体必须用花括号。

### 9.3 模式匹配

```text
match 表达式 {
    模式1 => { 语句 }
    模式2 => { 语句 }
}
```

**match 分支用 `=>`；`=` 只用于绑定与赋值。** 这是 `=>` 仅有的两处用法之一（另一处是 `while let`）。

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

规则：`match` 必须穷尽，编译器检查，用于 `option` / `result` / `ary` 解构与枚举变体；`while let` 不要求穷尽，匹配不成立就结束循环。

- 枚举模式写成 `枚举名.变体`，带字段的变体用 `[字段: 类型, ...]` 绑定字段（`_` 表示不绑定）。
- 穷尽检查按变体名：缺少任一变体都报 `match on enum 'x' must cover variant 'y'`。
- 模式里的枚举名必须与主体类型一致，字段名必须属于该变体，字段类型注解必须与声明一致。
- 枚举值不支持直接字段访问（`s.reason` 报错），必须通过 `match` 取出。
- 枚举模式同样可用于 `while let`：变体不符就结束循环，不要求穷尽。

### 9.4 错误传播

```rp
func read_config(path: string) -> result<string, string> {
    let content: string = read_file(path)?;
    return ok(content);
}
```

`?` 仅用于返回 `result` 的函数内；对 `err(e)` 立即返回 `err(e)`。

---

## 10. 模块与导入

### 10.1 模块声明（`mod`）

每个 `.rp` 文件首行：

```rp
mod user_api;
```

规则：

- 模块名必须等于文件所在目录名，完全一致，使用单段 `snake_case`，不写点分名。
- 同一目录下所有 `.rp` 文件必须声明同一个模块名。
- 子目录是独立模块，模块名同样等于该子目录名。
- 缺少 `mod` 声明、模块名与目录名不一致、同目录模块名不一致，都直接编译失败。
- `mod foo;` 不引入子模块文件：项目内 `.rp` 由项目加载器按目录递归收集并合并，`mod` 只声明当前文件的模块身份。

### 10.2 模块导入（`use`）

采用 Rust 风格的路径分段，但调用仍保持 Rip 的点号形式：

```rp
use std::http;
use std::json;
use std::sys as sys;
use github::com::user::pkg as pkg;
use rust::serde_json as json;
```

规则：

- 路径用 `::` 分段，不带引号；路径至少一段。
- `as` 后是别名，必须是合法标识符；`as` 是唯一重命名方式。
- 不起别名时，默认用路径最后一段作为别名（`std::http` → `http`）。
- 只导入模块，不支持逐项导入（`use std::http::serve;` 报错），符合「唯一写法」。
- 前缀 `std` 是标准库，`rust` 是 Rust crate，其余为第三方 Rip 包。
- 导入后调用写 `别名.函数(...)`，不引入 `::` 调用写法；值的方法调用仍是 `值.方法()`。
- 第三方包放在项目根 `packages/` 下（也接受 `package/`，两者同时存在时优先 `packages/`），`use` 路径 = 相对包目录的路径、`/` 换成 `::`：`packages/utils/http.rp` → `use utils::http;`；包名可以多段，`.rpa` 预编译包与 `.rp` 源码包等价。
- 第三方包在编译时与项目源码合并为同一编译单元，包内 `pub` 函数按普通函数名调用（`http.client(...)` 由别名解析到同名全局函数）；同名函数、类型、全局变量在合并后必须全局唯一，冲突会在类型检查前报错并给出两处声明位置。
- 包内源码文件不参与"模块名必须等于目录名"的校验，包身份以 `packages/` 下的路径为准。
- `import` 不是关键字：写 `import "std/http";` 只会得到语法错误。

### 10.3 可见性

默认模块内私有，`pub` 公开：

```rp
pub func serve(port: int, handler: func(http.request) -> http.response) -> void { ... }
pub type user = [id: int, name: string];
```

规则：只有"默认私有 / `pub` 公开"两级；不引入 `private` / `protected`。

### 10.4 包清单 `rip.toml`

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
exclude = ["storage", "data/db"]   # 不参与编译的目录（目录名或相对路径）

[build]
layout = "typed"     # flat（缺省，全部平铺）| typed（bin / lib / data / share）
target = "linux"     # 缺省为当前平台；只允许与宿主平台一致
copy = ["storage", "uploads"]
assets = ["public"]

[runtime]
log = "english"        # 运行时自身提示语言：english（缺省）| chinese

[server]
workers = 8                   # HTTP 工作线程数，0 或缺省为 CPU 核数
max_connections = 1024        # 连接队列上限，超出直接 503
max_body_bytes = 8388608      # 请求体上限，超出 413
max_header_bytes = 65536      # 请求头总字节上限
max_headers = 100             # 头部条数上限
read_timeout_ms = 30000
write_timeout_ms = 30000
keep_alive_ms = 5000          # keep-alive 空闲超时
max_requests = 1000           # 单连接最大请求数
health_path = "/healthz"      # 置空关闭
ready_path = "/readyz"
metrics_path = ""             # 非空时暴露 Prometheus 指标，如 "/metrics"

[cluster]
redis = "redis://127.0.0.1:6379/0"   # 集群共享底座；不需要可整段省略
task_lock = true                     # 定时任务用 Redis 租约选主
```

`[build]` 语义：

- `layout = "flat"`（缺省）：可执行文件、数据目录、静态资源、README 都在交付目录根部，和早期版本一致。
- `layout = "typed"`：`bin/` 放可执行文件，`data/` 放 `copy` 声明的数据与 `migrations/`，`share/` 放静态资源，`lib/packages/` 放第三方包副本（包已内嵌，`lib/` 仅便于交付后查看与替换）。
- 项目带有资源、迁移或运行期数据却仍是 `flat` 时，构建会打印一行 `hint` 建议改用 `typed`；不改变默认值，避免已有项目升级后启动脚本失效。
- `target`：产物格式遵循目标平台本地标准（Windows `.exe`，Linux/Unix/macOS 无扩展名 ELF/Mach-O）。缺省构建当前平台；写 `[build] target = "<triple>"` 或命令行 `--target <triple>` 可交叉编译。目标 triple 与宿主一致时直接调用内置编译组件（快速路径）；不同时改用 cargo 临时工程链接 `rp-core`，需要目标标准库、链接器与 C 依赖工具链（见 10.13）。`RIP_TARGET_LINKER` 可指定交叉链接器；工具链缺失时报错并打印所需的 triple，不会产出不可用的文件。
- `copy`：随包交付的运行期数据，工具链不预设目录名。

`rip build` 成功后会把交付目录打成同级的 `<目录名>.zip`（解压得到完整的 `dist/`），便于直接分发；数据目录很大时用 `--no-archive` 关闭。

### 10.5 第三方包与预编译包 `.rpa`

项目根目录约定：

```text
myapp/
├── main.rp
├── rip.toml
└── packages/
    ├── utils/http.rp          # 源码包：路径 utils/http
    ├── utils/strings.rp
    └── vendor/logger.rpa      # 预编译包：路径 vendor/logger
```

- 引入方式与源码包完全一致：`use utils::http;`、`use vendor::logger as logger;`。
- 源码包与预编译包可以混用；实际参与编译的是 `packages/` 下全部 `.rp` 与 `.rpa`。
  - 目录输入：递归合并目录内 `.rp`，产物为同目录 `<目录名>.rpa`；
  - 文件输入：产物为同名 `.rpa`；
  - 产物头部记录 `rip-package 1`、`rip-version` 与包名；`rip build` 遇到版本不一致会报错并拒绝加载。
- `packages/` 与 `tests/`、`dist/` 一样不按项目源码收集，只由包加载逻辑处理。
- 打包交付：包默认内嵌进可执行文件（`flat`、`typed` 都一样）；`typed` 布局会额外复制一份到 `lib/packages/`。需要额外分发源码包时直接复制项目根下的 `packages/` 目录即可。

项目模式（多文件编译单元）：

- 目录内存在 `rip.toml` 即视为一个项目，`rip run` / `rip dev` / `rip build` / `rip check` 都以项目为处理单位。
- 编译单元 = 入口 + 项目内递归收集到的全部 `.rp`；`tests/`、`build/`、`dist/`、`target/`、`node_modules/` 与点号开头的目录不参与编译；数据目录等由 `[compile] exclude` 声明（目录名或相对项目根的路径，如 `storage`、`data/db`），工具链不内置 `storage/`、`uploads/` 之类特判。
- 运行期数据目录（数据库、用户上传）的名字完全由代码决定：`http.save_file` 的第一个参数、`std/db` 的连接路径都是普通字符串，写哪个目录就存哪个目录；编译时如需跳过，把该目录写进 `[compile] exclude` 即可。
- `[compile] exclude` 与 `[build] copy` 独立：前者只影响源码收集，后者只影响打包复制。
- `rip test <项目目录>` 在项目模式下合并 `packages/` 后统一执行全部 `#[test]` 函数；没有 `#[test]` 时不执行 `main`（`main` 可能是常驻服务）。目录不是项目时，逐个 `.rp` 文件单独编译执行测试。
- 入口取 `[module] entry`；未配置时依次找 `<目录名>.rp`、`main.rp`、`src/main.rp`。入口必须包含 `func main() -> void`。
- 各文件仍各自声明 `mod 所在目录名;`，同目录同模块；跨文件调用直接写函数名，不需要为本地文件写 `use`（`use` 只用于 `std::*`、`rust::*` 与外部 Rip 包）。
- 同一编译单元内函数名、类型名、常量名唯一，重名直接编译失败。
- 诊断必须映射回原文件与行号：`相对路径:行号: error: ...`。
- 没有 `rip.toml` 时按单文件处理，行为与单文件模式完全一致。
- `[runtime] log` 控制**运行时自身提示**的语言：`"english"`（缺省）或 `"chinese"`，其它值按缺省处理。影响范围仅限运行时错误、HTTP 与 WebSocket 启动信息、后台任务与定时任务日志、`std/log` 级别标签；**不影响** Rip 程序自身的 `print` 输出，也不改变返回给程序的 `result` 错误值。编译期诊断（词法、语法、类型、所有权检查）统一使用英文。`rip build` 会把该配置写入产物，交付后无需再带 `rip.toml`。

### 10.6 Rust 包互操作（唯一方式）

**目标**：Rust 包在 Rip 里就是普通包，调用方式完全按 Rip 语法，**不暴露任何 Rust 特有语法**（无 `&`、无 `'a`、无 `unsafe`、无 `Box`、无 `impl`）。

**引入与调用**：

```rp
use rust::serde_json as json;

let s: result<string, string> = json.to_string(user);
let v: result<any, string> = json.from_str(s);
```

**类型映射（固定，不可扩展）**：

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

表中 `ary` 即 Rip 的结构类型，声明关键字可写 `type` / `struct` / `obj`，三者是严格的别名关系（见 3.4）。映射表固定，包作者不可扩展、不可新增别名。

**Rip 声明文件**：Rust 包作者必须提供 `.rp.decl` 接口声明，描述导出函数的 Rip 签名：

```rp
// serde_json.rp.decl
pub func to_string(v: any) -> result<string, string>
pub func from_str(s: string) -> result<any, string>
```

**安全边界**：

- `unsafe` 隔离在 Rust 包内，不传染用户层。
- 引用 `&T` / `&mut T` 不暴露给用户层，一律返回值或 `own` 语义。
- panic 必须被包捕获转为 `result`，禁止穿透到 Rip 运行时。
- Rust 包在**工具链发布阶段**就编译并静态链接进 `rip`，使用者机器不执行 `cargo`、也不需要 Rust 环境。
- `rip add rust:xxx` 只记录意图；某个 Rust 包能否使用，取决于当前 `rip` 是否已内置它。当前版本内置 `rust/serde_json`，新增 crate 需在发布 `rip` 时纳入（见 10.11）。

**约束**：

- `use rust::xxx;` 是唯一引入方式。
- 不引入 `extern` / `ffi` / `cgo` 等第二套机制。
- 类型映射固定，包作者不可扩展。
- **Rust 包可覆盖任意领域**（含 GUI、音视频、数据库驱动等），Rip 不限制包的类型，只约束调用方式。本版标准库不提供 GUI 封装，GUI 需求通过 `use rust::xxx;` 接入。
- 需要新增 Rust 能力而不重新发布 `rip` 时，使用 10.12 的原生包（动态库 + `.rp.decl`）；内置方式与新 crate 的取舍见该节。

### 10.7 命令

```text
rip run main.rp                    # 一次性执行（HTTP 默认监听 127.0.0.1）
rip run main.rp --host 0.0.0.0 --port 8080   # 对外提供服务
rip run main.rp --cert server.pem --key server.key   # HTTPS（PEM 证书链 + 私钥）
rip dev main.rp --port 8080        # 开发热重载
rip repl                           # REPL
rip build main.rp                 # 编译为原生二进制并输出到 dist/（程序 + 资源 + README）
rip build main.rp -o release      # 输出到指定目录
rip build main.rp --no-archive    # 不生成同级 dist.zip
rip build main.rp --native        # 实验性原生编译（标量 int 子集）
rip build main.rp --target x86_64-unknown-linux-gnu   # 交叉编译（需目标工具链）
rip profile main.rp               # 函数级耗时报告（--json / --top N）
rip dap                           # DAP 调试适配器（stdio；--port N 走 TCP）
rip pack packages/utils/http      # 把第三方包编译成 http.rpa
rip lock                          # 生成/更新 rip.lock
rip lock --check                  # 只校验，不写入（CI 用）
rip vendor                        # 拉取依赖到 packages/ 并生成 rip.lock
rip add github.com/user/rp-pkg     # 添加 Rip 依赖
rip add rust:serde_json            # 引用内置 Rust 包（互操作，无需本地 Rust 环境）
rip test                           # 运行全部 #[test]
rip test --coverage --coverage-min 80   # 语句覆盖率报告（低于阈值失败）
rip bench                          # 运行全部 #[bench] 并输出耗时统计
rip new shop --template http       # 创建项目骨架（http / cli / empty）
rip doctor                         # 环境 / 项目 / 依赖 / AOT 工具链自检
rip migrate up                     # 应用 migrations/ 下未执行的迁移（版本表 rip_schema_migrations）
rip migrate status                 # 查看已应用 / 未应用的迁移
rip migrate down --to 1            # 回滚到版本 1（该版本保留）
rip check main.rp --json           # 输出结构化诊断（CI 门禁；失败时同样输出 JSON）
rip test examples/s3_demo --filter signature   # 只跑名字含 signature 的测试
rip check main.rp                  # 静态检查
rip fmt main.rp                    # 基础格式化
```

公共选项：

| 选项 | 作用 |
| --- | --- |
| --offline | 只使用本地已有的包，要求 rip.lock 存在且校验通过 |
| --deny-warnings | 把警告当作错误（CI 用） |

`rip test` 的规则：

- 测试函数写成 `#[test] func test_xxx() -> void { dev.assert(...); }`，不接收参数。
- 项目模式合并项目源码与 `packages/` 后统一执行；非项目目录逐个文件收集，**目录扫描不执行 `main`**（`main` 可能是常驻服务），只编译检查并运行 `#[test]`；显式传入单个文件时才在无 `#[test]` 时执行 `main`。
- 输出 `test <文件>.<函数> ... ok` / `FAIL`，最后给出 `N passed, M failed`。
- `--filter <名称>` 只运行函数名包含该子串的测试。
- 隔离：每个 `#[test]` 使用全新运行时，模块级 `const` / `static` 与运行时资源（数据库连接、网络句柄、计时器）互不共享；`--no-isolate` 改为同一文件共用一个运行时（更快，但状态会串）。
- 并发：缺省按 CPU 核数并行执行测试（每个线程各自建运行时），`--jobs N` 指定线程数，`--jobs 1` 串行。结果按文件与声明顺序输出，不受执行顺序影响。`--no-isolate` 只能串行。
- 超时：缺省单用例 30 秒，`--timeout <毫秒>` 调整，`0` 关闭。超时在语句 / 循环迭代 / 内建调用检查点中断；正在阻塞的单次系统调用无法中途打断。测试用到共享外部资源（固定端口、同一个数据库文件）时，用 `--jobs 1` 避免相互干扰。

### 10.8 依赖锁定与静态检查

**`rip.lock`（依赖锁定）**：

- `rip lock` 扫描项目 `packages/`（或 `package/`）下全部 `.rp` / `.rpa`，为每个包记录包名、版本、来源路径、SHA-256 与直接依赖，写入项目根 `rip.lock`。
- `rip run` / `rip dev` / `rip build` / `rip check` / `rip test` 在存在 `rip.lock` 时自动校验：包内容被改动、包缺失、或新增了未记录的包，都会报错并提示重新执行 `rip lock`。
- 没有 `rip.lock` 时行为与过去一致（不校验）；`--offline` 模式下则**必须**有 `rip.lock`，保证离线环境不会拿到未锁定的依赖。
- `[registry] source` 记录私有包源地址（写入 `rip.lock` 的 `registry` 字段）；`[registry] offline = true` 等价于始终传 `--offline`。

```toml
[registry]
source = "https://rip.example.com"
offline = false
```

**静态检查（`rip check` / 编译期告警）**：

- 未使用变量：`let` 声明的局部变量从未被读取；以 `_` 开头的名字不检查。
- 未使用导入：`use` 的别名既没用于调用也没用于 `pkg.type` 类型；只告警。第三方包（`packages/` 下）在编译期与源码合并，调用不走别名，因此不参与该判定。
- 未使用私有函数：`self func` 从未被调用（带路由/任务/测试注解的函数不算）。
- 包间循环依赖：`packages/` 内两个及以上包互相 `use` 时输出 `warning: package cycle: a -> b -> a`。
- 以上均为 warning，`--deny-warnings` 时可升级为失败。

**格式化（`rip fmt`）**：

- 统一 4 空格缩进：按 `{}` / `[]` / `()` 层级重排，行首闭合符号回到对应层级。
- 去掉每行行尾空白，连续空行压缩为一行，文件以一个换行结束。
- 相邻的 `use ...;` 块按路径排序。
- 三引号模板字符串与 `/* */` 注释内部保持原样，不重排。
- 支持文件或目录（递归）；`--check` 只检查并以退出码 1 表示「需要格式化」，`--stdout` 打印结果不写回。

### 10.9 跨平台构建完整示例

**产物形态对照**：

| 形态 | 命令 | 构建依赖 | 产物内含 | 适用 |
| --- | --- | --- | --- | --- |
| 源码执行 | rip run main.rp | rip | 解释器 + 源码 | 开发、热更新 |
| 子集原生 | rip build main.rp --native | rip（内置编译组件） | 仅标量 int 子集代码 | 计算密集验证 |
| 完整 AOT（回退） | rip build main.rp | rip + 兼容运行时库 | 全语言原生代码 + 解释器目标码 | 兼容旧工具链 |
| 完整 AOT（纯净，缺省） | rip build main.rp | rip + 纯净运行时库 | 全语言原生代码，无解释器/VM、无源码 | 生产交付 |

> 交付给使用者的 `rip` 已经包含 `toolchain/`（内置编译组件）与 `lib/rip/`（AOT 运行时库），因此上表所有命令都只依赖 `rip` 本身，不需要另装编译工具。从源码构建工具链的开发流程见 10.11。

**1）准备工具链（三平台相同）**

```bash
cd /path/to/rip
cargo build --release                                       # target/release/rip[.exe]
cargo build --release -p rp-core --target-dir target/aot     # 纯净宿主：关闭 interp feature
```

**2）构建纯净 AOT**

Windows（PowerShell）：

```powershell
$env:RIP_AOT_DEPS = "target/aot/release/deps"
.\target\release\rip.exe build main.rp --aot
.\dist\main.exe --port 8080
```

Linux / macOS：

```bash
export RIP_AOT_DEPS=target/aot/release/deps
./target/release/rip build main.rp --aot
./dist/main --port 8080
```

- 不设 `RIP_AOT_DEPS` 时会在 `rip` 同级的 `deps/` 里找 `librp_core-*.rlib`；找不到就回退 `librp_runtime-*`，并打印 `note: ... interpreter included`。
- `RIP_AOT_DEPS` 是普通路径，Windows 可写成 `target/aot/release/deps`，无需绝对路径。

**3）产物校验**

```bash
# 三平台通用：纯净 AOT 不应出现 VM 与语言前端
grep -a "rp-runtime/src/vm.rs" dist/main   # 无输出（Windows 为 rp-runtime\src\vm.rs）
grep -a "rp_parser" dist/main              # 无输出

# Linux
file dist/main
ldd dist/main

# macOS
file dist/main
otool -L dist/main

# Windows
dumpbin /headers dist\main.exe | findstr PE32
```

**4）平台差异**

| 平台 | 链接器 | 额外依赖 | 说明 |
| --- | --- | --- | --- |
| Windows（MSVC） | 系统 link.exe | 无 | rustup 默认工具链即可 |
| Windows（MinGW） | rust-lld | 无 | 仓库 .cargo/config.toml 已设置；避免含空格的 GNU ld 路径问题 |
| Linux | 系统 cc | pkg-config、libssl-dev | tiberius 走 native-tls |
| macOS | 系统 clang | Xcode Command Line Tools | — |

- 宿主目标（与 `rustc -vV` 的 `host:` 一致）走本机快速路径；其他 triple 走 cargo 交叉构建：临时工程以 path 依赖 `crates/rp-core`，在 `<repo>/target/<triple>/release/` 产出后复制到交付目录。
- 交叉编译需要：`rustup target add <triple>`、目标链接器（`.cargo/config.toml` 或 `RIP_TARGET_LINKER`）、以及 sqlite / openssl 等 C 依赖的交叉工具链。
- 源码树位置按 `RIP_AOT_SOURCE` > 可执行文件所在仓库 > 编译期记录的仓库根查找；安装版需 `RIP_AOT_SOURCE`。
- `--native` 子集编译不支持交叉，会直接报错。

**5）链接错误排查**

`rip build --aot` 直接调用内置编译组件，不经过包管理器。构建脚本声明的原生库搜索路径、`-lstatic=` / `-framework` 与链接参数会自动从 `lib/rip/build/*/output`（开发机上为 `target/*/release/build/*/output`）收集；排查时设置 `RIP_AOT_DEBUG=1` 会打印实际执行的编译命令。

| 报错 | 常见原因 | 处理 |
| --- | --- | --- |
| cannot find -lssl / -lcrypto | Linux 未装 OpenSSL 开发包 | apt install pkg-config libssl-dev |
| ld: framework not found ... | macOS 缺 Command Line Tools / SDK | xcode-select --install |
| linker 'link.exe' not found | Windows 缺 MSVC 生成工具 | 安装 VS Build Tools，或改用 MinGW 工具链 |
| cannot find -lsqlite3 | RIP_AOT_DEPS 指错目录，build 输出缺失 | 去掉 RIP_AOT_DEPS 后重新 `rip build`（运行时库会自动构建） |
| unable to find library -lwinapi_* | MinGW 的 GNU ld 路径含空格 | 保留仓库 .cargo/config.toml 的 rust-lld 设置 |
| 写产物 Permission denied | 旧二进制仍在运行 | 先停止旧进程再构建 |

**6）交付目录**

`rip build` 的交付规则：`README.md`、`[build] copy` 声明的数据目录、`migrations/`、`public/`/`assets/`/`static/`、`typed` 布局下的 `lib/packages/`。整个目录拷走即可运行；构建结束后还会生成同级的 `<目录名>.zip`，`--no-archive` 可关闭。

`rip run` 与 `rip build` 的运行时错误都会带上完整调用栈（`at inner (line 3)` 这类）：AOT 生成的函数在入口登记帧、出口按深度截断，与解释器的 `call_decl` 行为一致。部署后的参数调整见 10.13。

**7）三平台 CI**

完整配置见仓库 [`.github/workflows/ci.yml`](../.github/workflows/ci.yml)，核心步骤：

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

CI 还会断言 AOT 产物不含 `mod ...` 源码、`rp_parser` / `rp_lexer` 与 `rp-runtime/src/vm.rs`，并上传三平台产物。

### 10.10 包管理完整示例

下面从零做一个纯 Rip 包，覆盖「编写 → 引用 → 测试 → 预编译分发 → 锁定 → 离线」。

**1）项目结构**

```text
slugdemo/
├── main.rp
├── rip.toml
└── packages/
    └── text/
        └── slug.rp        # 包路径 = text/slug
```

`rip.toml`：

```toml
[module]
name = "slugdemo"
version = "0.1.0"
entry = "main.rp"

[build]
copy = ["data"]        # 有 data/ 才复制；没有会提示 warning，可删掉这行
```

`packages/text/slug.rp`：

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

`main.rp`：

```rp
mod slugdemo;

use std::dev;
use text::slug;

func main() -> void {
    dev.log(slug("Hello Rip World"));
}
```

**2）引用与测试**

```bash
rip run main.rp          # [log] hello-rip-world
rip test .               # 1 passed, 0 failed
```

要点：

- `use text::slug;` 的路径 = `packages/` 下的相对路径（去扩展名）。
- 第三方包与项目源码在编译期合并，`pub func slug` 直接按函数名调用；不需要 `text.slug(...)`。
- 同一编译单元内函数名全局唯一，两个包不能导出同名函数。

**3）预编译分发 `.rpa`**

```bash
rip pack packages/text                    # 产物：packages/text/text.rpa
cp packages/text/text.rpa ../other-app/packages/text.rpa
rm -rf packages/text                      # 同一项目里不要同时保留 .rp 与 .rpa
```

使用方目录：

```text
other-app/
├── main.rp                 # use text::slug;
├── rip.toml
└── packages/
    └── text.rpa            # 路径 = text
```

- `.rpa` 是带 `rip-package 1` 头部与 `rip-version` 的源码归档，和 `.rp` 等价，可混用。
- **同一包不要同时存在两种形态**：`packages/text/slug.rp` 与 `packages/text.rpa` 会让 `slug` 定义两次，报 `function 'slug' is already defined`。

**4）依赖锁定与离线**

```bash
rip lock            # 生成/更新 rip.lock
rip lock --check    # 只校验（CI 用）
rip run main.rp --offline
```

`rip.lock` 示例：

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

- 有 `rip.lock` 时，包内容被改动、包缺失或新增未记录的包都会报错，提示重新执行 `rip lock`。
- `--offline` 要求 `rip.lock` 存在且校验通过；`[registry] offline = true` 等价于始终离线。

**5）`[dependencies]` 与 `[registry]`**

```toml
[dependencies]
"text" = "^0.1.0"

[registry]
source = "https://rip.example.com"
offline = false
advisories = "https://rip.example.com/v1/advisories.json"   # 可选，缺省为 <source>/v1/advisories.json
```

- 包源支持本地目录、`file://` 与 `https://`。**在线包源强制 https**：只有 `localhost` / `127.0.0.1` / `::1` 允许 `http://`（本地开发用），其它 `http://` 源直接报错拒绝，防止中间人替换包内容。配置了**在线包源**时，`rip lock` / `rip vendor` / `rip run` 会按 `[dependencies]` 解析约束、求解版本、下载并校验 `sha256`，解包到 `packages/`；**远端源的下载必须带索引 `sha256`，缺摘要即拒绝下载**（本地目录源不受此限制）。离线（`--offline` 或 `[registry] offline = true`）只用已存在的本地包，并要求 `rip.lock` 校验通过。
- **缺省公共包源**：没有写 `[registry] source` 时使用 `https://registry.rip-lang.dev`，因此 `rip search` / `rip info` / `rip add` / `rip lock` 开箱即用。优先级：`--registry` > `[registry] source` > 环境变量 `RIP_REGISTRY` > 公共包源；网络不可用时错误信息会带上当前使用的源与换源方法。
- `rip pub` **不会**回退到公共包源：必须用 `--registry`、`[registry] source` 或 `RIP_REGISTRY` 显式指定发布目标，避免误发。
- `[dependencies]` 已全部 vendored 到 `packages/` 且满足约束时，`ensure` 不走网络（`rip run` / `rip build` / `rip lock` 都不会被包源拖住）；只有缺包或版本不满足时才按约束求解并拉取。
- **版本求解**：有界深度优先 + 回溯。同一包只选一个版本；候选按版本降序尝试，遇到依赖冲突时回退到更早的候选。无解时报出每条约束的来源（`rip.toml` 或某个上游包的具体版本）。
- **包源索引**：`v1/index/<name>` 为单个包的全部版本；`v1/index.json` 为在线索引用目录；归档位于 `v1/packages/<name>/<version>.zip`，索引里记录 `sha256`。`rip pub` 写本地包源时会自动重建 `v1/index.json`。
- `[dependencies]` 的版本是 `semver` 约束（`^0.1.0`、`~1.2`、`1.2.*`、`*` 等）；`rip add <包>[@约束]` 写入清单后立即拉取（等价于接着执行 `rip lock`）。
- `[registry] source` 会写入 `rip.lock` 的 `registry` 字段，便于审计来源。
- `rip search <关键词>` 与 `rip info <包名>` 可查看包源内容（`--registry` 临时覆盖源，`--json` 输出结构化结果）。

**包源目录布局**：

```text
registry/
└── v1/
    ├── index.json
    ├── advisories.json
    ├── index/
    │   └── acme.text          # 每行一个版本：版本、sha256、依赖
    └── packages/
        └── acme.text/
            └── 1.2.0.zip
```

**依赖审计（`rip audit`）**：校验 `rip.lock` 与 `packages/` 是否一致，匹配安全公告，并列出许可证。

- 结构问题（缺 `rip.lock`、格式错误、包内容与校验和不符、新增未记录的包）始终判定失败。
- 公告来源优先级：`[registry] advisories` > 环境变量 `RIP_ADVISORIES` > `<source>/v1/advisories.json`。公告字段 `id` / `package` / `requirements`（semver 约束）/ `severity`（`low` / `moderate` / `high` / `critical`）/ `title` / `url`，文件可以是裸数组或 `{"advisories": [...]}`。
- `--deny <LEVEL>` 设定失败阈值（缺省 `high`；`none` 表示不因公告失败），`--json` 输出机器可读结果，`--licenses` 打印每个包的许可证（取包内 `rip.toml` 的 `[module] license`）。

**6）常见错误对照**

| 报错 | 原因 | 处理 |
| --- | --- | --- |
| mod name 'x' must match directory name 'y' | 目录名与 mod 声明不一致 | 改名目录或改 mod 行 |
| function 'slug' is already defined | 同一包同时存在 .rp 与 .rpa，或两个包导出同名函数 | 删除其中一种形态 / 改名函数 |
| package "text/slug" is not available in this build | use 路径与 packages/ 结构不一致 | 按相对路径修正 use |
| rip.lock: ... has changed | 包内容改动后未重新锁定 | rip lock |
| unused import "..." | 只对 std::*、rust::* 生效 | 第三方包 use 不产生该告警 |

### 10.11 分发与安装：单文件工具链

交付给使用者的只有两样东西：**已编译的 `rip`** 与**它自带的运行库目录**。使用者不需要安装任何编译工具，也不需要看到 Rip 的实现源码。

**1）交付目录结构**

```text
rip/
├── rip.exe                          命令行入口（已编译的运行时）
├── toolchain/
│   ├── bin/                         内置编译组件与运行期 DLL
│   └── lib/rustlib/<host>/          目标平台的运行库与链接器
├── lib/rip/
│   ├── deps/                        AOT 运行时库（librp_core-*.rlib 及依赖）
│   ├── build/                       依赖的构建产物（原生库搜索路径）
│   └── native/                      外部原生库（如 winapi 导入库）
└── README.md
```

- `rip` 按**自身所在目录**定位 `toolchain/` 与 `lib/rip/`，整个目录可以移动到任意路径，不需要配置环境变量。
- 每个平台的 `deps` 与 `toolchain` 都是该平台专属产物，不能跨平台混用。

**2）开发方组装（从源码发布）**

Windows：

```powershell
cargo build --release
cargo build --release -p rp-core --no-default-features --target-dir target/aot
powershell -ExecutionPolicy Bypass -File scripts/bundle-toolchain.ps1
# 产物：dist/rip/（约 1.4 GB，内含编译组件与全部运行库）
```

Linux / macOS 等价步骤（按同样的布局手工复制）：

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

**3）使用者视角**

```bash
rip run main.rp                 # 解释执行：源码解析跑在已编译的运行时上
rip build main.rp               # 原生编译：只调用 rip 自带组件，无需额外安装
```

- 两条命令都只依赖 `rip` 目录本身；`rip build` 的编译组件、链接器、运行时库全部在包内。
- Rust 互操作（`use rust::...;`）所需的 crate 在发布 `rip` 时已静态链接进运行时，使用者侧同样不需要 Rust 环境；例如 `rust/serde_json` 在 `rip run` 与 `rip build` 下都可直接使用。
- 项目若需要新的 Rust crate，由 SDK 发布方在构建 `rip` 时纳入，使用者升级 `rip` 即获得，不需自行安装或编译。
- 构建中间产物（宿主源码）只写入系统临时目录并在编译结束后删除，**交付目录里不会出现任何实现源码**。
- `RIP_TOOLCHAIN` / `RIP_AOT_DEPS` 仅用于排障或自定义安装位置，正常使用无需设置。
- 缺失组件时的提示是「请安装完整版 rip」，而不是要求安装外部工具链。

### 10.12 原生包（Rust 插件）

原生包让 Rip 调用 Rust 实现的能力，**使用者侧仍然只需要 `rip`**。签名来自 `.rp.decl`，实现来自平台动态库（AOT 下可直接静态链接）。

**1）目录约定**

```text
<root>/acme/tools/
├── package.decl      # 接口声明（唯一签名来源）
├── tools.dll         # Windows；Linux 为 libtools.so，macOS 为 libtools.dylib
└── libtools.a        # 可选：静态库（Rust staticlib），供 --aot 静态链接
```

包路径即相对路径，代码里写 `use acme::tools;`。搜索根目录按顺序为：

1. `RIP_NATIVE_PATH`（多个用路径分隔符隔开，排障/自定义安装用）
1. `<rip>/lib/rip/plugins`：随工具链分发的原生包
1. `<可执行文件>/lib/native`：随交付产物分发的原生包
1. 当前工作目录下的 `lib/rip/plugins` 与 `lib/native`——**仅在 `rip` CLI（源码运行与构建）下搜索**；打包交付的二进制不搜索工作目录，防止在恶意目录启动时误加载外部原生包

**2）声明文件**

```rp
abi = 1
platform = "windows-x86_64"
sha256 = "…"                 // 可选：动态库摘要，填了就强制校验
prefix = "acme"              // 可选：符号前缀，多个原生包共存时使用

pub func shout(text: string) -> string
pub func fail(msg: string) -> result<string, string>
```

- `abi` 必须与当前 `rip` 一致（本版为 `1`）。
- `platform` 为 `<os>-<arch>`，`x64`/`amd64` 与 `x86_64` 等价。
- `prefix` 只允许 ASCII 字母、数字与 `_`。
- 类型支持 `int`/`float`/`string`/`bool`/`any`/`void`、`vec<T>`、`map<K,V>`、`option<T>`、`result<T,E>`、`share<T>`。

**3）插件 ABI**

动态库导出四个 C 符号：

```c
uint32_t rp_native_abi(void);
const uint8_t *rp_native_decl(size_t *out_len);      // 返回与 package.decl 相同的内容
uint8_t *rp_native_call(const uint8_t *name, size_t name_len,
                        const uint8_t *args, size_t args_len,
                        size_t *out_len);
void rp_native_free(uint8_t *ptr, size_t len);
```

- 参数是 JSON 数组，返回值是 `{"ok": …}` 或 `{"err": "…"}`。
- 声明里写了 `prefix = "acme"` 时，四个符号名统一变成 `rp_native_acme_abi` / `rp_native_acme_decl` / `rp_native_acme_call` / `rp_native_acme_free`。
- 声明返回 `result<T,E>` 的函数：`err` 会变成 `err(...)` 值；返回其它类型的函数：`err` 作为运行时错误抛出。
- panic 必须在插件内 `catch_unwind` 拦住，禁止穿透 ABI。
- 加载时校验：ABI 版本、`platform`、`sha256`、以及插件内置声明与 `package.decl` 是否一致。

**4）两种执行方式**

| 方式 | 加载时机 | 交付要求 |
| --- | --- | --- |
| rip run | 首次调用时从搜索根目录加载 | 原生包目录已就位（CLI 模式的工作目录或 RIP_NATIVE_PATH） |
| rip build | 有静态库时编译期直接链接，无静态库时运行期加载 | 静态链接的包无需交付；其余复制到 <产物>/lib/native/<包路径> |

- **静态链接快速路径**：包目录里存在 `lib<name>.a`（Linux/macOS/Windows-GNU）或 `<name>.lib`（Windows-MSVC）时，

`rip build` 会用 `#[link(name = "…", kind = "static")]` 直接并进可执行文件，产物仍是单文件，不生成 `lib/native/`；启动时校验 ABI 版本，签名仍来自 `package.decl`。插件用 `crate-type = ["cdylib", "staticlib"]` 即可同时提供两种形态。

- `.dll.a` / `.dll.lib` 是动态库导入库，不会被当作静态库。
- **多个原生包共存**：`package.decl` 里写 `prefix = "acme"` 时四个 ABI 符号带前缀，可以任意多个带前缀的包同时静态链接进一个产物。

没有前缀的包共用固定符号名，一个产物里只能静态链接一个；多余的按包路径排序退回动态加载，并在构建时给出提示。

- `rip build` 只会复制**程序实际 use 的原生包**，`README.md` 会逐条列出（静态链接的会标注「已静态链接」）。
- 原生包是二进制文件，不受“交付物不含源码”的影响；没有使用原生包（或全部静态链接）时产物仍是单文件。

**5）最小插件实现**

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

编译：`cargo build --release`（`[lib] crate-type = ["cdylib"]`；希望 AOT 静态链接则写 `["cdylib", "staticlib"]`），把动态库（以及可选的静态库）与 `package.decl` 放进上述目录即可。需要多包共存时，把四个 `#[no_mangle]` 函数按上表改成带前缀的名字，并在 `package.decl` 里写同样的 `prefix`。

### 10.13 运行时配置

构建期会把 `rip.toml` 的 `[server]`、`[cluster]`、`[db]`、`[runtime]` 固化进产物。部署后要调整这些参数，不需要重新构建。

**1）配置文件**

在可执行文件旁放 `config/rip.toml`：

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
otlp_endpoint = "http://127.0.0.1:4318"   # 留空关闭追踪导出
otlp_service = "myapp"
otlp_sample_ratio = 0.1

[cluster]
redis = "redis://127.0.0.1:6379/0"
task_lock = true

[db]
max_open = 32            # 每个 DSN 允许同时存活的连接（空闲 + 借出）
max_idle = 8             # 每个 DSN 缓存的空闲连接上限
acquire_timeout_ms = 5000
idle_timeout_sec = 300
```

查找顺序（命中即用）：`RIP_CONFIG` 指定的路径 → `<可执行文件目录>/config/rip.toml` → `./config/rip.toml`。UTF-8 带 BOM 也能识别。

**2）环境变量**

容器与编排环境里不必挂文件：

| 变量 | 覆盖项 |
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
| RIP_DISABLE_SHELL | 设置后禁用 `process.run_shell`（运维安全开关，调用直接返回错误） |
| RIP_FS_DENY | 运维路径拒绝开关：平台路径分隔符隔开的目录前缀列表（如 `/etc:/proc` 或 `C:\Windows;C:\Program Files`），`std/fs` 任何操作的路径参数落在前缀之下直接报错；进程启动时快照，代码无法绕过 |
| RIP_HTTP_BLOCK_PRIVATE | 设置后 `http.fetch` / `fetch_file` / `download` 拒绝对回环、私网、链路本地、唯一本地与组播地址的出站访问（SSRF 防护，默认关闭） |
| OTEL_EXPORTER_OTLP_ENDPOINT | [server] otlp_endpoint |
| OTEL_SERVICE_NAME | [server] otlp_service |
| OTEL_TRACES_SAMPLER_ARG | [server] otlp_sample_ratio |

**3）优先级**

`命令行参数 > 环境变量 > config/rip.toml > 构建期内嵌值`

命令行可覆盖的只有 `--port` / `--host` / `--cert` / `--key`，其余项走环境变量或配置文件。`rip run` 与 `rip build` 产出的程序使用同一套规则。

**4）部署隔离建议**

Rip 的定位是「代码即信任」：`std/fs` / `std/process` 不设语言级沙箱，`RIP_FS_DENY` / `RIP_DISABLE_SHELL` 只是运维侧的进程内护栏（防误用，不防恶意代码），真正的安全边界在操作系统层。生产部署建议：

- 用专用低权限系统账户或容器运行服务进程，文件权限只放开应用自身的数据目录；
- 需要限制文件访问面时设置 `RIP_FS_DENY`，需要封禁 shell 逃生口时设置 `RIP_DISABLE_SHELL`；
- 交付产物只从 exe 相对目录与 `RIP_NATIVE_PATH` 加载原生包，不搜索工作目录；原生包声明里尽量填 `sha256`。

---

## 11. 标准库

### 11.1 命名空间

| 命名空间 | 职责 |
| --- | --- |
| std/sys | 进程、环境、信号、系统信息 |
| std/fs | 文件、目录、路径 |
| std/time | 时间、日期、定时器、IANA 时区（见 11.14） |
| std/net | TCP / UDP |
| std/http | HTTP 服务端 / 客户端 / 路由 |
| std/ws | WebSocket |
| std/json | JSON 编解码 |
| std/db | 数据库连接、事务、关系查询、指标（见 11.12） |
| std/str | 字符串工具 |
| std/vec | 序列工具 |
| std/map | 字典工具 |
| std/crypto | 哈希、编码、随机数、密码哈希、AES、RSA（见 11.6） |
| std/xml | XML 解析与序列化（见 11.7） |
| std/encode | URL / HTML / CSV / 查询串编解码（见 11.8） |
| std/task | 后台任务、定时任务调度（见 11.5） |
| std/sync | 命名互斥锁、原子整数、通道（见 11.5） |
| std/dev | 调试、日志、断点、计时（release 剔除） |
| std/log | 分级日志 |
| std/redis | Redis 客户端、分布式锁、集群限流（见 11.11） |
| std/cache | 进程级 TTL + LRU 缓存（见 11.12） |
| std/queue | 基于 SQLite 的持久化任务队列（见 11.12） |
| std/cli | 参数、子命令、交互输入（见 11.12） |
| std/llm | 大模型统一调用 |
| std/zip | ZIP 归档读写（见 11.14） |
| std/mail | SMTP 邮件发送（见 11.14） |
| std/reflect | 运行时类型信息与动态派发（见 11.15） |

**无** **`std/tpl`**，HTML 模板后续包扩展。

### 11.2 HTTP（返回格式由 `return` 决定）

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

**核心规则**：

- `http.response` 只是普通 `ary`，**用户** **`return`** **什么就是什么，框架不强制任何格式**。
- `body` 字段：返回原始字符串，**框架不设** **`Content-Type`**，用户自己写 `headers`。
- `json` 字段：**可选便利糖**。用户把 `ary` / `vec` / `map` 赋给 `json` 字段时，框架自动序列化并设 `Content-Type: application/json`。
- `sse` 字段：用户主动选择 SSE 时用。响应头先发出，事件逐帧 flush，客户端断开即结束；一条连接占用一个解释器线程，长连接多时提高 `[server] workers`。
- **`body`** **/** **`json`** **/** **`sse`** **三选一**，编译期检查互斥。
- **中间件**：`http.chain([mw...], handler)` 按顺序包裹，首次请求时组合并缓存；`mw` 是 `func(下一级 handler) -> 新的 handler`。`http.cors("*")` 自动处理 `OPTIONS` 预检，`http.security_headers()` 补安全头，`http.with_headers(map)` 追加固定头，`http.rate_limit(rate, burst)` 按客户端 IP 做路由级限流。`http.group(前缀, [mw...], [routes...])` 让组内路由写相对路径，整组注册为一条前缀路由。
- **OpenAPI**：`http.openapi(router, title, version)` 从路由表生成 OpenAPI 3.0 JSON（路径、方法、`operationId`、路径参数），在路由表构建完成后调用一次即可。
- **请求上下文**：`req.ctx` 是 `map<string, any>`，中间件写入（如当前用户），处理函数读取，同一请求内共享同一个 map。
- **响应压缩**：1KiB 以上的文本类响应按 `Accept-Encoding` 自动 gzip，并补 `Vary: Accept-Encoding`；静态文件与 SSE 不压缩。无需在业务代码里处理。
- **静态资源**：`http.static` 响应带 `ETag` / `Last-Modified` / `Cache-Control`，支持 `If-None-Match`（`304`）与单段 `Range`（`206` / `416`），文件从磁盘流式发送，不占解释器内存；HTML 用 `no-cache`，其余长缓存。
- **会话与校验**：`std/session` 提供 HMAC-SHA256 签名的无状态 cookie（`save` / `cookie` / `load` / `clear` / `csrf_token` / `csrf_check`），多实例无需共享存储；`std/validate` 把字段校验写成一行并返回可读错误。`std/jwt` 提供 HS256 的 `sign` / `verify`（校验强制算法为 HS256、拒绝 `none`、默认强制校验 `exp`，可选传入 `aud` / `iss` 做受众与签发方校验）。密钥必须来自环境变量或配置。`Set-Cookie` 的 `Secure` 属性判定：服务端启用 HTTPS（`--cert` / `--key`）时自动附加；纯 HTTP 部署在可信代理之后（`trust_proxy = true`）时，`cookie(value, ttl, request)` 与 `clear(request)` 传入当前请求即按 `X-Forwarded-Proto` 最右值判定是否附加 `Secure`（不传请求参数则保持旧行为，不加 `Secure`）。
- **模板**：`std/template.render(source, data)` 默认 HTML 转义，支持 `if` / `unless` / `each` / 注释；编译结果按源码缓存在线程本地，适合服务端渲染。
- 返回 HTML：`body` + `headers = ["Content-Type" = "text/html; charset=utf-8"]`。
- 返回 JSON：用 `json` 字段（糖），或自己 `json.encode` 后放 `body` + 手写头（手动）。**两种都允许**，语义不同，不算第二写法。
- `multipart/form-data` 请求自动解析：普通字段并入 `form`，文件进入 `files`；`http.save_file(目录, 文件)` 解码 `data_base64`、安全化文件名后写入目录（目录不存在时自动创建），返回落盘路径或 `err(...)`。目录参数是普通字符串，由代码决定，运行时没有任何内置上传目录。
- `http.fetch` 是出站请求：支持 `http` / `https`（系统根证书校验）、任意方法与请求头、字符串请求体，返回 `status` / `headers` / `body`。用于调用第三方 API、Webhook 等；目标地址与凭据全部由代码给出，运行时不做白名单或目录限制。`http.fetch_file` 与之一致，但请求体直接读取文件原始字节，适合上传图片、视频等二进制内容。`http.download(url, headers, path)` 把响应体原始字节写入文件，返回字节数，适合下载二进制对象。连接超时 30 秒，读写超时 60 秒；`HEAD` 只读响应头，不读 body。响应体上限 256 MiB（`Content-Length` 预检 + 读取封顶），超限返回 `err(...)`，防止对端恶意大响应耗尽内存。设置环境变量 `RIP_HTTP_BLOCK_PRIVATE=1` 后，`http.fetch` / `fetch_file` / `download` 会拒绝对回环、私网（RFC1918）、链路本地、唯一本地与组播地址的访问（SSRF 防护开关，默认关闭以保持既有内网调用行为）。
- **对象存储属于 Rip 层，不在运行时内置**：S3 / OSS / COS 都是公开的 HTTP 协议 + 签名规范，用 `http.fetch` / `http.fetch_file` / `http.download` 加 `std/crypto`（`sha256`、`hmac_sha256_hex`、`sha256_file`）与 `std/encode`（`url_encode`）即可在 Rip 代码或第三方包里实现，仓库自带 `examples/s3_demo/packages/s3` 作为参考实现（AWS SigV4，兼容 MinIO / R2 / B2 等 S3 兼容服务）。运行时只提供通用 HTTP 与密码学原语，不引入任何厂商 SDK 或协议分支。
- `http.fetch_retry(..., attempts)` 在连接失败或返回 5xx 时按 100ms × 次数退避重试，`attempts` 取 1..5；最终仍失败时返回最后一次响应（5xx）或 `err(...)`。适合调用跨机服务、对象存储等瞬时故障场景；业务错误（4xx）不重试。

**服务端运行语义（强制）**：

- **并发**：明文 HTTP 由 tokio 管理连接（keep-alive 不占线程，I/O 线程数随 CPU 核数伸缩、限制在 4–16），处理函数在固定数量的解释器线程上执行（缺省为 CPU 核数，`[server] workers` 可覆盖）；请求按「剩余队列容量」分发给最空闲的解释器线程，避免单个慢路由造成队头阻塞；响应头与响应体合并为一次写；TLS 仍为每连接一个工作线程。每个线程拥有独立解释器，`static` / 全局变量按线程隔离，**不跨请求共享**；需要共享状态时用 Redis、数据库或文件。
- **连接复用**：支持 HTTP/1.1 keep-alive，空闲超过 `[server] keep_alive_ms` 或达到 `max_requests` 后关闭。
- **超时**：读超时与写超时由 `[server] read_timeout_ms` / `write_timeout_ms` 控制，防止慢连接长期占用线程。
- **限流背压**：连接队列上限为 `max_connections`，超出时直接返回 `503` 并关闭连接；请求体超过 `max_body_bytes` 返回 `413`；请求行、头部超限返回 `413`。支持 `Content-Length` 与 `Transfer-Encoding: chunked`。
- **进程内限流**：`[server] rate_limit` 开启令牌桶（每秒补充），超出返回 `429` + `Retry-After: 1`，不进入处理函数；`rate_limit_per_ip = true` 时按客户端 IP 分桶，`rip_http_rate_limited_total` 记录次数。多实例需要全局配额时改用 `std/redis` 的 `rate_limit`。
- **反向代理**：部署在可信代理之后时设 `[server] trust_proxy = true`，客户端 IP 取 `X-Forwarded-For` 最左侧合法地址（退回 `X-Real-IP`），访问日志、限流、`req.client_ip` 一并生效；直连场景保持 `false`，否则客户端可以伪造 IP。更安全的方式是 `trusted_proxies = "10.0.0.0/8,..."`：只有来自这些网段的请求才采信转发头，并逐跳还原真实客户端。开启 `trust_proxy = true` 但未配置 `trusted_proxies` 时，启动会打印告警，提示任何客户端都可伪造 `X-Forwarded-For`。
- **故障隔离**：单个请求的运行时错误只影响该请求（返回 `500`），工作线程内的 panic 被捕获后继续服务后续连接。
- **优雅停机**：收到 SIGINT / SIGTERM（Windows 为 Ctrl+C）后停止 accept，把已排队连接处理完再退出；停机期间 `/readyz` 返回 `503`。
- **内置端点**：`GET /healthz` 恒返回 `200 ok`（存活探针）；`GET /readyz` 在未停机时返回 `200 ready`、停机中返回 `503`；`[server] metrics_path` 非空时该路径返回 Prometheus 文本格式指标。三个路径都先于用户路由处理，可在 `[server]` 里改路径或置空关闭。
- **请求 ID**：缺少 `X-Request-Id` 时自动生成 `req-<时间>-<序号>`，写入 `http.request.request_id` 并在响应头回传；`std/log` 的 JSON 格式会自动带上该字段。
- **指标**：`http.metrics()` 返回与内置端点相同的 Prometheus 文本，便于自定义暴露路径。

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

### 11.2.1 路由注解（默认注册方式）

带路由注解的函数在构建 `http.router` 时**自动注册**，不需要再在显式路由表里写一遍：

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

规则：

- 带路由注解的函数签名固定为 `func(read req: http.request) -> http.response`：恰好一个 `http.request` 参数，返回 `http.response`。参数可不加 `read`，但不能是 `write`。
- 注解只提供方法与路径；请求解析、响应组装由函数自己完成。
- 处理器需要的数据库连接、配置等由应用自己持有（例如模块级 `static`），框架**不隐式注入**任何额外参数。
- 同一程序中两条注解声明了相同的 `(方法, 路径)` 是编译错误。
- 注册顺序：先注解路由，后 `http.router` 显式传入的 routes。因此注解与显式路由重复时注解优先命中；`http.static` 生成的 `/*` 通配必须放在显式 routes 中，否则会遮蔽注解路由。
- 显式路由表（`http.get(path, handler)` 等）仍然完全可用；没有注解时行为与之前一致。两种方式可以混用。
- 没有显式 routes 时传空表：`let extra: vec<http.route> = []; http.serve(8080, http.router(extra));`。
- 绑定地址缺省为 `127.0.0.1`（开发默认，仅本机可访问）；`rip run --host H` / `rip dev --host H` 可改为 `0.0.0.0`、本机 IP 或本机可解析的域名，启动日志打印实际配置的地址。

### 11.2.2 HTTPS / TLS

`http.serve` 同时支持明文 HTTP 与 HTTPS，TLS 由 CLI 注入，不写进 Rip 源码：

```powershell
rip run main.rp --cert server.pem --key server.key --port 443
```

- `--cert` 为 PEM 证书链（可含多个证书，首个为站点证书），`--key` 为对应的 PEM 私钥（PKCS#8 或 PKCS#1）。
- 两者必须同时给出；只给一个时 `http.serve` 报运行时错误 `HTTPS requires both --cert and --key`。
- 启用后启动日志变为 `https://`，`http.serve` 的端口、路由、静态资源、上传逻辑完全不变。
- 证书与私钥只从 CLI 读取，不参与 `rip build` 打包；部署时把 PEM 放到目标机器的指定路径，用 `--cert` / `--key` 指向即可。
- TLS 版本由运行时协商（TLS 1.2 / 1.3），不校验客户端证书（无 mTLS）。

### 11.3 字符串方法

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
s.graphemes() -> vec<string>                          // 字素簇（用户可见字符）
s.grapheme_len() -> int
s.width() -> int                                      // 终端显示宽度
```

### 11.4 其他库（核心方法）

**`std/llm`**（四个方法，无别名）：

```rp
type llm.message = [role: string, content: string];
type llm.options = [model: string, temperature: float, max_tokens: int, top_p: float, stream: bool];
type llm.response = [content: string, model: string, prompt_tokens: int, completion_tokens: int, finish_reason: string];

llm.new_client(config: llm.config) -> llm.client;
client.chat(msgs: vec<llm.message>, opts: llm.options) -> result<llm.response, string>;
client.chat_stream(msgs, opts, on_chunk: func(string) -> void) -> result<llm.response, string>;
client.chat_json(msgs, opts, target: type) -> result<any, string>;
```

**`std/cli`**：`args` / `command` / `flag` / `option` / `positionals`，见 11.12。

**`std/dev`**：`log` / `log_json` / `breakpoint` / `assert` / `time_start` / `time_end` / `trace`，`rip build --release` 时编译期剔除。

**`std/log`**：`debug` / `info` / `warn` / `error` / `set_level` / `set_format` / `set_file`。

- `set_level("debug" | "info" | "warn" | "error")` 之后的低级别日志不再输出（缺省 `info`）。
- `set_format("text" | "json")`：`text` 为 `时间 [级别] 消息`，`json` 每行一个对象 `{"time","level","message"[,"request_id"]}`，便于日志采集。
- `set_file(path) -> result<bool, string>`：同时追加写入文件（目录必须已存在），返回 `ok(true)` 或 `err(原因)`；控制台输出保留。

### 11.5 定时任务与并发（`std/task` + `std/sync`）

Rip 的并发模型是**线程级隔离**：每个任务在独立线程中运行，拥有独立的解释器状态，线程之间不共享 Rip 变量，也不共享 `vec` / `map` / `ary`。

**定时任务**用注解声明，程序启动时自动调度：

```rp
#[interval("1m")]
func session_cleanup() -> void {
    // 每分钟执行一次
}

#[interval("500ms")]
func poll_queue() -> void {
    // 每 500 毫秒执行一次
}
```

规则：

- `#[interval("...")]` 只能挂在模块级无参 `func` 上；有参数、未知属性名、非法间隔都是编译错误。
- 间隔格式：正整数 + `ms`（毫秒）/ `s` / `m` / `h` / `d`，如 `"100ms"`、`"30s"`、`"5m"`、`"1h"`、`"1d"`；必须大于 0。
- 启动顺序：`main` 开始执行前启动全部定时任务；`main` 返回后进程结束，任务随之终止。常驻服务由 `http.serve` 阻塞 `main`，因此任务持续运行。
- 任务首次执行发生在一个间隔之后，不会在启动瞬间立即执行。
- 任务内的错误不会终止进程，只打印 `[task] 函数名:行号: error: ...`。

**一次性后台任务**用 `std/task`：

```rp
use std::task;

task.spawn("send_report") -> bool        // 异步执行无参函数，立即返回是否成功启动
task.sleep(毫秒: int) -> void            // 让当前线程休眠
```

**异步函数（`async func` / `await`）**：

```rp
async func load(id: int) -> user {
    // 在独立运行时中执行
    return fetch_user(id);
}

func handle(id: int) -> void {
    // 先启动多个任务，再按需等待：任务之间真正并行。
    let a: task<user> = load(1);
    let b: task<user> = load(2);
    let first: user = await a;
    let second: user = await b;
    print(first.name + second.name);
}
```

规则：

- `async` 只能修饰模块级自由函数；方法、`main`、带属性（路由 / 定时 / 测试）的函数都不能是 async。
- 调用 async 函数**立即**把任务交给调度器执行，返回 `task<T>`；`T` 是该函数的返回类型。任务在调用点就开始运行，不会拖到 `await` 才启动。
- `await` 只接受 `task<T>`，返回 `T`；对非任务值使用是编译错误。同一个任务只能 `await` 一次（所有权检查会拦截第二次）。
- 不 `await` 的任务仍在后台运行，结果被丢弃；需要结果时务必等待。
- 任务函数在**独立的运行时**中执行：看不到调用方的局部变量、模块级 `static` 与闭包，只能通过参数、返回值以及数据库 / 文件 / `std/sync` 等外部资源交互。
- 参数与返回值会跨线程复制；`func` / 闭包与 `task` 不能跨任务边界（编译期或运行期报错）。
- `await` 失败的任务会把原错误（含函数栈）重新抛到调用方，行为与同步调用一致。
- 调度器是进程级有界队列 + 固定工作线程（4× CPU 核数，8..=64）；队列满时在调用线程直接执行，形成反压。`await` 先等完成通知（约 200µs），让任务在调度线程上并行跑完；超时仍未完成才顺手执行队列里的任务，因此嵌套 `await` 不会因工作线程耗尽而死锁。
- 任务线程复用已初始化的运行时：每次任务前清空 globals、借出的数据库 / Redis 连接、网络句柄、defer 栈与请求上下文，再重新初始化模块级变量，隔离语义与新建运行时一致，但省掉了运行时构建开销。
- 调试器只跟踪创建任务的那个线程；任务线程独立执行，不命中调用方设置的断点。

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
    // 并行执行：总耗时约 400ms，而不是串行的 800ms。
    print(total);
    print(time.timestamp_ms() - start);
}
```

**跨线程共享（`std/sync`）**：

Rip 的值（`vec` / `map` / `ary` / 闭包）是单线程引用计数，不能跨线程。跨线程共享状态用进程级命名原语：

```rp
use std::sync;

sync.mutex(name: string) -> sync.mutex
sync.atomic(name: string, initial: int) -> sync.atomic
sync.channel(name: string, capacity: int) -> sync.channel
sync.exists(name: string) -> bool
sync.remove(name: string) -> bool
```

规则：

- **按名字共享**：同一个进程里，同一名字拿到的是同一个对象；HTTP 工作线程、`parallel` 分支、`task.spawn`、`#[interval]` 都能看到。
- 名字必须是固定常量，不要用请求参数拼接（注册表按名字常驻，动态名字会无限增长）。
- **同名不能换类型**：先创建为 mutex 后再当 channel 用，会报运行时错误。
- 句柄是普通值，可以放进 `vec` / `map` / `ary`；调用方法不需要 `let mut`（内部是进程级共享对象）。
- `sync.remove` 只在确认没有其它线程使用该名字时调用（例如测试收尾）；移除后同名会创建新对象，旧句柄仍指向旧对象。

**互斥锁**：

```text
mutex.lock() -> void
mutex.try_lock() -> bool
mutex.lock_timeout(ms: int) -> bool
mutex.unlock() -> void
mutex.is_locked() -> bool
mutex.name() -> string
```

- `lock` 阻塞直到拿到；`try_lock` 立即返回；`lock_timeout` 最多等 `ms`。
- 不记录持有者，任何线程都能 `unlock`。临界区里推荐用 `defer` 释放：

```rp
let mutex: sync.mutex = sync.mutex("report_lock");

func write_report(text: string) -> result<void, string> {
    mutex.lock();
    defer mutex.unlock();     // 提前 return / 出错也会释放
    fs.write_file("report.txt", text)?;
    return ok;
}
```

**原子整数**：

```text
atomic.load() -> int
atomic.store(v: int) -> void
atomic.add(v: int) -> int      // 返回加之前的旧值
atomic.sub(v: int) -> int      // 返回减之前的旧值
atomic.swap(v: int) -> int     // 返回旧值
atomic.compare_swap(old: int, new: int) -> bool
atomic.name() -> string
```

- 全部是 `SeqCst` 原子操作，适合计数、ID 分配、任务完成标志。
- `initial` 只在第一次创建时生效；后续用同名再调用不会重置。

**通道**：

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

- 有界队列：容量 `1..1000000`；满时 `send` 阻塞、`try_send` 返回 `false`、`send_timeout` 最多等 `ms`。
- `recv` 阻塞到有值或通道关闭；`try_recv` 立即返回；`recv_timeout` 最多等 `ms`。
- 关闭后：排队中的值仍可读完，之后 `recv` 立即返回 `none`；`send` 报错，`try_send` / `send_timeout` 返回 `false`。
- 通道传输的是值的**深拷贝**（保留整数宽度、`option` / `result`、`ary` 类名与 `map` 键类型），不是共享引用；函数值不能进通道。
- `option<any>` 用 `.as_int()` / `.as_string()` 等收窄，与 `json.decode` 的约定一致。

```rp
// 生产者：后台任务
func worker() -> void {
    let jobs: sync.channel = sync.channel("jobs", 64);
    let done: sync.atomic = sync.atomic("jobs_done", 0);
    jobs.send(42);
    done.add(1);
}

// 消费者：主线程用超时轮询，不会被永久阻塞
task.spawn("worker");
let jobs: sync.channel = sync.channel("jobs", 64);
match jobs.recv_timeout(1000) {
    some(value) => { dev.log(value.as_int().unwrap_or(0).to_string()); }
    none => { dev.log("1 秒内没有消息"); }
}
```

**超时与取消**：

- 所有阻塞操作（`lock` / `lock_timeout` / `send` / `send_timeout` / `recv` / `recv_timeout`）每 20ms 检查一次 HTTP 处理函数的截止时间；请求超时会立刻中断并返回 504，不会因为等锁或等通道而挂住工作线程。
- 后台任务没有截止时间；用 `*_timeout` 变体或 `try_*` 自行控制，避免永久阻塞。
- 线程之间仍然不共享 Rip 变量；需要共享的只有 `std/sync` 的原语和进程级外部资源（缓存、队列、Redis、数据库）。

**与 `parallel` 配合**：`parallel` 的每条语句是一个独立分支，看不到外层局部变量；分支内用同名原语拿句柄即可汇总结果。

```rp
parallel {
    sync.atomic("jobs_done", 0).add(1);
    sync.atomic("jobs_done", 0).add(1);
}
let done: sync.atomic = sync.atomic("jobs_done", 0);
// done.load() == 2
```

**多实例部署**：定时任务在每台机器上都会启动，默认会重复执行。配置 `[cluster] redis` + `task_lock = true` 后，每次触发前用 Redis 租约选主，只有抢到 `rip:task:<函数名>` 的实例执行；详见 11.11。

### 11.6 加密、编码与密码哈希（`std/crypto`）

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

规则：

- 摘要类函数返回**小写十六进制**字符串；`base64_*` 使用标准 Base64（带 `=` 填充）。
- `base64_decode` / `hex_decode` 输入非法时返回 `err("invalid ... input")`，不终止进程。
- `random_hex(n)` 返回 `n` 字节密码学随机数的十六进制串（长度 `2n`），`n` 必须在 `1..=1024`，越界返回 `err(...)`。
- `password_hash` 返回 `ript$迭代次数$盐$摘要`：PBKDF2-HMAC-SHA256（210000 次迭代）、16 字节随机盐，同一密码每次结果不同；迭代次数写进哈希串，日后调高不影响老数据校验。
- `password_verify(password, hashed)` 从字符串中读取迭代次数与盐重新计算，定长比较；只接受 `ript$` 格式，旧格式或非法串、迭代次数异常、密码不符都返回 `false`，不抛错。
- 密码字段只存 `password_hash` 的结果，校验一律用 `password_verify`，不要自己拼 `sha256` 比较。生产环境请用 release 版 `rip`（`cargo build --release`）：debug 版运行时未优化，每次哈希约数秒；release 版约 100–150ms。
- `aes_encrypt` / `aes_decrypt` 使用 AES-256-GCM。当前格式（V2）：密钥经 PBKDF2-HMAC-SHA256（210000 次迭代、每次加密随机生成 16 字节盐）派生为 32 字节，密文为 `RIPV2:base64(盐 || 12 字节随机 nonce || 密文 || 16 字节认证标签)`；`aes_decrypt` 同时兼容旧版无前缀格式（SHA-256 直接派生），存量数据无需迁移，但新数据一律是 V2。密钥错误或密文被篡改时解密返回 `err(...)`。
- `rsa_*`：`rsa_sign` / `rsa_verify` 使用 RSA PKCS#1 v1.5 签名（内部固定 SHA-256 摘要，结果为 Base64）；`rsa_encrypt` / `rsa_decrypt` 使用 **RSA-OAEP（SHA-256）**，不采用易受 Bleichenbacher 攻击的 PKCS#1 v1.5 加密（旧版 v1.5 密文无法用当前运行时解密）。`rsa_generate(bits)` 生成 `bits` 位密钥对（1024..=8192 且为 8 的倍数），返回 `private_key_pem`（PKCS#8）与 `public_key_pem`（SPKI）。
- `aes_encrypt` 的 V2 格式内置 PBKDF2 派生，弱口令也能安全使用（代价是每次加密多约百毫秒）；旧版无前缀密文仍按 SHA-256 直接派生解密，该路径**只适合高熵密钥**（如 `random_hex(32)` 或配置注入的密钥）。
- 私钥 / 公钥 PEM 同时接受 PKCS#8 / PKCS#1（公钥为 SPKI / PKCS#1），便于与 OpenSSL 等工具互通。
- 本包只处理数据加密与签名；HTTPS 证书加载与 TLS 握手由运行时通过 `--cert` / `--key` 提供（见 11.2.2），Rip 源码不接触私钥以外的 TLS 细节。

### 11.7 XML（`std/xml`）

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

规则：

- `xml.node` 是运行时类型，通过上面的访问函数读取字段，不直接 `.name` / `.attrs`。
- 解析支持 XML 声明、注释、DOCTYPE、CDATA、自闭合标签与命名实体（`&amp;` `&lt;` `&gt;` `&quot;` `&apos;`）以及 `&#NN;` / `&#xNN;`。
- 标签不匹配、属性值未加引号、文档为空、根节点后还有内容时返回 `err(...)`。
- `xml.text` 返回元素直接文本（去首尾空白），不拼接子节点文本；`find` / `find_all` 按标签名深度优先搜索。
- `xml.stringify` 输出紧凑单行，`xml.pretty` 输出两空格缩进的多行。

### 11.8 编码（`std/encode`）

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

规则：

- `url_encode` 使用 RFC 3986 百分号编码，只保留 `A-Z a-z 0-9 - _ . ~`；`url_decode` 认 `%XX` 与 `+`（空格），非法转义返回 `err(...)`。
- HTML 转义与 `xml.escape` 共用同一套规则（`& < > " '`）。
- CSV 默认 `\r\n` 换行，含逗号、引号、换行的字段自动加引号并把 `"` 写成 `""`；`csv_parse` 支持引号字段、转义引号与 CRLF/LF。
- `query_parse` 把 `a=1&b=2` 解析为 `map<string, string>`，同名键后者覆盖；`query_stringify` 按键排序输出并做 URL 编码。

### 11.9 TCP / UDP（`std/net`）

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

规则：

- `timeout_ms` 为 `0` 表示一直等待；`tcp_accept` / `udp_recv` 超时返回 `err("timeout")`，对端正常关闭返回 `err("connection closed")`。
- `net.conn`、`net.listener`、`net.socket` 是运行时不透明句柄，只能传给本包函数，关闭后再次使用返回 `err(...)`。
- `tcp_recv_line` 读到 `\n` 为止（去掉行尾 `\r`），超过 `max_bytes` 时截断返回；`tcp_recv` 一次最多读 `max_bytes`。
- `net.packet` 字段：`data`、`host`、`port`。
- 本包为同步阻塞 API；服务端可用 `task.spawn` 把监听循环放到独立线程。

### 11.10 WebSocket（`std/ws`）

```rp
use std::ws;

ws.serve(port: int, handler: func(ws.conn) -> void) -> void
ws.connect(url: string, timeout_ms: int) -> result<ws.conn, string>
ws.send(conn: ws.conn, text: string) -> result<int, string>
ws.recv(conn: ws.conn, timeout_ms: int) -> result<string, string>
ws.ping(conn: ws.conn) -> result<bool, string>
ws.close(conn: ws.conn) -> result<bool, string>
```

规则：

- `ws.serve` 在 `127.0.0.1`（或 `--host`）上监听并完成握手，每个连接调用一次 `handler`，连接在 handler 返回后关闭；handler 内用 `ws.recv` / `ws.send` 做双向通信。
- **握手安全**：握手阶段读写超时 10 秒，慢连接不会占用服务；请求带 `Origin`（浏览器发起）且 `Origin` 与 `Host` 不同源时返回 `403` 拒绝，防止跨站 WebSocket 劫持；非浏览器客户端（不带 `Origin`）不受影响。
- 客户端用 `ws.connect(url, timeout_ms)`，支持 `ws://` 与 `wss://`：`wss://` 用 rustls + 系统根证书库完成 TLS 握手（校验证书链与主机名），默认端口 443（`ws://` 默认 80）；服务端 `ws.serve` 仍只监听明文，HTTPS 站点由 `[server]` 的 TLS 配置终结。
- **掩码校验**：按 RFC 6455 对收到的帧做角色校验——服务端拒绝未掩码帧、客户端拒绝已掩码帧，违例即回 `close 1002`（协议错误）并断开；符合规范的浏览器与标准库对端不受影响。
- 只处理文本帧（UTF-8），二进制帧按文本解码；`ping` 自动回 `pong`，收到 `close` 后 `recv` 返回 `err("connection closed")`。
- 单帧上限 16 MiB；`ws.conn` 是不透明句柄，关闭后继续操作返回 `err(...)`。

### 11.11 Redis 与集群协作（`std/redis`）

```rp
use std::redis;

redis.connect(url: string) -> result<redis.conn, string>
redis.lock(conn: redis.conn, key: string, ttl_ms: int) -> result<option<string>, string>
redis.unlock(conn: redis.conn, key: string, token: string) -> result<bool, string>
redis.rate_limit(conn: redis.conn, key: string, limit: int, window_ms: int)
    -> result<bool, string>

// redis.conn 方法
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

规则：

- URL 只接受 `redis://` 与 `rediss://`，格式 `redis://[user:password@]host[:port][/db]`；`rediss://` 用系统根证书做 TLS 校验。
- 连接属于当前解释器（线程），不跨线程共享；连接超时 5 秒，读写超时 10 秒，超时返回 `err(...)` 而不是阻塞请求线程。
- `redis.connect` 按 URL 透明复用进程级空闲连接池：再次连接同一 URL 时优先借出空闲连接（借出前 `PING` 验活，过期或断开的连接丢弃后继续找）；`conn.close()` 或任务结束时干净的连接归还池中，读写失败或回复未读完的连接直接丢弃。每个 URL 最多缓存 8 条空闲连接，空闲超过 300 秒在下次借出时淘汰；借出/归还不阻塞、不限制借出总数。
- `redis.lock` 用 `SET key token NX PX ttl` 抢租约：抢到返回 `some(token)`，被别人持有返回 `none`；`redis.unlock` 用 Lua 比对 token 后删除，避免误删他人租约。租约到期自动释放，持有者崩溃不会永久死锁。
- `redis.rate_limit` 是固定窗口计数：窗口内前 `limit` 次返回 `true`，超出返回 `false`。
- 集群定时任务：`rip.toml` 的 `[cluster] redis` 给地址、`task_lock = true` 开启选主后，每个 `#[interval]` 任务在每次触发前抢 `rip:task:<函数名>` 租约，只有抢到的实例执行。租约时长为间隔的两倍（最少 10 秒）；Redis 不可用时退化为本机执行，保证任务不中断。
- 会话、缓存、分布式锁、集群限流都以此为底座；运行时不在本地进程内保存任何跨实例状态。

### 11.12 集合工具、cli、进程缓存、持久化队列与数据库关系

**`std/str` / `std/vec` / `std/map`**：无副作用的函数式接口，需要原地修改时仍用内建方法（`.push` / `.reverse` / `.set`）。

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
         sum(items)                -> int（饱和运算）
         range(n) / range_between(start, end) -> vec<int>

std/map: keys / values / len / has / get
         merge(a, b) / without(m, key)
```

- `split` 的分隔符为空串时按字符拆分；`slice` 按 Unicode 字符计数，越界自动收敛。
- `repeat` 的结果超过 32 MiB 返回错误，避免一次调用打满内存。
- `merge` 返回新 map，`b` 覆盖 `a`；所有集合函数都不是原地操作。

**`std/cli`**：

```rp
cli.args() -> vec<string>                     // 原始参数（不含程序名）
cli.command() -> string                       // 第一个位置参数
cli.positionals() -> vec<string>              // 去掉所有选项后的位置参数
cli.flag(name) -> bool                        // --name / --name=... / -n
cli.option(name, default) -> string           // 缺省值兜底
```

**`std/cache`**：进程级、跨工作线程共享的 TTL + LRU 缓存，只存 `string`。

```rp
cache.set(key, value, ttl_ms) -> void         // ttl_ms = 0 表示永不过期
cache.get(key) -> option<string>
cache.delete(key) -> bool
cache.clear() -> void
cache.len() -> int
cache.configure(max_entries) -> int           // 16..=1_000_000，返回生效值
cache.stats() -> map<string, int>             // hits / misses / evictions / entries
```

- LRU 用「访问序号 + 有序索引」实现，命中/写入/淘汰都是 O(log n)，不做全表扫描。
- 结构化数据先 `json.encode` 再存，读出后 `json.decode`。
- 缓存不跨进程：多实例部署要共享状态时用 `std/redis`。

**`std/queue`**：基于 SQLite 的持久化任务队列（WAL + `busy_timeout`，表 `rip_jobs` 自动创建）。

```rp
type queue.job = [id: int, queue: string, payload: string, attempts: int];

queue.enqueue(dsn, queue, payload, delay_ms) -> result<int, string>
queue.reserve(dsn, queue, lease_ms) -> result<option<queue.job>, string>
queue.ack(dsn, id) -> result<bool, string>
queue.fail(dsn, id, message) -> result<string, string>   // 返回新状态：pending / failed
queue.release_expired(dsn) -> result<int, string>
queue.stats(dsn, queue) -> result<map<string, int>, string>   // pending / running / failed
```

- DSN 写法与 `std/db` 一致（`sqlite:storage/q.db`）。
- 至少一次投递：`reserve` 在事务内选出可运行任务并写入租约，租约过期后由 `release_expired` 放回队列，进程崩溃不会丢任务；处理函数必须幂等。
- `fail` 按 1s / 2s / 4s / 8s 指数退避重排，5 次后转为 `failed` 并保留 `last_error`。

**`std/db` 关系查询与指标**：

```rp
db.has_many(conn, 子表, 外键列, 父 id) -> result<vec<map<string, any>>, string>
db.belongs_to(conn, 父表, id) -> result<option<map<string, any>>, string>
db.belongs_to_by(conn, 父表, 唯一列, 值) -> result<option<map<string, any>>, string>
```

**`std/db` 连接方法**：

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

- `execute_many` 用一条语句批量绑定多组参数（每组一行）：不在显式事务里时自动包一层事务，任一行失败整批回滚并返回错误，全部成功返回受影响行数之和。导入类场景用它代替「循环里逐条 `execute`」。
- `row_json` 是 JSON API 的快速路径：取查询结果的第一行直接序列化为 JSON 对象字符串，跳过 `map` 值树的构造；无结果行返回 `Err("row not found")`。键序与值和 `json.encode(query_one(...))` 逐字节一致（键按字典序），响应体可直接写入 `http.response` 的 `body`。
- 已在 `.begin()` 开启的事务里调用 `execute_many` 时不再嵌套事务，边界仍由调用方的 `commit` / `rollback` 决定。
- 表名、列名经过标识符校验并按方言引用，无法注入 SQL；条件值仍走 `?` 绑定。
- 每次 `execute` / `query` 都累加 `rip_db_queries_total`、`rip_db_errors_total`、`rip_db_duration_ms_total`，缓存命中累加 `rip_cache_hits_total` / `rip_cache_misses_total` / `rip_cache_evictions_total`，都在 `[server] metrics_path` 端点可见。
- Postgres DSN 支持 `sslmode`（URL 查询参数 `?sslmode=require` 或关键字写法 `sslmode=require`）：`disable`（缺省，明文）、`require`（仅加密，不校验服务端证书）、`verify-ca` / `verify-full`（用系统根证书库校验证书链与主机名；rustls 始终校验主机名，因此 `verify-ca` 比 libpq 更严格）。`allow` / `prefer` 依赖回退明文重连，不支持并返回错误。
- MSSQL URL 形式 DSN 支持 `encrypt` / `trustcert` 参数（`?encrypt=true&trustcert=false` 或 `;encrypt=false`，键名不区分大小写、忽略下划线，`TrustServerCertificate` 亦可）：`encrypt` 缺省 `true`（强制 TLS），置 `false` 关闭加密；`trustcert` 缺省 `true`（直接信任服务端证书，与历史行为一致），置 `false` 改用系统根证书库校验。ADO 串形式（`Server=...;Encrypt=...;TrustServerCertificate=...`）由连接串自身语义决定。

### 11.13 可观测性（追踪、日志、指标）

**追踪**：配置 `[server] otlp_endpoint` 后启用 OTLP/HTTP JSON 导出。

- 每个 HTTP 请求一个服务端 span：名称为 `方法 路由模式`，属性包含 `http.method` / `http.target` / `http.route` / `http.status_code` / `http.client_ip` / `duration_ms`；`5xx` 记为错误状态。
- 数据库 `execute` / `query` 作为子 span 挂在当前请求的 span 下；SSE 的 span 覆盖整个推送过程。
- 入站 `traceparent` 的 trace id 与 span id 会被沿用（分布式链路不中断），无上游时生成新的 trace id；出站 `http.fetch` / `http.download` 自动补新的 `traceparent`。
- 采样按 trace id 确定性判定，同一 trace 的所有 span 结果一致；队列上限 4096，满时丢弃最旧并计入 `rip_trace_spans_dropped_total`。
- 导出失败会把整批放回队列重试；Collector 长时间不可用时仍受队列上限约束，不会撑爆内存。

**日志**：`std/log` 的 `json` 格式在请求上下文中自动带上 `request_id` 与 `trace_id`，可直接与追踪对齐。

- `log.set_file(path)` 之后调用 `log.set_rotate(max_bytes, keep)` 开启轮转：单文件超过 `max_bytes` 时依次改名为 `.1`、`.2` …，保留 `keep` 份，原文件重新打开继续追加。
- 轮转状态属于当前解释器；HTTP 工作线程各自持有一份，写入同一路径时各线程独立轮转（需要单文件集中收集时用 Collector / 日志代理）。
- 访问日志（`[server] access_log`，默认开启）由请求线程只追加内存，后台线程每 100ms 整批写 stdout：stdout 变慢时不会拖住请求；缓冲上限 16MB，超出后新日志丢弃并计入 `rip_access_log_dropped_total`。

**指标**：`[server] metrics_path` 暴露 HTTP、限流、数据库、缓存与追踪计数，命名见 11.12 与 10.13。

**工程化工具**：

- `rip test --coverage [--coverage-min N]`：语句行覆盖率；解释器与字节码两条执行路径都插桩，未开启时字节码不增加任何指令。
- `rip bench`：运行 `#[bench]` 函数，输出 `min` / `mean` / `p95` / `max` 与 ops/s。
- `rip fmt [file|dir] [--check] [--stdout]`：统一缩进与空行；`--check` 适合 CI 门禁。
- `rip new <name> [--template http|cli|empty]`：生成 `rip.toml`、入口文件、`.gitignore`，以及 CI 模板 `.github/workflows/ci.yml` 与 `.gitlab-ci.yml`；项目名同时作为包名。CI 依次执行 `rip check` / `rip test` / `rip build --no-archive`，带依赖时取消注释 `rip lock --check` 与 `rip audit --deny high`。
- `rip doc [file] [-o <dir>] [--json]`：从 AST 与源文件 `///` 注释生成单文件 HTML API 文档（类型、枚举、接口、全局函数、方法归属与签名）；`--json` 输出同一模型，便于接入其它文档站。
- `rip search <关键词> [--registry URL] [--json]`：搜索包源（包名、版本、描述）；本地目录与 HTTP 包源都支持。
- `rip info <包名> [--registry URL] [--json]`：列出包的全部版本、依赖与校验和。
- `rip audit [--deny LEVEL] [--json] [--licenses]`：依赖锁定、安全公告与许可证审计，见 10.10 第 5 节。
- `rip profile [file] [--top N] [--json]`：运行程序并输出函数级调用次数、总耗时、自身耗时与占比；覆盖闭包与内建模块调用，多线程调用会归并。
- `rip dap [--port N]`：启动 DAP 调试适配器；支持断点、单步（next / stepIn / stepOut）、暂停、调用栈、Locals / Globals、变量名求值与断开。程序输出通过 DAP `output` 事件回传，不污染协议；`--port` 用于会大量输出的服务。
- 调试会话会停用字节码快速路径，逐语句检查断点；断点行号按入口源文件计。
- 编辑器集成在 `editors/vscode/`：`.rp` 语法高亮（TextMate）、LSP（`rip lsp`）与 DAP（`rip dap`）调试配置；`rip.executable` 设置项指定工具链路径，调试缺省 `program = "${file}"`。

### 11.14 时区、Unicode、ZIP 与邮件

**时区（`std/time`）**：内置 IANA 时区数据库（含夏令时规则），不依赖系统 tzdata。

```rp
time.zone_names() -> vec<string>
time.local_zone() -> string
time.offset(timestamp_ms: int, zone: string) -> result<int, string>
time.format_zone(timestamp_ms: int, pattern: string, zone: string) -> result<string, string>
time.parse_zone(text: string, pattern: string, zone: string) -> result<int, string>
```

- `zone` 是 IANA 名称（`Asia/Shanghai`、`America/New_York`），大小写不敏感；未知时区返回 `err`。
- `offset` 返回该时刻相对 UTC 的偏移秒数，夏令时已计入（纽约 7 月为 `-14400`）。
- `format_zone` / `parse_zone` 的格式与 `time.format` 一致（strftime 风格），`%:z` 输出 `+08:00`。
- 夏令时回拨产生的重复本地时刻取**较早**的一次；春季跳过的时刻返回 `err`。
- `time.format` / `time.parse` 仍是“进程本地时区”的快捷方式；需要确定性时一律用 `*_zone`。

**Unicode（`std/str`）**：

```rp
str.normalize(text: string, form: string) -> result<string, string>  // nfc / nfd / nfkc / nfkd
str.graphemes(text: string) -> vec<string>
str.grapheme_len(text: string) -> int
str.width(text: string) -> int
```

- `len` / `slice` / `at` 按**码点**计数；`grapheme_len` / `graphemes` 按**字素簇**计数（emoji、组合音标不会被拆开）。
- `width` 是终端显示宽度：东亚全角字符记 2，组合字符记 0，用于对齐表格与日志。
- `normalize` 的 `nfkc` 常用于搜索 / 去重前把全角折成半角；未知形式返回 `err`。
- 同名方法：`s.normalize(form)` / `s.graphemes()` / `s.grapheme_len()` / `s.width()`。

**ZIP（`std/zip`）**：

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

- 压缩算法固定 deflate；条目名统一用 `/` 分隔、按 UTF-8 写入。
- 条目名必须是相对路径：绝对路径、盘符与 `..` 一律拒绝（zip-slip 防护）。
- 解压与读取的上限：单条目 256 MiB、单次总计 2 GiB，超出返回 `err`（压缩炸弹防护）。
- `read_text` 要求内容为合法 UTF-8；二进制用 `read_base64` 取回 base64 字符串。
- `add` 在已有归档后追加；文件不存在时等价于 `create`。同名条目以最后写入的为准。

**邮件（`std/mail`）**：同步 SMTP 客户端，TLS 复用运行时自带的 rustls。

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
    cfg.subject = "日报";
    cfg.text = "纯文本正文";
    cfg.html = "<p>HTML 正文</p>";
    return mail.send(cfg);
}
```

- 配置用 `mail.config()` 取默认值后逐字段覆盖，字段类型由编译器校验。
- `tls`：`"none"`（明文）/ `"starttls"`（587，默认端口按配置）/ `"ssl"`（465，隐式 TLS）。
- `username` 为空则不认证；`password` 只在有 `username` 时使用。
- `text` 与 `html` 同时存在时发 `multipart/alternative`；有附件时外层为 `multipart/mixed`。
- 附件内容：`content_base64` 非空时优先，否则读 `path`；`filename` 必填。
- 头部按 RFC 2047 编码，正文 UTF-8；`timeout_ms` 限制单次 socket 操作，`0` 表示不设超时。
- `mail.send` 是阻塞调用：HTTP 处理函数里请放到 `task.spawn` 的后台函数中执行。

### 11.15 反射与动态派发（`std/reflect`）

静态类型是首选。反射只用于日志、序列化、插件桥接等确实需要在运行期读取结构体字段或按名字调用方法的场景。

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

- `type_name` 返回具体类型名：结构体与枚举是声明名（如 `user`、`status`），其余是内建名（`int`、`string`、`vec`、`map`、`option`、`result`、`func`、`none`、`null`）。
- `kind` 返回归类名：结构体与枚举统一为 `"struct"`，其余与 `type_name` 相同。
- `fields` 是可见字段名：结构体按声明顺序（不含内部 `#variant`），`map` 取字符串键，`vec` 取 `"0"`、`"1"` …。
- `get` 支持结构体字段、`map` 字符串键与 `vec` 数字下标；不存在时返回 `none`。`has` 是 `get` 的布尔形式。
- `variant` 只对枚举值返回变体名，其余返回 `none`；无字段变体同样适用。
- `invoke` 按名字调用同名方法（与 `.` 调用是同一套语义），成功返回 `ok(value)`；方法不存在、参数类型不符或运行期错误都返回 `err(消息)`，不会终止程序。
- `is` 判定 `type_name` 或 `kind` 是否等于给定名字，用于插件按类型分派。
- 反射不改变类型检查：参数位置接受任意值（库内签名是 `any`），返回值是 `any`，取值仍需显式转换（见第 12 节）；用户代码里不需要、也不应书写 `any`。

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

## 12. any 类型规则

- `any` **只允许出现在** **`map<string, any>`** **的值类型**和 JSON 解码边界（`json.decode` / `req.json`）。
- 从 `any` 取值必须显式转换：`.as_string()` / `.as_int()` / `.as_float()` / `.as_bool()` / `.as_vec()` / `.as_map()`，返回 `option<T>`。
- `any` 不参与编译期类型检查，丢失静态安全，是显式逃逸口。
- 除 `map<string, any>` 外，**任何位置禁止** **`any`**。

---

# 第二部分 实现规范

## 13. 项目结构

```text
rip/
├── Cargo.toml                    # workspace
├── crates/
│   ├── rp-lexer/                 # 词法分析
│   ├── rp-parser/                # 语法分析 → AST
│   ├── rp-ast/                   # AST 定义
│   ├── rp-types/                 # 类型系统 + 类型检查
│   ├── rp-ownership/             # 所有权 / 借用 / 生命周期检查
│   ├── rp-runtime/               # 运行时（值系统、字节码 VM、树遍历解释器、堆、内建、HTTP/DB/net）
│   ├── rp-core/                  # 纯净宿主（rp-runtime 关闭 interp，AOT 产物链接）
│   ├── rp-rust-interop/          # Rust 包互操作
│   ├── rp-cli/                   # 命令行工具 rip（含 AOT 代码生成）
│   ├── rp-lsp/                   # 语言服务器
│   └── rp-dap/                   # 调试适配器
├── examples/ tests/ demo/ doc/
```

## 14. 编译流水线

```text
源码 .rp
  ↓
[Lexer] → Token 流
  ↓
[Parser] → AST
  ↓
[Type Checker] → 类型化 AST，报类型错误
  ↓
[Ownership Checker] → 借用/生命周期/Send-Sync 检查，插入 drop/move/borrow 点
  ↓
┌─────────────────────┬─────────────────────┐
│ 解释模式             │ AOT 模式             │
│ AST → Bytecode       │ AST → LLVM IR        │
│ VM 执行               │ LLVM → ELF/EXE       │
└─────────────────────┴─────────────────────┘
```

**关键约束**：解释模式也必须**先过完整前端检查**（类型 + 所有权），不允许边跑边查。检查结果作为中间信息传给两个后端。

**当前实现**：解释模式对每个函数尝试编译为栈式字节码（仅标量子集）；无法编译的函数整体回退树遍历解释器，两者共用类型检查结果、运算与调用实现。`RIP_NO_VM=1` 可强制全解释执行，用于行为对比。AOT 提供两条路径：`rip build --native` 把标量 int 子集翻译为 Rust 后由 `rustc -O` 编译（完全独立，无运行时支持库）；`rip build --aot` 把全量语法翻译为 Rust 并静态链接 `rp-runtime`，产物不含源码与语言前端。

## 15. 依赖 crate

| 用途 | crate |
| --- | --- |
| 词法 | logos（或手写） |
| 语法 | 手写递归下降（推荐） |
| LLVM 后端 | inkwell |
| Cranelift 备选 | cranelift-codegen |
| 哈希表 | hashbrown |
| HTTP | hyper / axum（运行时内部） |
| JSON | serde_json（运行时内部） |
| LSP | tower-lsp |
| 日志 | tracing |
| 错误 | thiserror / anyhow（内部） |
| 时区 | chrono-tz（IANA 数据库）+ iana-time-zone |
| Unicode | unicode-normalization / unicode-segmentation / unicode-width |
| ZIP | zip（deflate） |
| 邮件 | lettre（SMTP + rustls） |

## 16. 实现硬约束

1. **唯一写法**：同一语义只允许一种语法，不允许别名、糖、第二写法。
1. **禁止推断**：所有类型显式，唯一例外是 `for` / `match` 解构。
1. **解释模式先检查后执行**：不允许边跑边查。
1. **引用计数 + 环回收**：容器用 `Rc` 即时释放，闭包与容器互指的环由运行时环回收器清理；不提供全量追踪 GC。堆上限由 `[server] max_heap_mb` 控制，超限请求以运行时错误结束。
1. **无** **`unsafe`** **暴露**：用户层无裸指针、无 unsafe。
1. **无 eval**：禁止运行期执行字符串代码。
1. **`ary`** **字段封闭**：运行期不能加字段。
1. **禁止下标**：所有访问走点号。
1. **HTTP 格式由** **`return`** **决定**：`body` / `json` / `sse` 三选一，框架不强制。
1. **Rust 包只按 Rip 语法调用**：不暴露 Rust 特有语法，类型映射固定。
1. **命名规范检查**：违规 warning，`--deny-warnings` 升级 error。
1. **禁止 shadowing**：同作用域变量名唯一。

---

# 第三部分 测试用例

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

预期输出：`Hello, Rip!`

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
        name = "张三",
        age = 25,
        flags = 0b1010,
        perms = 0o755,
        color = 0xFF00FF,
        tags = ["a", "b"],
        scores = ["math" = 90, "english" = 85],
        address = [city = "北京", zip = "100000"]
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
    let mut u: user = [id = 1, name = "张三"];

    let msg: string = greet(u);
    dev.log(msg);

    rename(u, "李四");
    dev.log("renamed: " + u.name);

    let id: int = consume(u);
    dev.log("id = " + id.to_string());

    // dev.log(u.name);   // 编译报错：use after move
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
    dev.log("a still = " + a.to_string());     // Copy，a 仍可用

    let s: string = "hello";
    let len: int = take_string(s);
    dev.log("len = " + len.to_string());
    // dev.log(s);                              // Move，s 已失效
}
```

## 21. `data_structures.rp`

```rp
mod examples;

use std::dev;

func main() -> void {
    // vec：提前定义同构 + 简写
    let mut nums: vec<int> = [1, 4, 3];
    nums.sort(func(a: int, b: int) -> int { return a - b; });
    nums.push(5);
    dev.log("nums.0 = " + nums.0.to_string());
    dev.log("nums.at(1) = " + nums.at(1).unwrap_or(0).to_string());

    // vec：提前定义同构 + 显式
    let nums2: vec<int> = [:int = 1, :int = 2];

    // vec：提前定义异构
    let pair: vec<string, int> = ["a", 1];
    dev.log("pair.0 = " + pair.0);

    // vec：内部定义异构
    let mixed: vec = [:int = 1, :string = "a", :bool = true];
    dev.log("mixed.len = " + mixed.len().to_string());

    // vec：倒序
    let mut xs: vec<int> = [1, 2, 3];
    xs.reverse();
    dev.log("xs.0 = " + xs.0.to_string());     // 3

    // map
    let mut scores: map<string, int> = [];
    scores.set("张三", 90);
    scores.set("李四", 85);
    dev.log("scores.len = " + scores.len().to_string());
    dev.log("张三 = " + scores.get("张三").unwrap_or(0).to_string());

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
        "1" = [id = 1, name = "张三", age = 25,
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

验证：

- `curl localhost:8080/` 返回 JSON，`Content-Type: application/json`
- `curl localhost:8080/text` 返回纯文本，`Content-Type: text/plain`
- `curl localhost:8080/html` 返回 HTML，`Content-Type: text/html`

## 24. `rust_interop.rp`

```rp
mod examples;

use rust::serde_json as json;
use std::dev;

type user = [id: int, name: string];

func main() -> void {
    let u: user = [id = 1, name = "张三"];
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
        [role = "user", content = "用一句话介绍 Rip"]
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

# 第四部分 交付清单

实现完成需满足：

1. `rip run hello.rp` 输出 `Hello, Rip!`
1. `rip run types.rp` 全类型通过（`int` / `i8`–`i64` / `u8`–`u64`、引号区分）
1. `rip run ownership.rp` 通过；取消注释报 use-after-move
1. `rip run copy_vs_move.rp` 通过
1. `rip run data_structures.rp` 通过（`vec` 三种写法、`map` 增删查、`.sort` / `.reverse`）
1. `rip run control.rp` 控制流全通过
1. `rip dev http_server.rp`：`/` 返回 JSON、`/text` 返回纯文本、`/html` 返回 HTML
1. `rip run rust_interop.rp` 通过，`use rust::serde_json;` 调用与普通包无区别
1. `rip build http_server.rp` 产出 `dist/`：单一 ELF/EXE（静态链接 runtime 与 Rust 依赖）+ 生成的 README（应用名、版本号、启动方式）+ `[build] copy` 声明的运行期数据目录，整个目录可在无 `rip` 的机器上运行
1. 所有 `.rp` 遵守：无下标、无推断、无 class、无 shadowing、无 `std/tpl`、无 Rust 语法泄漏；循环只用 `loop` / `while` / `while let` / `for ... in`，`=>` 只出现在 `match` 分支与 `while let`
1. 命名规范检查生效，违规 warning
1. `use std::sys as sys;` 别名生效；`use rust::xxx as xxx;` 别名生效
1. `package foo;` / `import "std/http";` 必须按语法错误处理：两个词都不是关键字，不留兼容路径

---

## 一句话收拢

**Rip（锐，`.rp`，工具** **`rip`，清单** **`rip.toml`）是强类型、零推断、无 class、无 GC、Rust 所有权语义、Go 结构 + PHP 易用的服务端/Web 语言。三种数据结构** **`ary`（固定字段）、`vec`（有序索引序列）、`map`（动态键值字典）；变量** **`let`** **/** **`let mut`** **/** **`const`** **/** **`static`，类型全显式，禁止 shadowing，命名** **`snake_case`（常量** **`SCREAMING_SNAKE_CASE`）；整数** **`int`**（= i64）** **/** **`float`**（= f64）** **/** 固定位宽 **`i8`–`i64`** **/** **`u8`–`u64`；参数修饰符** **`read`** **/** **`write`，Copy 类型复制、Move 类型默认转移；HTTP 返回格式由** **`return`** **决定，`body`** **/** **`json`** **/** **`sse`** **三选一；Rust 包通过** **`use rust::xxx;`** **引入，调用方式完全按 Rip 语法。实现分 Lexer → Parser → 类型检查 → 所有权检查 → 双后端（字节码 VM / LLVM AOT），解释与编译共享同一套前端检查，按第四部分 12 项交付清单逐项通过即为完成。**
