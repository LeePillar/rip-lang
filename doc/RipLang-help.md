# Rip 语言手册

> [中文](RipLang-help.md) | [English](RipLang-help.en.md)

Rip 是强类型、全显式、面向服务端的语言。这份手册只讲**怎么写、怎么用、怎么发布**，按「变量 → 语法 → 组件 → 项目 → 命令」的顺序组织，可以直接当速查表使用。

> 页面右上角搜索框支持搜标题与正文（按 `/` 聚焦）。左侧目录会随滚动高亮。代码块右上角可一键复制。

## 1. 五分钟上手

```bash
rip version                    # 确认工具链可用
rip new shop --template http   # 生成项目骨架
cd shop
rip run main.rp                # 启动服务：http://127.0.0.1:8080/
rip check main.rp              # 只做静态检查
rip build main.rp              # 编译原生二进制到 dist/
```

`rip new` 生成的项目结构：

```text
shop/
├── rip.toml      # 项目清单（名称、版本、入口、服务端参数）
├── main.rp       # 入口，必须含 func main() -> void
└── .gitignore
```

最小的 HTTP 服务：

```rp
mod shop;

use std::http;

#[get("/")]
func index(read req: http.request) -> http.response {
    return http.response[status = 200, json = [message = "Hello, Rip!"]];
}

func main() -> void {
    let routes: vec<http.route> = [];
    http.serve(8080, http.router(routes));
}
```

记住六条规则：

| 规则 | 说明 |
| --- | --- |
| 唯一写法 | 同一语义只有一种语法，没有别名写法 |
| 类型全显式 | 变量、参数、返回值、字段、容器元素都要写类型 |
| 语句以 ; 结尾 | 块和函数声明不加 |
| 循环四种 | loop / while / while let / for ... in，规则同 Rust |
| 没有 class | 用 ary（固定字段结构）+ 自由函数 |
| 没有推断 | 没有 :=，没有省略参数类型 |

## 2. 变量怎么定义

四种绑定，全部写在语句里，类型必须显式：

```rp
let name: string = "rip";        // 不可变
let mut count: int = 0;          // 可变
const MAX: int = 1024;           // 模块级编译期常量
static APP: string = "shop";     // 模块级静态值（启动时初始化）
```

规则：

- `let` 绑定后不能重新赋值；要改变量用 `let mut`。
- `const` / `static` 只能写在模块级（函数外），都不可重新赋值，也没有 `static mut`。
- 同一作用域禁止同名变量（无 shadowing）。
- 修改 `ary` 字段、调用 `push` / `set` 等修改容器的方法，根变量必须是 `let mut`。
- 命名：变量/函数/类型 `snake_case`，常量/静态 `SCREAMING_SNAKE_CASE`。

```rp
let mut total: int = 0;
total = total + 1;      // 合法

let fixed: int = 10;
// fixed = 11;          // 编译错误：不可变
```

### 2.1 类型一览

| 类型 | 写法 | 说明 |
| --- | --- | --- |
| 整数 | int（= i64）/ i8 / i16 / i32 / i64 | 默认 int；位宽类型与 Rust 一致 |
| 无符号整数 | u8 / u16 / u32 / u64 | 与 Rust 同语义 |
| 浮点 | float | 64 位 |
| 字符串 | string | UTF-8 不可变；单引号表示单字符 |
| 布尔 | bool | true / false |
| 结构 | [id: int, name: string] | ary，字段固定 |
| 序列 | vec\<T\> | 有序，下标从 0 开始 |
| 字典 | map\<K, V\> | 动态键值 |
| 可空 | option\<T\> | some(x) / none |
| 结果 | result\<T, E\> | ok(x) / err(e) |
| 任意 | any | 只在 JSON / 动态字典边界使用 |

不同整数类型之间必须显式转换：

```rp
let a: u32 = 0xFF;
let b: int = a.to_int();
let c: u8 = b.to_u8();
```

前缀只表示进制（`0x` / `0o` / `0b`），类型用后缀（`42i32`、`7u8`）；不带后缀的整数是 `int`，小数是 `float`。

### 2.2 三种数据结构

**ary：固定字段结构**

```rp
type address = [city: string, zip: string];
type user = [id: int, name: string, address: address];

let mut u: user = [
    id = 1,
    name = "张三",
    address = [city = "北京", zip = "100000"]
];

u.name;                 // 读
u.address.city;         // 嵌套读
u.name = "李四";         // 写，u 必须是 let mut
```

字段用点号访问，禁止 `u["name"]`、`u[0]`；字段集合编译期固定。

**vec：有序序列**

```rp
let nums: vec<int> = [1, 2, 3];
let empty: vec<int> = [];
let mixed: vec<string, int> = ["a", 1];   // 定长异构，只读

nums.0;        // 编译期常量下标
nums.at(1);    // 运行时下标，返回 result<int, string>
nums.len();
```

禁止 `nums[0]`；要按下标取用 `.0`（常量）或 `.at(i)`（运行时）。

**map：动态字典**

```rp
let mut scores: map<string, int> = [];
scores.set("张三", 90);
scores.get("张三");     // option<int>
scores.has("张三");     // bool
scores.delete("张三");  // option<int>

let config: map<string, string> = ["name" = "shop", "city" = "北京"];
```

禁止 `scores["张三"]`，统一用 `.get` / `.set` / `.has` / `.delete`。

三者的选择：字段固定用 `ary`，有序索引用 `vec`，动态键用 `map`，不做隐式互转。

### 2.3 option 与 result

```rp
let found: option<int> = some(1);
let missing: option<int> = none;

let ok_value: result<int, string> = ok(1);
let bad_value: result<int, string> = err("boom");
```

- `option<T>`：`.unwrap_or(x)`、`.unwrap()`、`.is_some()`、`.is_none()`。
- `result<T, E>`：`.unwrap_or(x)`、`.unwrap()`、`.unwrap_err()`、`.is_ok()`、`.is_err()`。

### 2.4 命名类型与递归类型

```rp
type user = [id: int, name: string, tags: vec<string>];
type node = [value: int, children: vec<node>];   // 递归：经 vec 间接自引用
```

- 类型定义运行期不可修改，同文件类型名唯一。
- 递归类型必须经 `vec` / `option` / `map` 等容器间接引用；纯别名环（`type a = b; type b = a;`）直接报错。

### 2.5 泛型函数

```rp
func identity<T>(x: T) -> T {
    return x;
}

func size<T>(read items: vec<T>) -> int {
    return items.len();
}

let n: int = identity(1);              // 从实参推断
let s: string = identity<string>("a"); // 显式类型实参（推断不出时用）

// 约束：S 必须实现 shape，函数体内可以调用接口方法
func total_area<S: shape>(read items: vec<S>) -> float {
    let mut sum: float = 0.0;
    for item in items {
        sum = sum + item.area();
    }
    return sum;
}
```

- 类型实参可以省略（从实参推断），也可以显式写 `f<T>(...)`；显式写法必须写全。
- `<T: 接口名>` 的约束决定函数体能调用哪些方法；没有约束的 T 只能整体传递。
- 运行时类型擦除，只保留一份实现；`rip run` 与 `rip build` 的 AOT 产物行为一致。

### 2.6 Copy 与 Move

| 类型 | 语义 |
| --- | --- |
| int\* / float / bool | Copy，赋值与传参自动复制 |
| string / ary / vec / map / 闭包 | Move，赋值或按值传参后原变量失效 |

```rp
let a: vec<int> = [1, 2];
let b: vec<int> = a;        // a 被 move，此后不能再用 a
let c: vec<int> = b.clone(); // 需要两份时就 clone
```

函数参数默认是「Copy / Move」；不想交出所有权时写 `read`：

```rp
func total(read items: vec<int>) -> int {   // 只读借用，调用方保留 items
    return vec.sum(items);
}

func add_one(write counter: int) -> void {  // 可写借用，实参必须是 let mut
    counter = counter + 1;
}
```

### 2.7 枚举（ADT）

```rp
type status = enum {
    active,
    pending,
    banned(reason: string),
}

let s: status = status.active;
let b: status = status.banned[reason = "spam"];
```

- 变体用 `,` 分隔；无字段只写名字，有字段写 `名字(字段: 类型, ...)`。
- 构造：`枚举.变体` / `枚举.变体[字段 = 值]`，字段必须写全、类型必须匹配。
- 枚举是 Move 类型；需要两份时用 `.clone()`。
- 枚举可以挂方法：`func status.blocked(read s: status) -> bool`，调用 `s.blocked()`。
- 枚举值不能直接取字段（`s.reason` 报错），必须用 `match` 解构。
- JSON 序列化为 `{"variant": "banned", "reason": "spam"}`。

## 3. 语法与控制流怎么用

### 3.1 运算符

```text
+  -  *  /  %       算术（同类型，结果同类型；零除 panic）
== != > >= < <=     比较（同类型，返回 bool）
&& || !             逻辑（短路）
+                   字符串拼接（两侧都是 string）
```

优先级从低到高：`||` → `&&` → 比较 → `+ -` → `* / %` → 一元 `! -`。

### 3.2 if

```rp
if score >= 90 {
    print("A");
} else if score >= 80 {
    print("B");
} else {
    print("C");
}
```

条件必须是 `bool`（没有 truthy/falsy）；括号可省略，分支必须用花括号。

### 3.3 循环：四种写法（规则同 Rust）

循环只有这四种，按场景挑一种写，不和别的语法叠加。

```rp
// while：条件成立就继续，最常用的计数 / 条件循环
let mut count: int = 0;
while count < 10 {
    print(count.to_string());
    count = count + 1;
}

// loop：一直循环，退出条件写在循环体里
let mut i: int = 0;
loop {
    if i >= 10 { break; }
    print(i.to_string());
    i = i + 1;
}

// for ... in：遍历 vec（得到元素）或 map（得到 key）
let tags: vec<string> = ["a", "b"];
for tag in tags {
    print(tag);
}

let mut scores: map<string, int> = ["math" = 90];
for key in scores {          // key: string
    print(key);
}

// while let：每轮取一个值并匹配模式，取不到就结束
while let some(value) = cache.get("key") => {
    print(value);
    cache.delete("key");       // 删掉后下一轮 get 返回 none，循环结束
}
```

**break / continue**（四种循环通用，只作用于最内层）：

```rp
let mut k: int = 0;
while k < 10 {
    k = k + 1;
    if k == 2 { continue; }    // 跳过本次，回到条件判断
    if k == 5 { break; }       // 直接结束循环
    print(k.to_string());
}
```

| 场景 | 用哪种 | 写法 |
| --- | --- | --- |
| 次数固定 / 计数 | while | `while i < n { ...; i = i + 1; }` |
| 退出条件在中间 | loop | `loop { ... if done { break; } }` |
| 遍历 vec / map | for ... in | `for x in xs { ... }` |
| 反复取可空值到取空 | while let | `while let some(v) = next() => { ... }` |

- 循环条件必须是 `bool`，没有 truthy / falsy。
- 没有 `do-while`，也没有 C 风格三段式 `for`；条件循环直接写 `while`。
- `while let` 里 `=` 绑定模式、`=>` 引出循环体（`=>` 全语言只有 `match` 分支和这里两处）。
- `while let` 常用于 `cache.get`、`map.delete` 这类返回 `option` 的调用。

### 3.4 match

```rp
match opt {
    some(v) => { print(v.to_string()); }
    none => { print("empty"); }
}

match res {
    ok(v) => { print(v.to_string()); }
    err(e) => { print("error: " + e); }
}
```

- 分支用 `=>`（左边匹配 → 右边执行）；`=` 只用于绑定与赋值；分支体必须用花括号。
- `option` 必须同时覆盖 `some` / `none`，`result` 必须覆盖 `ok` / `err`，缺分支是编译错误。
- 解构结构体：

```rp
match user {
    [id: int, name: string, address: _] => {
        print(name);
    }
}
```

枚举必须覆盖所有变体：

```rp
match state {
    status.active => { print("ok"); }
    status.pending => { print("wait"); }
    status.banned[reason: string] => { print(reason); }
}
```

- 枚举模式写成 `枚举名.变体`；带字段的变体用 `[字段: 类型, ...]` 绑定字段（`_` 不绑定）。
- 少写任一变体都是编译错误：`match on enum 'status' must cover variant 'banned'`。

### 3.5 `?`：把错误往上抛

```rp
func read_config(path: string) -> result<string, string> {
    let content: string = fs.read_file(path)?;
    return ok(content);
}
```

`?` 只能用于返回 `result` 的函数；遇到 `err` 立即返回，遇到 `ok` 取出值继续。非 `result` 函数里使用 `?` 是编译错误。

### 3.6 函数定义与调用

```rp
func add(a: int, b: int) -> int {
    return a + b;
}

func log_message(msg: string) -> void {
    print(msg);
}

func main() -> void {
    let sum: int = add(1, 2);
    log_message("sum=" + sum.to_string());
}
```

- 每个参数写 `名字: 类型`，必须写返回类型，没有返回值写 `-> void`。
- 不支持重载、默认参数、可变参数。
- 函数可以定义在 `main` 之后，运行前统一注册。

**可见性**：`self func` 只在同包（同目录）内可见；`pub func` / `pub type` 导出给其他包。不加修饰符的函数保持全局可见。

**闭包**：参数与返回类型必须写，类型必须显式：

```rp
let double: func(int) -> int = func(x: int) -> int {
    return x * 2;
};
```

**三种调用形式**（记住这三条就能调用一切）：

```rp
add(1, 2)                    // 1. 普通函数：直接写名字
http.serve(8080, handler)    // 2. 标准库：use 后用「别名.函数」
items.push(3)                // 3. 值的方法：值.方法()
```

标准库导入可改别名：

```rp
use std::http;
use std::sys as sys;
use rust::serde_json as json;
```

### 3.7 注解（写在函数上方）

| 注解 | 作用 | 示例 |
| --- | --- | --- |
| #[get("/path")] 等 | 注册 HTTP 路由 | #[post("/users")] |
| #[interval("5m")] | 定时任务 | 间隔用 ms / s / m / h / d |
| #[test] | 测试函数，rip test 执行 | 无参数、无入参、返回 void |
| #[bench] | 基准函数，rip bench 执行 | 无参数、无入参、返回 void |

路由处理函数签名固定为 `func(read req: http.request) -> http.response`。

### 3.8 方法与接口

给类型加方法：`func 类型.方法(接收者, ...)`，接收者写 `read`（只读）或 `write`（可修改）。

```rp
type user = [id: int, name: string];

func user.display(read u: user) -> string {
    return u.name;
}

func user.rename(write u: user, name: string) -> void {
    u.name = name;
}

let mut u: user = [id = 1, name = "张三"];
print(u.display());
u.rename("李四");
```

- 调用就是 `u.display()`，不需要额外语法。
- `write` 方法只能对 `let mut` 变量调用。
- 方法名在类型内唯一；完整名字是 `user.display`。

**接口**：声明一组方法，任何类型只要实现这些方法就能当接口用。

```rp
type shape = interface {
    func area() -> float
    write func scale(factor: float) -> void
}

type circle = [radius: float];

func circle.area(read c: circle) -> float {
    return 3.0 * c.radius * c.radius;
}

func circle.scale(write c: circle, factor: float) -> void {
    c.radius = c.radius * factor;
}

let shapes: vec<shape> = [circle[radius = 1.0]];
for s in shapes {
    print(s.area().to_string());
}
```

- 不用写 implements：方法名、参数、返回类型一致就自动实现接口。
- 接口能放进 `vec`、作为参数 / 返回值；调用时自动按真实类型找方法。
- 接口值用命名构造器创建（`circle[radius = 1.0]`），运行时据此记录类型。

### 3.9 defer：函数退出时清理

`defer 调用;` 把一次调用推迟到**当前函数退出时**执行，用来关连接、解锁、删临时文件。

```rp
func save(path: string, text: string) -> result<void, string> {
    let f: file = fs.open(path)?;
    defer f.close();            // 实参在 defer 处就地求值
    f.write(text)?;
    return ok;
}
```

- 只能写调用：`defer f(...)` / `defer x.m(...)` / `defer 模块.f(...)`。
- 参数在 `defer` 那一行就求值，之后再改变量不影响已经捕获的值。
- 多个 `defer` 后进先出：最后登记的最先执行。
- 正常返回、`return`、`?` 提前返回、运行时报错都会执行。
- 每个函数只执行自己的 `defer`，内层函数返回时只清理内层登记的项。

```rp
func demo() -> void {
    defer print("first");        // 最后执行
    defer print("second");       // 先执行
    print("body");
}
// 输出：body / second / first
```

### 3.10 async / await：异步任务

`async func` 声明会在独立运行时执行的任务函数；调用它立即返回 `task<T>`，用 `await` 取回结果。

```rp
async func load(id: int) -> user {
    return fetch_user(id);
}

func main() -> void {
    // 先启动两个任务，再依次等待：两个任务真正并行。
    let a: task<user> = load(1);
    let b: task<user> = load(2);
    let first: user = await a;
    let second: user = await b;
    print(first.name + second.name);
}
```

- `async` 只能修饰模块级自由函数；方法、`main`、带注解的函数都不能是 async。
- 任务在调用点就开始执行；不 `await` 的任务仍在后台运行，结果丢弃。
- `await` 只接受 `task<T>`，返回 `T`；同一任务只能等待一次。
- 任务有独立运行时：看不到调用方变量与闭包，参数与返回值按值复制（闭包不能跨任务）。
- 任务失败时，`await` 会把原错误（含函数栈）抛回调用方。
- 调度器按 4× CPU 核数创建 8..=64 个工作线程；`await` 先等完成通知（约 200µs）让任务在调度线程上跑完，超时未完成才由等待方协助执行，嵌套 `await` 不会死锁。

## 4. 内建方法速查

### 4.1 整数与浮点

| 方法 | 返回 |
| --- | --- |
| .to\_int(base) / .to\_i8(base) / .to\_i16(base) / .to\_i32(base) / .to\_i64(base) / .to\_u8(base) / .to\_u16(base) / .to\_u32(base) / .to\_u64(base) | 对应整数类型；接收者必须是变量；`base` 是进制（2..36，缺省 10）：`let s: string = "ff"; s.to_u8(16)`、`let n: i8 = 55; n.to_i8(16)` |
| .to\_float() / .to\_string() / .to\_string\_base(base) | float / string |
| .abs() / .min(o) / .max(o) / .mod(m) / .pow(e) / .clamp(lo, hi) | 同类型 |

浮点：`.to_int()`、`.to_float()`、`.to_string()`、`.abs()`、`.min`、`.max`、`.pow`、`.clamp`。

### 4.2 字符串

| 方法 | 返回 | 说明 |
| --- | --- | --- |
| .len() | int | Unicode 字符数 |
| .at(i) / .slice(start, end) | result\<string, string\> | 越界返回 err |
| .first() / .last() | string | 空串返回 "" |
| .contains / .starts\_with / .ends\_with | bool |  |
| .index\_of / .last\_index\_of | option\<int\> |  |
| .to\_upper / .to\_lower / .trim / .trim\_start / .trim\_end | string |  |
| .replace(old, new) / .replace\_all(old, new) | string | 替换一处 / 全部 |
| .split(sep) / .split\_lines() / .chars() / .bytes() | vec\<string\> / vec\<u8\> |  |
| .reverse() / .filter(f) / .remove\_whitespace() | string |  |
| .to\_int() / .to\_float() / .to\_bool() | result\<T, string\> |  |
| .pad\_start(n, pad) / .pad\_end(n, pad) / .repeat(n) | string |  |
| .join(items) | string | items: vec\<string\> |

### 4.3 vec

`.len()`、`.is_empty()`、`.at(i)`、`.push(x)`、`.pop()`、`.insert(i, x)`、`.remove(i)`、`.clear()`、`.reverse()`、`.sort(cmp)`、`.map(f)`、`.filter(f)`、`.reduce(f, init)`、`.find(f)`、`.any(f)` / `.all(f)` / `.count(f)`、`.slice(start, end)`、`.concat(other)`、`.unique()`、`.flatten()`、`.join(sep)`、`.chunk(n)`、`.zip(other)`、`.take(n)`、`.skip(n)`、`.clone()`。

### 4.4 map

`.set(k, v)`、`.get(k)`、`.has(k)`、`.delete(k)`、`.len()`、`.keys()`、`.values()`、`.items()`、`.merge(other)`、`.filter(f)`、`.clone()`。

### 4.5 any

`.as_string()`、`.as_int()`、`.as_float()`、`.as_bool()`、`.as_vec()`、`.as_map()`，全部返回 `option<T>`，类型不符返回 `none`。

## 5. 标准库组件

### 5.1 组件总览

| 组件 | 用途 | 常用入口 |
| --- | --- | --- |
| std/http | Web 服务端、路由、中间件、客户端 | http.serve / http.router / http.fetch |
| std/db | SQLite / MySQL / PostgreSQL / SQL Server | db.open、db.table |
| std/json | JSON 编解码 | json.encode / json.decode |
| std/str std/vec std/map | 集合工具 | str.split、vec.sum、map.merge |
| std/time | 时间与格式化 | time.now / time.format |
| std/fs std/path | 文件、目录、路径 | fs.read\_file、path.join |
| std/process std/sys | 子进程、环境变量、堆信息 | process.run、sys.env\_get |
| std/log std/dev | 分级日志、调试输出 | log.info、dev.log |
| std/crypto | 哈希、AES、RSA、密码、随机 | crypto.sha256、crypto.password\_hash |
| std/encode | URL / HTML / CSV / 查询串 | encode.url\_encode |
| std/xml std/regex | XML 解析、正则 | xml.parse、regex.replace |
| std/rand std/uuid std/compress | 随机、UUID、gzip | rand.int、uuid.v4、compress.gzip |
| std/net std/ws std/redis | TCP/UDP、WebSocket、Redis | net.tcp\_connect、ws.serve |
| std/task std/cache std/queue | 定时/后台任务、进程缓存、持久化队列 | task.spawn、cache.set、queue.enqueue |
| std/sync | 跨线程互斥锁、原子计数、通道 | sync.mutex、sync.atomic、sync.channel |
| std/session std/jwt std/validate std/template | Web 通用件 | 会话、JWT、校验、模板 |
| std/cli | 命令行参数 | cli.flag / cli.option |
| std/llm | 大模型调用（本地模拟） | llm.new\_client |
| std/zip std/mail | ZIP 归档、SMTP 邮件 | zip.extract、mail.send |
| std/reflect | 运行时类型信息、按名字调用方法 | reflect.type\_name、reflect.invoke |
| rust/serde\_json | Rust 互操作包 | json.to\_string / json.from\_str |

### 5.2 HTTP 服务端

```rp
use std::http;

http.serve(port: int, handler: func(http.request) -> http.response) -> void
http.router(routes: vec<http.route>) -> func(http.request) -> http.response
http.get / post / put / delete / patch / options / head(path, handler) -> http.route
http.static(prefix: string, dir: string) -> http.route
http.group(prefix, middlewares, routes) -> http.route
http.chain(middlewares, handler) -> handler
http.cors(origin) / http.security_headers() / http.with_headers(map) / http.rate_limit(rate, burst)
http.openapi(handler, title, version) -> string
```

**路由**：注解优先，`http.router` 构建时自动注册；静态资源等显式路由作为参数传入。

```rp
#[get("/users/:id")]
func show(read req: http.request) -> http.response {
    let id: string = req.params.get("id").unwrap_or("");
    return http.response[status = 200, json = [id = id]];
}

func main() -> void {
    let extra: vec<http.route> = [http.static("/assets", "public")];
    http.serve(8080, http.router(extra));
}
```

路径里 `:name` 是参数，`*` 是前缀匹配；通配静态路由要放在显式路由最后。

**`http.request` 字段**

| 字段 | 类型 | 说明 |
| --- | --- | --- |
| method / path | string | 请求方法 / 路径 |
| params | map\<string, string\> | 路径参数 |
| query | map\<string, string\> | 查询参数 |
| headers | map\<string, string\> | 请求头，键小写 |
| body | string | 原始请求体 |
| json | option\<any\> | 请求体为 JSON 时解析 |
| form | map\<string, string\> | 表单字段 |
| cookies | map\<string, string\> | Cookie |
| files | vec\<http.file\> | 上传文件 |
| client\_ip / request\_id | string | 客户端 IP / 请求 ID |
| ctx | map\<string, any\> | 中间件与处理函数共享的数据 |

**`http.response` 字段**

| 字段 | 类型 | 默认 |
| --- | --- | --- |
| status | int | 200 |
| headers | map\<string, string\> | 空 |
| body | string | 空 |
| json | option\<any\> | none |
| sse | option\<func(func(http.sse\_event) -\> void) -\> void\> | none |

`body` / `json` / `sse` 三选一，同时写两个是编译错误。写 `json` 自动序列化并设置 `application/json`。

**中间件**：工厂函数 `func(handler) -> handler`。

```rp
func require_user(next: func(http.request) -> http.response)
    -> func(http.request) -> http.response {
    return func(write req: http.request) -> http.response {
        let token: string = req.headers.get("authorization").unwrap_or("");
        if token == "" {
            return http.response[status = 401, body = "unauthorized"];
        }
        req.ctx.set("user", token);
        return next(req);
    };
}

http.serve(8080, http.router([
    http.group("/api", [http.cors("*"), http.security_headers(), require_user], [
        http.get("/me", me)
    ])
]));
```

**SSE 流式输出**：

```rp
func ticks(send: func(http.sse_event) -> void) -> void {
    send(http.sse_event[event = "tick", data = "1"]);
}

#[get("/events")]
func events(read req: http.request) -> http.response {
    return http.response[sse = some(ticks)];
}
```

**上传与下载**：

```rp
#[post("/upload")]
func upload(read req: http.request) -> http.response {
    let mut paths: vec<string> = [];
    for file in req.files.clone() {
        match http.save_file("uploads", file) {
            ok(path) => { paths.push(path); }
            err(message) => { }
        }
    }
    return http.response[status = 201, json = [files = paths]];
}
```

**调用第三方服务**：

```rp
http.fetch(method, url, headers, body) -> result<http.client_response, string>
http.fetch_file(method, url, headers, path) -> result<http.client_response, string>
http.fetch_retry(method, url, headers, body, attempts) -> result<http.client_response, string>
http.download(url, headers, path) -> result<int, string>
```

`http.client_response`：`status: int`、`headers: map<string, string>`（键小写）、`body: string`。二进制下载用 `download` 或 `fetch_file`。

### 5.3 数据库

```rp
db.open(dsn) -> result<db.conn, string>
```

| 数据库 | DSN 示例 |
| --- | --- |
| SQLite | storage/app.db、sqlite::memory: |
| MySQL | mysql://root:pass@127.0.0.1:3306/app |
| PostgreSQL | postgres://user:pass@127.0.0.1:5432/app |
| SQL Server | mssql://sa:pass@127.0.0.1:1433/app |

SQL 统一写 `?` 占位符，运行时按方言改写。

**连接方法**：`.execute(sql, params)`、`.execute_many(sql, rows)`（批量绑定多组参数，自动包事务）、`.query(sql, params)`、`.query_one(sql, params)`、`.last_insert_id()`、`.begin()` / `.commit()` / `.rollback()`、`.close()`。

```rp
match conn.query("SELECT id, name FROM users WHERE name LIKE ?", ["%张%"]) {
    ok(rows) => {
        for row in rows {
            dev.log(row.get("name").unwrap_or("").as_string().unwrap_or(""));
        }
    }
    err(message) => { log.error(message); }
}
```

**链式 ORM**：

```rp
db.table(conn, "users").insert(values)                    // map / ary
db.table(conn, "users").where("id", "=", 1).update(changes)
db.table(conn, "users").where("id", "=", 1).delete()
db.table(conn, "users").field("id, name").order("id", "desc").limit(10).select()
```

构造器方法：`.field`、`.where`、`.where_in`、`.where_null`、`.join`、`.order`、`.limit`、`.offset`、`.select`、`.find`、`.count`、`.value`、`.column`、`.insert`、`.update`、`.delete`、`.to_sql`、`.clone`。

**关系查询**：`db.has_many(conn, 子表, 外键列, 父 id)`、`db.belongs_to(conn, 父表, id)`、`db.belongs_to_by(conn, 父表, 唯一列, 值)`。

### 5.4 JSON、时间、文件、路径、进程与系统

```rp
json.encode(any) -> string
json.decode(string) -> result<any, string>

time.now() -> time.datetime          // .timestamp_ms() / .format(pattern) / .to_string()
time.timestamp_ms() / timestamp_s() -> int
time.format(timestamp_ms, pattern) -> string
time.format_zone(timestamp_ms, pattern, zone) -> result<string, string>
time.parse_zone(text, pattern, zone) -> result<int, string>
time.offset(timestamp_ms, zone) -> result<int, string>   // 相对 UTC 的秒数（含夏令时）
time.zone_names() -> vec<string> / time.local_zone() -> string
time.sleep(ms) -> void

fs.read_file / write_file / append_file / exists / make_dir / remove_dir
fs.remove_file / copy_file / rename / file_size / list_dir / list_dir_all / metadata
fs.temp_file(prefix) / temp_dir(prefix)

path.join(a, b) / normalize(p) / absolute(p) / file_name(p) / parent(p) / extension(p) / stem(p)

process.run(cmd, args) -> [code: int, stdout: string, stderr: string]
process.run_shell(cmd) -> [code: int, stdout: string, stderr: string]   // 设置环境变量 RIP_DISABLE_SHELL 后调用直接返回 err

sys.env_get(name) -> option<string>
sys.args() -> vec<string>
sys.exit(code) -> void
sys.platform() -> string
sys.heap_usage() / sys.gc() -> map<string, int>
```

### 5.5 日志与调试

```rp
log.debug / info / warn / error(text) -> void
log.set_level("debug" | "info" | "warn" | "error") -> void
log.set_format("text" | "json") -> void
log.set_file(path) -> result<bool, string>
log.set_rotate(max_bytes, keep) -> void       // 超过上限切成 .1 / .2 …

dev.log(value) -> void            // 调试输出，release 构建剔除
dev.log_json(value) / dev.assert(cond) / dev.trace(msg) / dev.time_start(name) / dev.time_end(name)
```

`json` 格式的日志在请求上下文中自动带 `request_id` 与 `trace_id`，能和追踪对上。

### 5.6 加密与编码

```rp
crypto.md5 / sha1 / sha256 / sha512(text) -> string
crypto.hmac_sha256(key, text) -> string
crypto.base64_encode / base64_decode / hex_encode / hex_decode
crypto.random_hex(bytes) -> result<string, string>
crypto.password_hash(password) -> result<string, string>
crypto.password_verify(password, hashed) -> bool
crypto.aes_encrypt(text, key) / aes_decrypt(cipher, key) -> result<string, string>
crypto.rsa_generate(bits) / rsa_sign / rsa_verify / rsa_encrypt / rsa_decrypt

encode.url_encode / url_decode / html_escape / html_unescape
encode.csv_encode(rows) / csv_parse(text)
encode.query_parse(text) / query_stringify(map)

xml.parse(text) -> result<xml.node, string>
xml.stringify / pretty / name / text / attrs / children / find / find_all / escape / unescape

regex.is_match(pat, text) -> result<bool, string>
regex.find / find_all / captures / replace
rand.int(min, max) / rand.float() / rand.bool()
uuid.v4() / uuid.nil()
compress.gzip(text) / gunzip(base64)
```

### 5.7 任务、缓存与队列

```rp
task.spawn("函数名") -> bool         // 独立线程执行一次无参函数
task.sleep(ms) -> void

#[interval("5m")]
func cleanup() -> void { }

cache.set(key, value, ttl_ms) -> void     // ttl_ms = 0 永不过期
cache.get(key) -> option<string>
cache.delete(key) / cache.clear() / cache.len()
cache.configure(max_entries) -> int
cache.stats() -> map<string, int>

queue.enqueue(dsn, queue, payload, delay_ms) -> result<int, string>
queue.reserve(dsn, queue, lease_ms) -> result<option<queue.job>, string>
queue.ack(dsn, id) / queue.fail(dsn, id, message)
queue.release_expired(dsn) / queue.stats(dsn, queue)
```

- 定时任务每个在独立线程、独立解释器里运行，不共享 Rip 变量。
- `cache` 只存 `string`，结构化数据先 `json.encode`。
- `queue` 基于 SQLite，至少一次投递，处理函数必须幂等；`queue.job` 字段：`id` / `queue` / `payload` / `attempts`。

```rp
#[interval("10s")]
func poll() -> void {
    match queue.reserve("storage/jobs.db", "mail", 30000) {
        ok(job) => {
            match job {
                some(item) => {
                    // 处理 item.payload
                    queue.ack("storage/jobs.db", item.id);
                }
                none => { }
            }
        }
        err(message) => { log.error(message); }
    }
}
```

### 5.8 会话、JWT、校验与模板

```rp
session.save(data, secret, ttl_secs) -> string
session.cookie(token, ttl_secs) -> string   // 服务端启用 HTTPS 时自动附加 Secure 属性
session.load(req, secret) -> map<string, any>
session.clear() -> string
session.csrf_token(secret) / session.csrf_check(req, secret)

jwt.sign(payload, secret, ttl_secs) -> string
jwt.verify(token, secret) -> result<map<string, any>, string>

validate.required(value) -> option<string>
validate.int_range(value, min, max) -> result<int, string>
validate.string_len(value, min, max) -> result<string, string>
validate.email(value) -> result<string, string>

template.render(source, data) -> string
template.escape(text) -> string
template.register(name, f) -> void
template.clear_cache() -> void
```

模板语法：`{{ x }}` 自动转义、`{{{ raw }}}` 原样输出、`{{#if x}}…{{else}}…{{/if}}`、`{{#unless x}}`、`{{#each items}}{{ @index }}:{{ . }}{{/each}}`、`{{! 注释 }}`；`register` 注册后可写 `{{ f(name, "x", 1) }}`（实参支持变量路径、字符串、整数/浮点、布尔、`null`；只能注册模块级具名函数）。

```rp
let token: string = jwt.sign(["sub" = "42", "role" = "admin"], secret, 3600);
match jwt.verify(token, secret) {
    ok(claims) => { return http.response[status = 200, json = some(claims)]; }
    err(message) => { return http.response[status = 401, body = message]; }
}
```

### 5.9 网络、WebSocket、Redis、LLM

```rp
net.tcp_connect(host, port, timeout_ms) -> result<net.conn, string>
net.tcp_send(conn, data) / tcp_recv(conn, max_bytes, timeout_ms) / tcp_recv_line(...)
net.tcp_close(conn) -> result<bool, string>
net.tcp_listen(host, port) / tcp_accept(listener, timeout_ms) / tcp_listener_close(listener)
net.udp_bind(host, port) / udp_send(socket, host, port, data) / udp_recv(socket, max_bytes, timeout_ms)

ws.serve(port, func(ws.conn) -> void) -> void   // 握手超时 10s；浏览器 Origin 与 Host 不同源时拒绝（403）
ws.connect(url, timeout_ms) -> result<ws.conn, string>
ws.send(conn, text) / ws.recv(conn, timeout_ms) / ws.ping(conn) / ws.close(conn)

redis.connect(url) -> result<redis.conn, string>
redis.lock(conn, key, ttl_ms) -> result<option<string>, string>
redis.unlock(conn, key, token) -> result<bool, string>
redis.rate_limit(conn, key, limit, window_ms) -> result<bool, string>
conn.get / set / setex / setnx / del / exists / expire / incr / decr
conn.hget / hset / hdel / lpush / rpush / lpop / rpop / brpop / llen / eval / close

llm.new_client(config) -> llm.client
client.chat(msgs, opts) / chat_stream(msgs, opts, on_chunk) / chat_json(msgs, opts, target)
```

### 5.10 集合工具与命令行

```rp
str.split(text, sep) / lines(text) / join(items, sep)
str.trim / trim_start / trim_end / lower / upper
str.contains / starts_with / ends_with
str.replace(text, from, to) / slice(text, start, end)
str.repeat(text, n) / pad_start(text, width, pad) / pad_end(...)
str.index_of(text, sub) / len(text) / is_empty(text) / split_once(text, sep)
str.normalize(text, form) -> result<string, string>   // nfc / nfd / nfkc / nfkd
str.graphemes(text) / grapheme_len(text) / width(text)

vec.len / is_empty / contains / index_of / first / last / sum / range / range_between
map.keys / values / len / has / get / merge / without

cli.args() / command() / positionals()
cli.flag(name) -> bool
cli.option(name, default) -> string
```

### 5.10 并发原语

线程之间不共享 Rip 变量；需要共享状态时用 `std/sync` 的**命名**原语：同一个名字在 HTTP 工作线程、`parallel` 分支、`task.spawn`、定时任务里都是同一个对象。

```rp
use std::sync;

let lock: sync.mutex = sync.mutex("report_lock");
let counter: sync.atomic = sync.atomic("jobs_done", 0);
let jobs: sync.channel = sync.channel("jobs", 64);
```

**互斥锁**：`lock()` / `try_lock()` / `lock_timeout(ms)` / `unlock()` / `is_locked()`。拿到锁后推荐用 `defer` 释放：

```rp
func write_report(text: string) -> result<void, string> {
    lock.lock();
    defer lock.unlock();      // 提前返回或出错也会释放
    fs.write_file("report.txt", text)?;
    return ok;
}
```

**原子整数**：`load()` / `store(v)` / `add(v)` / `sub(v)` / `swap(v)` / `compare_swap(old, new)`，全部是 `SeqCst`；`add` / `sub` / `swap` 返回旧值。

**通道**（有界队列，传输值的深拷贝）：

```rp
ch.send(value) / ch.try_send(value) / ch.send_timeout(value, ms)
ch.recv() / ch.try_recv() / ch.recv_timeout(ms)   // 返回 option<any>
ch.close() / ch.is_closed() / ch.len() / ch.capacity()
```

- 满时 `send` 阻塞；`try_send` 返回 `false`；`send_timeout` 最多等 `ms`。
- 关闭后先读完排队值，再 `recv` 得到 `none`；之后 `send` 报错。
- 取出的值是 `any`，用 `.as_int()` / `.as_string()` 等收窄。
- 所有阻塞操作每 20ms 检查一次 HTTP 处理函数截止时间，请求超时立即中断；后台任务用 `*_timeout` 或 `try_*` 避免永久阻塞。
- `sync.exists(name)` / `sync.remove(name)` 用于检查与清理注册表；名字要用固定常量。

```rp
// 生产者：后台任务
func worker() -> void {
    let jobs: sync.channel = sync.channel("jobs", 64);
    jobs.send(42);
}

// 消费者：超时轮询
task.spawn("worker");
let jobs: sync.channel = sync.channel("jobs", 64);
match jobs.recv_timeout(1000) {
    some(value) => { dev.log(value.as_int().unwrap_or(0).to_string()); }
    none => { dev.log("1 秒内没有消息"); }
}
```

### 5.11 时区、Unicode、ZIP 与邮件

**时区**：内置 IANA 数据库（含夏令时），不依赖系统 tzdata。格式串与 `time.format` 一致，`%:z` 输出 `+08:00`。

```rp
match time.format_zone(ts, "%Y-%m-%d %H:%M:%S %:z", "Asia/Shanghai") {
    ok(text) => { dev.log(text); }
    err(message) => { log.error(message); }
}
match time.parse_zone("2024-07-01 08:00:00", "%Y-%m-%d %H:%M:%S", "Asia/Shanghai") {
    ok(ms) => { dev.log(ms.to_string()); }
    err(message) => { log.error(message); }
}
time.offset(ts, "America/New_York")     // ok(-14400)：夏令时已计入
time.zone_names() / time.local_zone()         // 全部时区名 / 系统时区名
```

- 未知时区返回 `err`，不会 panic。
- 夏令时回拨的重复时刻取较早一次；春季跳过的时刻返回 `err`。
- `time.format` / `time.parse` 使用进程本地时区；需要确定性时用 `*_zone`。

**Unicode**：`len` / `slice` 按码点，字素簇与显示宽度另算。

```rp
str.normalize(text, "nfc")        // nfc / nfd / nfkc / nfkd
str.graphemes(text)                 // vec<string>：emoji、组合音标不拆开
str.grapheme_len(text) / str.width(text)

"👨‍👩‍👧‍👦".len()              // 7（码点）
"👨‍👩‍👧‍👦".grapheme_len()     // 1（用户可见字符）
"中文".width()               // 4（终端列数）
```

**ZIP**：条目名必须是相对路径，绝对路径、盘符与 `..` 一律拒绝。

```rp
zip.create(path, files) -> result<int, string>          // map<string, string>
zip.add(path, files) -> result<int, string>
zip.list(path) -> result<vec<zip.entry>, string>
zip.extract(path, dir) -> result<int, string>
zip.read_text(path, entry) / read_base64(path, entry) / contains(path, entry)

match zip.create("dist.zip", ["a.txt" = "hello", "docs/b.txt" = "world"]) {
    ok(count) => { dev.log(count.to_string()); }
    err(message) => { log.error(message); }
}
```

- `zip.entry`：`name` / `size` / `compressed_size` / `crc32` / `is_dir`。
- 解压与读取上限：单条目 256 MiB、单次总计 2 GiB（压缩炸弹防护）。
- `read_text` 要求 UTF-8；二进制用 `read_base64`。

**邮件**：`mail.config()` 取默认值后逐字段覆盖，字段类型由编译器校验。

```rp
use std::mail;

let mut cfg: mail.config = mail.config();
cfg.host = "smtp.example.com";
cfg.port = 587;
cfg.tls = "starttls";                  // none / starttls / ssl
cfg.username = "bot@example.com";
cfg.password = "secret";                // 只在有 username 时使用
cfg.from = "bot@example.com";
cfg.to = ["ops@example.com"];
cfg.subject = "日报";
cfg.text = "纯文本正文";

match mail.send(cfg) {
    ok(_) => { dev.log("sent"); }
    err(message) => { log.error(message); }
}
```

- `text` + `html` 同时存在时发 `multipart/alternative`；有附件时外层 `multipart/mixed`。
- 附件用 `mail.attachment()`：`filename` 必填，内容取 `content_base64`（优先）或 `path`。
- `timeout_ms` 限制单次 socket 操作；`mail.send` 阻塞，HTTP 处理函数里请放到 `task.spawn` 的后台函数。

## 6. 项目与包

### 6.1 目录结构

```text
shop/
├── rip.toml            # 项目清单
├── main.rp             # 入口（mod shop;）
├── models/user.rp      # 子模块，mod models;
├── services/user.rp    # 子模块，mod services;
├── controllers/*.rp    # 路由处理函数
├── packages/           # 第三方 Rip 包（vendored）
├── migrations/         # 数据库迁移
├── tests/              # 测试
└── public/             # 静态资源
```

规则：

- 每个文件第一行必须是 `mod 目录名;`，`mod` 名与目录名完全一致。
- 同目录所有 `.rp` 属于同一个 mod；子目录是独立的 mod。
- 项目模式（存在 `rip.toml`）会递归合并所有 `.rp`；跨文件调用不需要 `use`，直接按函数名调用。

### 6.2 rip.toml 模板

```toml
[module]
name = "shop"
version = "0.1.0"
entry = "main.rp"

[build]
dir = "dist"
name = "shop"
# 随产物交付的数据目录
copy = ["storage", "uploads"]

[compile]
# 编译时跳过的数据目录
exclude = ["storage", "uploads"]

[dependencies]
# acme/tools = "^1.2"

[runtime]
log = "chinese"

[server]
workers = 0                 # 0 = 按 CPU 核数
max_connections = 1024
max_body_bytes = 8388608
request_timeout_ms = 30000
health_path = "/healthz"
ready_path = "/readyz"
metrics_path = ""           # 改成 "/metrics" 暴露指标
access_log = true
```

### 6.3 可见性与模块导入

```rp
pub func create_order(...) -> ... { }     // 导出给其他模块 / 包
pub type order = [id: int, total: int];

self func hash_password(raw: string) -> string { }   // 仅同目录（同 mod）可用
```

导入用 Rust 风格的 `use`，路径用 `::` 分段；调用仍保持 Rip 的 `模块.函数()`：

```rp
use std::http;              // 别名默认取最后一段：http
use std::sys as sys;        // 显式改名
use acme::tools;            // packages/acme/tools/ 下的包，别名 tools
use rust::serde_json as json;
```

规则：

- `use` 只导入模块，不支持 `use std::http::serve;` 这种逐项导入（唯一写法）。
- 导入后调用写 `http.serve(...)`，不写 `http::serve(...)`；方法调用仍是 `值.方法()`。
- 第三方包放在项目根 `packages/` 下，`use` 路径就是相对 `packages/` 的路径（`/` 换成 `::`）。
- `package`、`import` 不是关键字，写出来就是语法错误，没有兼容路径。

## 7. 编译、运行、打包、发布

| 命令 | 用途 |
| --- | --- |
| rip run main.rp | 直接运行源码；HTTP 默认监听 127.0.0.1 |
| rip run main.rp --host 0.0.0.0 --port 8080 | 对外提供服务 |
| rip run main.rp --cert a.pem --key a.key | 启用 HTTPS |
| rip dev main.rp | 开发模式：启动并热重载 |
| rip check main.rp [--json] | 只做静态检查（CI 门禁） |
| rip build | 编译原生二进制，输出到 dist/ 与 dist.zip |
| rip build -o release --no-archive | 指定输出目录 / 不生成 zip |
| rip test [dir\|file] | 运行全部 #[test] |
| rip test --coverage --coverage-min 80 | 语句覆盖率，低于阈值失败 |
| rip bench [dir\|file] | 运行 #[bench]，输出耗时统计 |
| rip fmt [file\|dir] [--check] | 格式化；--check 只检查 |
| rip new \<name\> [--template http\|cli\|empty] | 创建项目骨架（含 GitHub / GitLab CI 模板） |
| rip doc [file] [-o \<dir\>] [--json] | 从 /// 注释生成单文件 HTML API 文档 |
| rip search \<关键词\> [--registry URL] | 搜索包源（--json 输出结构化结果） |
| rip info \<包名\> [--registry URL] | 查看包的全部版本、依赖与校验和 |
| rip audit [--deny high] [--licenses] [--json] | 依赖锁定 / 安全公告 / 许可证审计 |
| rip migrate up\|down\|status | 数据库迁移（migrations/） |
| rip doctor | 环境与项目自检 |
| rip profile [file] [--top N] [--json] | 函数级耗时报告（调用次数 / 总耗时 / 自身耗时） |
| rip dap [--port N] | DAP 调试适配器（stdio 或 TCP） |
| rip build --target \<triple\> | 交叉编译到指定 Rust 目标 |

**调试**：编辑器通过 `rip dap` 接入，支持断点、`next` / `stepIn` / `stepOut`、暂停、调用栈、Locals / Globals 与变量名求值；程序输出走 DAP `output` 事件，不会破坏协议。会大量输出的服务建议用 `rip dap --port 4711`，编辑器以 `debugServer` 方式连接。仓库的 `editors/vscode/` 提供了语法高亮、LSP（`rip lsp`）与 DAP 调试配置，`rip.executable` 指定工具链路径。

**性能**：`rip profile main.rp` 运行结束后按总耗时排序列出函数，`--top 0` 显示全部，`--json` 便于接入 CI；闭包与 `std/*` 调用也会统计。

**交叉编译**：`rip build --target x86_64-unknown-linux-gnu` 会用 cargo 为目标平台构建运行时并产出二进制；需要先 `rustup target add <triple>`，并在 `.cargo/config.toml` 或 `RIP_TARGET_LINKER` 配置链接器（sqlite / openssl 等 C 依赖也要有对应的交叉工具链）。目标与宿主相同时仍走内置编译器的快速路径。

**打包产物**：`rip build` 默认把可执行文件、静态资源、`[build] copy` 声明的数据目录、README 一起放进 `dist/`；`flat` 布局全部在根目录，`typed` 布局分为 `bin/`、`data/`、`share/`。交付目录里只需一个可执行文件即可运行。

**运行期配置**：可执行文件旁放 `config/rip.toml` 即可覆盖 `[server]` / `[runtime]` / `[db]` 等参数，无需重新编译；也支持 `RIP_SERVER_HOST`、`RIP_SERVER_WORKERS` 等环境变量。

## 8. 开发自己的独立包

### 8.1 包的形态

```text
packages/acme/tools/
├── rip.toml        # [module] name / version 必填
└── tools.rp        # mod tools;
```

导入方：

```rp
use acme::tools;
tools.slugify("Hello World");
```

- 目录名（`tools`）必须等于文件里的 `mod tools;`。
- 只有 `pub` 函数 / 类型能跨包使用；`self func` 是包内私有。
- 合并编译后所有函数名、类型名全局唯一，命名时带上业务前缀避免冲突。

### 8.2 编写与发布

```rp
mod tools;

pub func slugify(text: string) -> string {
    return str.lower(str.replace(str.trim(text), " ", "-"));
}

self func internal_note() -> string {
    return "not exported";
}
```

```bash
rip check packages/acme/tools/tools.rp     # 本地检查
rip pack packages/acme/tools               # 生成 tools.rpa 预编译包
rip pub packages/acme/tools --registry https://pkg.example.com
```

- 发布前必须在 `rip.toml` 写 `[module] name` 与合法的 `version`；同版本重复发布会报错。
- 引用方按语义化版本约束声明依赖：

```toml
[dependencies]
acme/tools = "^1.2"
```

```bash
rip add acme/tools@^1.2      # 写清单并立即拉取到 packages/
rip vendor                   # 只拉取依赖（离线构建用）
rip lock --check             # CI 校验锁定版本
```

- `rip pack` 生成 `.rpa` 后可以删掉源码，导入方式不变。
- 内置 Rust 包用 `use rust::serde_json as json;`，调用方式与普通包一致。

## 9. 部署要点

```bash
rip build main.rp -o release
# 服务器上：
./shop --host 0.0.0.0 --port 8080
```

- 对外监听必须显式 `--host 0.0.0.0`（默认只监听回环地址）。
- HTTPS：`--cert` 与 `--key` 必须同时给出。
- 内置探针：`/healthz`（存活）、`/readyz`（就绪）；`metrics_path = "/metrics"` 时暴露 Prometheus 指标。
- 反代之后开启 `trust_proxy = true` 或配置 `trusted_proxies`，客户端 IP 才取 `X-Forwarded-For`；只开 `trust_proxy` 未配 `trusted_proxies` 时启动会打印伪造告警。
- 追踪：`otlp_endpoint = "http://127.0.0.1:4318"` 后自动导出 OTLP/HTTP trace，日志里的 `trace_id` 可直接关联。
- 需要限流时配置 `rate_limit` / `rate_limit_burst`，超出返回 `429`。

systemd 最小示例：

```text
[Unit]
Description=shop
After=network.target

[Service]
WorkingDirectory=/srv/shop
ExecStart=/srv/shop/shop --host 0.0.0.0 --port 8080
Restart=always

[Install]
WantedBy=multi-user.target
```

## 10. 常见错误速查

| 报错 | 原因 | 修正 |
| --- | --- | --- |
| mod name 'x' must match directory name 'y' | `mod` 名与目录名不一致 | 把 `mod` 改成目录名 |
| every .rp file in directory ... must start with 'mod ...;' | 缺少模块声明 | 首行补 `mod 目录名;` |
| type mismatch for 'x': expected string, found int | 类型不符 | 显式转换 .to\_string() / .to\_int() |
| cannot assign to immutable variable | 给 let 变量赋值 | 改成 let mut |
| use of moved value | 值已按值传参 | 传参前 .clone()，或参数写 read |
| ? can only be used in a function returning result | 函数返回值不是 result | 改签名或改用 match |
| match is not exhaustive | 缺 some/none 或 ok/err | 补齐分支 |
| match arm uses '=\>', not '=' | match 分支写成了 = | 改成 `=>` |
| for requires \`for \<name\> in \<vec\|map\>\` | 用了条件 for 或 C 风格 for | 改成 `while` / `loop` / `for x in y` |
| type vec\<T\> has no method 'x' | 方法名拼错 | 查第 4 节方法表 |
| v[0] 编译报错 | vec 不能用下标 | 用 v.0 或 v.at(i) |
| scores["k"] 编译报错 | map 不能用下标 | 用 scores.get("k") |
| route handler must take exactly one http.request | 路由函数签名不符 | 改成 func(read req: http.request) -\> http.response |
| package "std/x" is not available | 没导入或模块名错 | 补 `use std::x;` |
| unknown attribute '#[x]' | 注解名不合法 | 只支持路由、interval、test、bench |
| decimal integer literal cannot initialize string | 字面量与类型不符 | 加引号或显式转换 |

## 11. 完整示例

### 11.1 JSON API（路由 + 校验 + JWT）

```rp
mod api;

use std::http;
use std::jwt;
use std::validate;

static SECRET: string = "change-me";

#[post("/login")]
func login(read req: http.request) -> http.response {
    let name: string = req.form.get("name").unwrap_or("");

    match validate.string_len(name, 1, 32) {
        ok(valid) => {
            let token: string = jwt.sign(["sub" = valid], SECRET, 3600);
            return http.response[status = 200, json = [token = token]];
        }
        err(message) => {
            return http.response[status = 400, json = [error = message]];
        }
    }
}

#[get("/me")]
func me(read req: http.request) -> http.response {
    let token: string = req.headers.get("authorization").unwrap_or("");
    match jwt.verify(token, SECRET) {
        ok(claims) => { return http.response[status = 200, json = some(claims)]; }
        err(message) => { return http.response[status = 401, body = message]; }
    }
}

func main() -> void {
    let routes: vec<http.route> = [];
    http.serve(8080, http.router(routes));
}
```

### 11.2 分页查询（ORM）

```rp
func list_users(read conn: db.conn, page: int, size: int) -> result<vec<map<string, any>>, string> {
    let offset: int = (page - 1) * size;
    return db.table(conn, "users")
        .field("id, name, email")
        .order("id", "desc")
        .limit(size)
        .offset(offset)
        .select();
}
```

### 11.3 定时任务 + 队列

```rp
#[interval("30s")]
func send_mail() -> void {
    match queue.reserve("storage/jobs.db", "mail", 60000) {
        ok(job) => {
            match job {
                some(item) => {
                    // 调用邮件服务；失败时交给队列重试
                    if item.attempts > 3 {
                        queue.fail("storage/jobs.db", item.id, "too many attempts");
                    } else {
                        queue.ack("storage/jobs.db", item.id);
                    }
                }
                none => { }
            }
        }
        err(message) => { log.error(message); }
    }
}
```

## 12. 依赖管理：rip vendor

> 本节由《rip vendor 使用指南》合并而来，讲的都是使用者视角的命令与工作流。

`rip vendor` 把 `[dependencies]` 声明的第三方包（含传递依赖）补齐到项目的 `packages/` 目录，并把当前依赖状态固化到 `rip.lock`。

一句话：**让一份刚克隆下来的仓库，能在不联网的情况下构建出与原作者完全一致的依赖。**

---

### 12.1 命令

```bash
rip vendor            # 拉取缺失依赖，生成/更新 rip.lock
rip vendor --check    # 只校验，不写入（CI 用）
```

| 项目 | 说明 |
| --- | --- |
| 执行目录 | 必须在含 rip.toml 的项目根目录 |
| 全局选项 | --offline（不联网，缺包直接报错）、--deny-warnings |
| 退出码 | 0 成功；1 依赖或校验失败；2 当前不是 Rip 项目 |

正常输出：

```text
vendored 2 package(s) into .\packages; wrote .\rip.lock
```

没有依赖时：

```text
no packages to vendor; wrote .\rip.lock
```

校验通过时：

```text
rip.lock: ok (2 packages)
```

---

### 12.2 快速开始

`rip.toml`：

```toml
[module]
name = "app"
version = "0.1.0"
entry = "main.rp"

[registry]
source = "https://registry.example.com"

[dependencies]
"acme/tools" = "^1.2"
```

```bash
rip vendor
```

执行后：

```text
app/
├── rip.toml
├── rip.lock
└── packages/
    └── acme/tools/
        ├── .rip-registry     # 由 rip 写入的来源标记
        ├── rip.toml
        └── tools.rp
```

把 `packages/` 与 `rip.lock` 一起提交，仓库即可离线构建。

---

### 12.3 它做了什么

`rip vendor` 依次执行三步（与 `rip lock` 共用同一实现）：

- 本地已有且版本满足约束 → 不访问网络。
- 本地缺失或不满足 → 从 `[registry]` 拉取。
- 锁文件里已有该包时优先用锁定版本，保证可复现。

1. **校验约束**：版本区间、同一包的多处约束是否冲突、传递依赖是否完整。
2. **固化结果**：扫描 `packages/`，按**包目录**聚合，写入 `rip.lock`。

`packages/` 本身就是 Rip 的 vendored 目录，所以“拉取”和“锁定”在这里是同一个动作。

---

### 12.4 锁文件格式

```toml
# generated by 'rip lock'; do not edit
version = 1
registry = "https://registry.example.com"   # 仅当配置了 [registry] source

[[package]]
name = "acme/tools"                # 包目录相对 packages/ 的路径
version = "1.4.0"                  # 来自包内 rip.toml 的 [module] version
source = "packages/acme/tools"     # 本地目录
sha256 = "26c1200f1fb6…"           # 包目录内全部文件的摘要
deps = ["acme/base"]               # 直接依赖的包名
```

几条约定：

- **一个包目录一条记录**。同一目录下的多个 `.rp` 属于同一个包，不会重复出现。
- **包名 = 目录相对路径**，与 `use acme::tools;`、`[dependencies]` 的键一致。
- **摘要覆盖整目录**：按相对路径排序后，把「路径 + 文件内容」一起做 SHA-256，多文件包也能稳定校验。
- 不要手工编辑；需要更新时重新执行 `rip vendor`。

---

### 12.5 校验规则

`rip vendor --check`，以及 `rip run` / `rip dev` / `rip build` / `rip check` / `rip test`，都会在读取项目时校验锁文件：

| 情况 | 提示 |
| --- | --- |
| 锁文件缺失 | rip.lock is missing; run 'rip vendor' to generate it |
| 包内容被改动 | package 'x' does not match rip.lock |
| 包版本变化 | package 'x' version changed from 'a' to 'b' |
| 直接依赖列表变化 | package 'x' dependency list changed |
| 锁里的包不见了 | package 'x' from rip.lock is missing |
| packages/ 里多了未记录的包 | package 'x' is not recorded in rip.lock |

出现以上任一提示，重新执行 `rip vendor` 即可。

> 注意：`--check` 要求锁文件已经存在。首次克隆的仓库请先跑一次 `rip vendor`。

---

### 12.6 典型工作流

#### 首次引入依赖

```bash
rip add acme/tools@^1.2   # 写入 rip.toml 并立即拉取
rip vendor                # 生成 rip.lock
```

提交 `rip.toml`、`rip.lock` 与 `packages/`。

#### 新克隆仓库

```bash
rip vendor     # 还原 packages/ 与 rip.lock
rip run main.rp
```

若 `packages/` 已随仓库提交，这一步不会访问网络。

#### 升级依赖

```bash
# 修改 rip.toml 的约束，例如 ^1.2 -> ^1.3
rip vendor
```

- 旧锁定版本仍满足新约束时，沿用锁定版本（保持可复现）。
- 旧版本不满足新约束时，重新从包源按依赖约束做回溯求解（同一包只选一个版本）；无解时报出每条约束的来源。

#### CI

```bash
rip vendor --check
```

只校验、不写入；锁文件与 `packages/` 不一致时返回非零退出码。

安全公告与许可证检查：`rip audit --deny high --licenses`（公告来源 `[registry] advisories` / `RIP_ADVISORIES` / `<source>/v1/advisories.json`）；查看包源用 `rip search <关键词>` 与 `rip info <包名>`。

完全离线环境：

```bash
rip vendor --check --offline
```

---

### 12.7 与包源、离线的关系

- 包源由 `[registry] source` 指定，支持 `https://`、`file://` 与本地目录；`http://` 仅允许 `localhost` / `127.0.0.1` / `::1`（本地开发），其它 http 源直接报错拒绝。远端源的下载必须带索引 `sha256`，缺摘要即拒绝。未配置时使用公共包源 `https://registry.rip-lang.dev`，可用环境变量 `RIP_REGISTRY` 覆盖（`rip pub` 仍需显式指定目标，不回退公共源）；依赖已全部 vendored 时不会访问包源。
- 访问令牌：`[registry] token_env = "MY_TOKEN"` 指定环境变量名，未指定时读 `RIP_REGISTRY_TOKEN`。
- `--offline` 或 `[registry] offline = true` 时不访问网络；此时缺失依赖会直接报错：

```text
  error: package 'acme/tools' is not vendored and network access is disabled
```

- 由 rip 拉取的包目录内有 `.rip-registry` 标记；**手工放进 `packages/` 的包不会被静默覆盖**，需要先删除或调整为匹配的版本。

---

### 12.8 常见问题

**Q：`packages/` 要不要提交到仓库？**

两种都可以：

- 提交：完全离线可构建，仓库体积更大；
- 不提交：靠 `rip vendor` 从包源还原，需要能访问 `[registry]`。

**Q：`rip vendor` 和 `rip lock` 有什么区别？**

解析与写入逻辑完全一致。`rip vendor` 的输出侧重“vendored 了多少个包”，语义上强调把依赖固化到 `packages/`；两者生成的锁文件逐字节相同。

**Q：为什么改了约束却没有升级？**

因为锁定版本仍满足新约束。要强制重新选版，先把 `rip.lock` 里对应记录删除（或整体删除锁文件）再执行 `rip vendor`。

**Q：提示 `already exists under packages/ ... was not fetched by rip`？**

那个目录是手工放置的。删除它让 `rip vendor` 重新拉取，或把 `[dependencies]` 的约束调整成与该目录内的版本一致。

**Q：提示 `checksum mismatch`？**

包源返回的 zip 与索引里的 `sha256` 不一致。确认包源内容是否被替换，或联系包源维护者。

**Q：如何发布自己的包？**

`rip vendor` 只负责消费依赖。发布用 `rip pub`，见本手册第 7 章「编译、运行、打包、发布」。
