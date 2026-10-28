# Rip Demo 与项目目录结构规范

前后端目录分离的用户管理示例：登录，登录后对用户信息做增删改查搜。文末同时给出用 Rip 开发服务端 / Web 项目的目录结构规范。

> `demo/` 是用 Rip 语言写的**验证示例**，不是语言规范的一部分。Rip 是面向所有人的通用语言，示例服从规范，规范不服从示例。
>
> 语言规则见 [Rip 语言开发规范](../doc/RipLang-level.html) 与 [Rip 语言手册](../doc/RipLang-help.html)。

---

# 一、示例（demo/）

## 目录

```text
demo/
├── client/                     # Vue 3 前端（无构建步骤，浏览器直接运行）
│   ├── index.html              # 登录页 + 用户管理页
│   ├── app.js                  # 接口调用、token 管理、列表 / 搜索 / 弹窗
│   ├── style.css
│   └── vue.global.prod.js      # Vue 3 生产版（本地文件，免联网）
├── rip.ps1                     # 便捷命令：自动定位 rip.exe
├── stdlib/                     # 系统包自检示例（encode / xml / crypto / net / ws）
│   └── main.rp
└── server/                     # Rip 后端（MVC 分层，多文件项目）
    ├── rip.toml                # name = "server"，entry = "main.rp"
    ├── main.rp                 # 入口：func main() -> void
    ├── config.rp               # DSN、会话有效期
    ├── routes.rp               # 显式路由表（只保留静态资源）
    ├── models/                 # 模型层：表结构与数据访问（链式 ORM）
    │   ├── schema.rp           # 建表（按方言）、种子管理员
    │   ├── user.rp             # users 表：find / count / page / insert / update / delete
    │   └── session.rp          # sessions 表：登录态
    ├── services/               # 服务层：业务规则，返回 service_result
    │   ├── result.rp           # service_result 与 svc_ok / svc_created / svc_fail
    │   ├── auth_service.rp     # 登录 / 退出 / 当前用户
    │   └── user_service.rp     # 列表 / 详情 / 新增 / 修改 / 删除
    ├── controllers/            # 控制器层：解析请求、调用服务、组装响应
    │   ├── auth_controller.rp
    │   ├── user_controller.rp
    │   ├── file_controller.rp   # 文件上传
    │   └── home_controller.rp
    ├── middleware/             # 鉴权中间件
    │   └── auth.rp
    ├── tasks/                  # 定时任务（#[interval] 注解，独立线程）
    │   └── maintenance.rp
    ├── support/                # 通用工具：请求读取、响应封装、密码 / token / 校验
    │   ├── request.rp
    │   ├── response.rp
    │   ├── security.rp
    │   └── database.rp         # 按请求打开连接，交给处理器后再关闭
    ├── migrations/
    │   └── 0001_create_users_and_sessions.sql
    ├── storage/                # 运行期生成 app.db
    ├── uploads/                # 上传文件目录（启动时自动创建）
    └── test.ps1                # 接口自动化测试
```

项目模式：`demo/server` 下有 `rip.toml`，`rip run` 会递归收集所有 `.rp` 合并编译，各目录独立成模块（`models`、`services`、`controllers`、`middleware`、`tasks`、`support`），跨文件直接按函数名调用，无需 import。

## 启动

需要用 `rip` 命令；若提示 `无法将“rip”项识别为...`，说明 `rip.exe` 不在 PATH。仓库内没配就使用 `demo` 下的 [rip.ps1](rip.ps1)（自动定位 `target/debug` 或 `target/release` 里的 `rip.exe`）：

```powershell
# 方式一：用快捷脚本（任意目录下都行，脚本会自动 cd 到 server）
cd demo
.\rip.ps1 server --port 8080

# 方式二：已配置 rip 命令，手动从 server 目录启动
# 静态资源路径 ../client 与 SQLite 路径 storage/app.db 都按工作目录解析，
# 所以必须 cd 到 server。
cd demo/server
rip run main.rp --port 8080      # 也可 rip run . --port 8080
```

默认只监听 `127.0.0.1`，启动日志会打印实际地址：

```text
Rip HTTP server listening on http://127.0.0.1:8080
```

需要局域网访问或正式部署时显式指定绑定地址（IP 或本机可解析的域名）：

```powershell
rip run main.rp --host 0.0.0.0 --port 8080
```

浏览器打开 <http://127.0.0.1:8080>，默认管理员：

```text
admin@example.com / admin123
```

## 路由：注解自动注册

API 路由直接声明在控制器函数上，`http.router` 构建时自动注册，`routes.rp` 不再重复写 API：

```rp
#[get("/api/users")]
func users_index_action(read req: http.request) -> http.response {
    return with_conn(req, users_index_handle);
}
```

- 注解是 Rip 语言特性（[Rip 语言开发规范](../doc/RipLang-level.html) §2.6、§11.2.1），不是注释；写成 `/// #[get(...)]` 不会注册路由。
- 处理器签名固定为 `func(read req: http.request) -> http.response`，框架不注入额外参数。
- 数据库连接由 `support/database.rp` 的 `with_conn` 按请求打开、调用处理器、再关闭，处理器主体写成 `xxx_handle(read conn: db.conn, read req: http.request)`。
- 注解路由先注册，`routes.rp` 里的显式路由（只有 `http.static("/", "../client")`）后注册，静态通配不会盖住 API。
- 同一 `(方法, 路径)` 重复声明、处理器签名不符、未知属性名都是编译错误，`rip check` 直接失败。

## 测试

```powershell
cd demo/server
powershell -ExecutionPolicy Bypass -File .\test.ps1
```

测试使用独立的 `storage/tests/app_test.db`，每次运行前重建它；demo 正式库 `storage/app.db` **不会被删除或修改**，数据一直保留。脚本启动 18081 端口的临时实例，覆盖：静态资源、登录成功/失败、未授权 401、新增、详情、更新、按姓名/邮箱/手机号搜索、按状态筛选、重复邮箱 409、参数校验 400、无效 token 401、删除后 404、退出后 token 失效。

## 打包运行

`rip build` 把服务端项目（`main.rp` + `models/`、`services/`、`controllers/`、`middleware/`、`support/`、`tasks/` 等全部 `.rp`）合并进一个可执行文件，并把交付所需文件收集到 `dist/`，**交付时只需要给客户 `dist/` 整个目录**，目标机器不需要安装 `rip`。

本项目是前后端分离的服务端，`client/` 仅用于本地验证，**不进入交付包**：

```powershell
cd demo/server
rip build main.rp

# dist/ 内容（具体随包目录由 rip.toml 的 [build] copy 决定）：
#   server.exe       可执行程序（源码已内嵌）
#   README.md        自动生成：应用名称、版本号、启动方式
#   storage/         SQLite 数据库文件随包交付，数据不会丢
#   uploads/         已上传文件随包交付（存在时）
#   migrations/      数据库迁移脚本
```

输出目录与可执行名可在 [rip.toml](file:///e:/bak/rip/demo/server/rip.toml#L6-L10) 配置，也可用命令行覆盖：

```powershell
rip build main.rp -o release --name user_server
```

运行时自身提示（启动日志、运行时错误、任务日志、`std/log` 标签）默认英文；在 `rip.toml` 增加 `[runtime] log = "chinese"` 即切换为中文，`rip build` 会把该配置写入产物，交付后无需再带 `rip.toml`。本 demo 显式保留为 `english`。

在产物目录内启动（数据库路径相对工作目录，所以必须在该目录内运行）：

```powershell
cd demo/server/dist
.\server.exe --port 8080
```

`dist/` 与 `build/` 都不会被项目编译单元收集。已实测：`dist` 内含 `server.exe`、`README.md`、`storage/app.db`、`migrations/`；启动后 `/api/me` 返回 401，原有的管理员与用户数据仍在。

## 第三方包

项目根下的 `packages/`（也接受 `package/`）存放别人开发的包，use 路径就是相对包目录的路径（/ 换成 ::）：

```text
demo/server/
├── main.rp
├── rip.toml
└── packages/
    ├── utils/http.rp        # use utils::http;
    └── vendor/logger.rpa    # use vendor::logger as logger;
```

- 包内可以是 Rip 源文件（`.rp`），也可以是预编译包（`.rpa`）；两者混用，编译时与项目源码合并为同一编译单元。
- 用 `rip pack packages/utils/http` 把源码包编译成 `.rpa`，之后删除 `.rp` 仍可继续使用；`rip build` 遇到 `rip-version` 不一致会报错。
- 包名可以多段（`utils/http`），包内 `pub` 函数按全局函数名调用，调用前用 `use utils::http;` 引入别名。
- 交付时包已内嵌进可执行文件；`[build] layout = "typed"` 会额外把包复制到 `dist/lib/packages/`，便于交付后查看或替换。本 demo 使用缺省 `flat` 布局。

## 并发与集群

- `http.serve` 默认按 CPU 核数启动工作线程，可被 [rip.toml](file:///e:/bak/rip/demo/server/rip.toml#L28-L39) 的 `[server] workers` 覆盖。每个线程一个独立解释器，模块级 `static` 不跨请求共享。
- 支持 keep-alive、读 / 写超时、请求体与头部上限、连接队列上限；队列满返回 `503`，请求体超限返回 `413`。
- 内置探针：`GET /healthz`（存活）、`GET /readyz`（就绪，停机中返回 `503`）；把 `[server] metrics_path` 设为 `"/metrics"` 可暴露 Prometheus 指标，也可用 `http.metrics()` 自己挂路由。
- 收到 Ctrl+C / SIGTERM 后停止接收新连接，处理完已排队请求再退出，适合容器与 K8s 滚动发布。
- 多实例部署时在 `rip.toml` 打开 `[cluster]`：`redis` 提供会话 / 缓存 / 分布式锁底座，`task_lock = true` 让 `#[interval]` 任务用 Redis 租约选主，避免每台机器重复执行。Redis 用法见 `std/redis`（`redis.lock`、`redis.rate_limit`）。
- 单实例部署不需要 Redis，`[cluster]` 保持注释即可；服务端并发、超时、探针全部与是否集群无关。

## 定时任务与后台任务

定时任务用注解声明，服务启动时自动调度，运行在独立线程：

```rp
#[interval("1m")]
func session_cleanup() -> void {
    // 每分钟清理过期会话
}
```

- `#[interval("...")]` 支持 `ms` / `s` / `m` / `h` / `d`，函数必须无参。
- 本 demo 的 [tasks/maintenance.rp](file:///e:/bak/rip/demo/server/tasks/maintenance.rp) 每分钟清理过期会话。
- 一次性后台任务用 `std/task`：`task.spawn("函数名")` 异步执行后立即返回，`task.sleep(毫秒)` 让当前线程休眠。
- 每个任务有独立的解释器状态与数据库连接，不与 HTTP 请求共享 Rip 变量。
- 需要跨线程共享时用 `std/sync` 的命名原语（`sync.mutex` / `sync.atomic` / `sync.channel`）：同一进程内同名即同一对象，锁记得配 `defer unlock()`。

## 接口

| 方法 | 路径 | 说明 |
| --- | --- | --- |
| POST | `/api/login` | `{account, password}` → `{token, expires_at, user}` |
| POST | `/api/logout` | 删除当前 token 的会话 |
| GET | `/api/me` | 当前登录用户 |
| GET | `/api/users` | `page`、`limit`、`keyword`（姓名/邮箱/手机号）、`status` |
| POST | `/api/users` | 新增用户 |
| GET | `/api/users/:id` | 用户详情 |
| PUT | `/api/users/:id` | 更新用户（字段可部分提交） |
| DELETE | `/api/users/:id` | 删除用户（不能删除当前登录用户） |
| POST | `/api/upload` | `multipart/form-data` 上传文件，返回落盘路径 |

统一响应：`{"code":0,"message":"ok","data":...}`；出错为 `{"code":<HTTP 状态>,"message":"..."}`。

鉴权：除登录外，所有 `/api/*` 都需要 `Authorization: Bearer <token>`。

## 文件上传

运行时把 `multipart/form-data` 请求解析成 `req.files`（`vec<http.file>`），控制器用 `http.save_file(目录, 文件)` 落盘：

```rp
#[post("/api/upload")]
func upload_action(read req: http.request) -> http.response {
    return with_conn(req, upload_handle);
}

func upload_handle(read conn: db.conn, read req: http.request) -> http.response {
    let dir: string = config_upload_dir();
    let mut saved: vec<string> = [];
    for file in req.files.clone() {
        match http.save_file(dir, file) {
            ok(path) => { saved.push(path); }
            err(message) => { }
        }
    }
    return http.response[status = 201, json = [files = saved]];
}
```

- 上传目录由 [config.rp](file:///e:/bak/rip/demo/server/config.rp) 的 `config_upload_dir()` 决定：`RIP_UPLOAD_DIR` 优先，缺省 `uploads`；相对路径按运行目录解析。运行时不内置任何上传目录，也不限制写入位置。
- `main.rp` 启动时用 `fs.make_dir` 创建该目录，宿主目录不存在也能自动建立。
- `http.file` 字段：`field`、`filename`、`content_type`、`size`、`data_base64`；`http.save_file` 解码并安全化文件名（去掉路径分隔符，只保留字母数字与 `. - _`），返回落盘路径。
- `rip build` 只复制 `[build] copy` 里声明的路径；本 demo 声明了 `storage` 与 `uploads`，交付后在 `dist` 内启动会继续写入同一目录，重启/升级不丢文件。换成 OSS / COS 等第三方存储时，改为在代码里用 `http.fetch` 上传即可，编译与打包配置都不用改。

```powershell
curl.exe -H "Authorization: Bearer <token>" -F "file=@logo.png" http://127.0.0.1:8080/api/upload
# {"code":0,"message":"uploaded","files":["uploads\\logo.png"],"failed":0}
```

需要放到别的位置时设绝对路径：`$env:RIP_UPLOAD_DIR = "D:\data\uploads"`。

## 密码哈希与加密（std/crypto）

- [support/security.rp](file:///e:/bak/rip/demo/server/support/security.rp) 已不再自造哈希：`password_hash` / `password_verify` 直接调用 `crypto.password_hash` / `crypto.password_verify`，存储格式为 `ript$迭代次数$盐$摘要`（PBKDF2-HMAC-SHA256，210000 次迭代 + 16 字节随机盐），同一密码每次结果不同。
- 会话 token 用 `crypto.sha256` 与 `crypto.random_hex` 生成。
- 旧库检测到非 `ripx$` 前缀的密码时，启动阶段自动重新哈希种子管理员密码（已是 `ript$` 前缀时跳过）（日志 `upgrade admin password to std/crypto hash`）。
- 其他常用函数：`md5`、`sha1`、`sha256`、`sha512`、`hmac_sha256`、`base64_encode` / `base64_decode`、`hex_encode` / `hex_decode`、`random_hex`。
- 对称加密用 `crypto.aes_encrypt` / `crypto.aes_decrypt`（AES-256-GCM，密钥为任意字符串），签名用 `crypto.rsa_generate` / `crypto.rsa_sign` / `crypto.rsa_verify`（RSA PKCS#1 v1.5 + SHA-256），加解密用 `crypto.rsa_encrypt` / `crypto.rsa_decrypt`。
- 完整签名见 [Rip 语言开发规范](../doc/RipLang-level.html) §11.6。

## HTTPS / TLS

`http.serve` 的代码不用改，启动时用 CLI 提供 PEM 证书链与私钥即可切换到 HTTPS：

```powershell
rip run main.rp --cert server.pem --key server.key --port 8443
# Rip HTTP server listening on https://127.0.0.1:8443
```

- `--cert` 为 PEM 证书链（首个为站点证书），`--key` 为 PEM 私钥（PKCS#8 / PKCS#1）。
- 两者必须同时给出；缺一个时启动即报 `HTTPS requires both --cert and --key`。
- 证书与私钥不参与 `rip build` 打包，部署时放在目标机器的指定路径，用参数指过去即可。
- `rip dev` 同样支持 `--cert` / `--key`。

## 其他系统包

`demo/stdlib/main.rp` 是新增系统包的自检示例，运行：

```powershell
rip run demo/stdlib/main.rp
```

覆盖内容：

| 包 | 能力 | 示例 |
| --- | --- | --- |
| `std/encode` | URL / HTML / CSV / 查询串编解码 | `encode.url_encode`、`encode.csv_encode`、`encode.query_parse` |
| `std/xml` | XML 解析、查找、序列化 | `xml.parse`、`xml.find_all`、`xml.pretty` |
| `std/crypto` | 摘要、Base64/Hex、AES-256-GCM、RSA、密码哈希 | `crypto.aes_encrypt`、`crypto.rsa_sign` |
| `std/net` | TCP 连接 / 监听、UDP 收发 | `net.tcp_listen`、`net.udp_bind` |
| `std/ws` | WebSocket 服务端与客户端 | `ws.serve`、`ws.connect`、`ws.send` |
| `std/http` | 出站请求：第三方 API、Webhook、对象存储 | `http.fetch`、`http.fetch_file` |

`std/net` 与 `std/ws` 是同步阻塞 API：服务端监听循环用 `task.spawn("函数名")` 放到独立线程。完整签名与规则见 [Rip 语言开发规范](../doc/RipLang-level.html) §11.7–§11.10、[Rip 语言手册](../doc/RipLang-help.html) 的「标准库组件」章节。

## 数据库

默认 SQLite `storage/app.db`，首次启动自动建表并写入管理员；之后每次启动都复用同一个文件，数据长期保留（建表用 `CREATE TABLE IF NOT EXISTS`，不会清库）。切换到其它数据库只需改 DSN：

```powershell
$env:RIP_DB = "mysql://root:123456@127.0.0.1:3306/rip_demo"
rip run main.rp --port 8080
```

支持 SQLite（路径或 `sqlite::memory:`）、MySQL/MariaDB、PostgreSQL、SQL Server。建表语句会按 `db.driver` 生成对应方言的自增列与文本类型；`migrations/0001_create_users_and_sessions.sql` 是结构记录，切库时按其中的方言说明改写。

## 调用链

`controllers` 解析请求 → `services` 执行业务规则 → `models` 走链式 ORM → `support` 负责请求读取 / 响应封装 / 校验。控制器只做参数解析与响应组装，业务规则不写在控制器里。

---

# 二、项目目录结构规范

> 适用：Rip v1.3 工具链 0.1.0　包清单：`rip.toml`　入口：`<项目目录名>.rp` 或 `main.rp`
>
> 本节规定使用 Rip 开发服务端 / Web 项目的标准目录、命名和入口约定。工具链按项目模式递归收集 `.rp` 文件合并编译，目录即包；下面的分层结构直接生效。

## 1. 模块与目录

Rip 的模块声明就是目录名（Rust 风格的 `mod`）：

- 每个 `.rp` 文件第一行必须写 `mod 目录名;`。
- `mod` 名必须等于该文件所在目录的目录名，完全一致，使用单段 `snake_case`。
- 同一目录下所有 `.rp` 文件必须声明同一个 `mod` 名。
- 子目录是独立模块，`mod` 名同样等于该子目录名。
- `mod` 名不接受点分名；`module` 不是关键字。
- 只给本模块用的辅助函数加 `self`：`self func helper(...)`，其它模块调用会编译失败。

```text
myapp/
├── myapp.rp           # mod myapp;
├── models/
│   └── user.rp        # mod models;
└── tests/
    └── test_user.rp   # mod tests;
```

缺少 `mod` 声明、`mod` 名与目录名不一致、同目录 `mod` 名不一致，都会被 `rip check` / `rip run` / `rip build` 直接拒绝。

## 2. 标准结构

```text
myapp/
├── rip.toml                  # 包清单：名称、版本、入口、依赖
├── main.rp                   # 程序入口（mod myapp），必须包含 func main() -> void
├── config.rp                 # 配置加载（mod myapp）
├── routes.rp                 # 显式路由表（可选，静态资源兜底）
├── models/                   # 数据模型（mod models）
│   └── user.rp
├── services/                 # 业务逻辑（mod services）
│   └── user_service.rp
├── controllers/              # HTTP 控制器（mod controllers）
│   └── user_controller.rp
├── middleware/               # 中间件（mod middleware）
│   └── auth.rp
├── support/                  # 通用工具（mod support）
│   ├── request.rp
│   └── response.rp
├── public/                   # 静态资源，供 http.static 使用
│   ├── index.html
│   └── assets/
├── storage/                  # 运行期数据：SQLite 文件（若用）、日志
│   ├── app.db
│   └── logs/
├── uploads/                  # 运行期上传文件（不参与编译，打包时随包复制）
├── tests/                    # 测试用 .rp 文件（mod tests），rip test 递归收集
│   ├── test_user.rp
│   └── test_api.rp
├── migrations/               # SQL 迁移脚本，按序号命名
│   ├── 0001_create_users.sql
│   └── 0002_add_user_status.sql
└── docs/                     # 项目文档（可选）
```

## 3. 目录职责

| 目录 | 职责 | 禁止 |
| --- | --- | --- |
| 项目根 | 入口与全局配置 | 放二进制、日志、数据库文件 |
| `models/` | 类型定义、表结构映射、基础 CRUD | 写 HTTP 响应 |
| `services/` | 业务规则、事务编排 | 直接解析 HTTP 请求 |
| `controllers/` | 请求校验、调用 service、组装响应 | 写复杂 SQL |
| `middleware/` | 鉴权、日志、限流 | 承载业务逻辑 |
| `support/` | 请求 / 响应 / 校验等无状态工具 | 承载业务规则 |
| `public/` | 前端静态文件 | 存放密钥 |
| `storage/` | 运行期产物 | 提交到版本库 |
| `uploads/` | 运行期上传文件 | 提交到版本库、放可执行代码 |
| `tests/` | 测试脚本 | 存放生产代码 |
| `migrations/` | 建表 / 改表 SQL | 修改历史迁移文件 |

## 4. 入口约定

- `rip.toml` 的 `[module] entry` 指定入口。
- 未配置时，`rip run` 依次查找：`<当前目录名>.rp`、`main.rp`、`src/main.rp`、`main.rpp`、`src/main.rpp`。
- 也可以直接把目录传给 CLI：`rip run demo/server` 会按 `rip.toml` 的 `entry` 启动 `demo/server/main.rp`。
- 入口文件必须包含 `func main() -> void`。
- HTTP 服务在 `main` 中启动，不要让 `main` 提前返回。
- 数据库连接、表结构初始化在 `http.serve` 之前完成。
- 路由可以直接用 `#[get("/path")]` 等注解声明在 `controllers/` 的函数上，`http.router` 会自动注册；也可以保留显式路由表，两者可以混用。注解处理器只接收 `http.request`，连接等依赖由应用自己持有。

## 5. `rip.toml`

```toml
[module]
name = "myapp"
version = "0.1.0"
entry = "main.rp"

[dependencies]

[rust-dependencies]
serde_json = "1.0"
```

| 字段 | 必填 | 说明 |
| --- | --- | --- |
| `module.name` | 是 | 包名，与项目目录名一致，`snake_case` |
| `module.version` | 是 | 语义化版本 |
| `module.entry` | 否 | 入口文件，默认按入口约定查找 |
| `dependencies` | 否 | Rip 包 |
| `rust-dependencies` | 否 | Rust 互操作包 |

## 6. 命名约定

| 对象 | 规范 | 示例 |
| --- | --- | --- |
| 目录 | `snake_case`，目录名即包名 | `models/`、`user_service/` |
| 源文件 | `snake_case.rp` | `user_controller.rp` |
| 模块声明 | 等于所在目录名，单段 `snake_case` | `mod user_service;` |
| 类型 | `snake_case` | `user`、`user_profile` |
| 函数 | `snake_case` | `get_user`、`register_user` |
| 变量 | `snake_case` | `user_id` |
| 常量 / 静态 | `SCREAMING_SNAKE_CASE` | `MAX_PAGE_SIZE` |
| 数据库表 | 复数 `snake_case` | `users`、`user_roles` |
| 数据库字段 | `snake_case` | `created_at` |
| 迁移文件 | `0001_动作.sql` | `0001_create_users.sql` |

## 7. 分层写法

### 7.1 模型层

`models/user.rp`：

```rp
mod models;

use std::db;

type user = [
    id: int,
    name: string,
    email: string,
    created_at: string
];

func find_by_id(read conn: db.conn, id: int) -> result<option<map<string, any>>, string> {
    return db.table(conn, "users")
        .field("id, name, email, created_at")
        .where("id", "=", id)
        .find();
}
```

### 7.2 服务层

`services/user_service.rp`：

```rp
mod services;
```

服务层接收已校验的字段，完成业务规则、事务和多次数据访问，返回 `result<T, string>` 或统一的结果结构，不生成 `http.response`。

### 7.3 控制器层

`controllers/user_controller.rp`：

```rp
mod controllers;
```

控制器只做：读取 `http.request`、校验入参、调用服务、组装 `http.response`，统一 JSON 结构：

```json
{"code": 0, "message": "ok", "data": {}}
```

## 8. 数据库规范

`std/db` 按 DSN 前缀选择驱动：

| 数据库 | DSN | 说明 |
| --- | --- | --- |
| SQLite | `storage/app.db`、`sqlite::memory:` | 文件型，适合开发与小规模部署 |
| MySQL / MariaDB | `mysql://user:pass@host:3306/app` | 生产常用 |
| PostgreSQL | `postgres://user:pass@host:5432/app` | 生产常用 |
| SQL Server | `mssql://sa:pass@host:1433/app` 或 ADO 连接串 | 企业内网常用 |

约定：

- 连接统一在入口或 `support/` 的连接工具中打开，避免每个请求重复打开。
- DSN 通过环境变量注入，不写死在源码里；SQLite 文件放在 `storage/`，不要放源码目录。
- SQL 统一写 `?` 占位符，驱动层自动改写为 `$1` / `@P1`，不要手写方言占位符。
- 建表用 `migrations/` 下的 SQL，按目标数据库分别维护方言版本（自增列、时间类型、字符串类型不同）。
- 查询优先使用链式 ORM；复杂聚合、联表报表使用原生 SQL。
- 所有外部输入必须作为参数传入，不拼接 SQL 字符串。
- 唯一约束冲突返回 `409`，校验失败返回 `400`，不存在返回 `404`，数据库错误返回 `500`。
- 密码只存哈希，不存明文。

## 9. 配置与环境

- 开发用 SQLite：`storage/app.db`；测试用 `sqlite::memory:` 或 `storage/app_test.db`。
- 生产用环境变量提供完整 DSN，例如 `RIP_DB=mysql://user:pass@10.0.0.5:3306/app`。
- 敏感配置不写入源码，通过 `sys.env_get("KEY")` 读取。
- 端口用 `--port` 覆盖，不在源码里硬编码生产端口。

```rp
func open_database() -> result<db.conn, string> {
    let dsn: string = sys.env_get("RIP_DB").unwrap_or("storage/app.db");
    return db.open(dsn);
}
```

切换数据库时只改 DSN，业务代码与链式 ORM 不需要改动；只有 DDL 和个别方言函数需要按方言处理。

## 10. 测试规范

- 测试文件放在 `tests/`，文件名以 `test_` 开头，统一 `mod tests;`。
- `rip test` 会递归执行带 `main` 的 `.rp` 文件。
- 数据库测试使用独立文件，执行前删除旧库或使用内存库。
- HTTP 集成测试：先启动服务，再用 `curl` / `Invoke-WebRequest` 断言状态码和 JSON 字段。
- 测试必须覆盖：创建成功、重复创建、参数非法、查询、更新、删除、删除后查询。

## 11. 构建与部署

```bash
rip build main.rp
```

- 产物是项目根下的 `dist/` 交付目录，不要提交进版本库。
- 交付只需拷贝 `dist/`：里面有可执行文件、自动生成的 `README.md`（应用名、版本号、启动方式）、静态资源、`migrations/` 与 `storage/`。
- 静态资源放 `public/`、`assets/`、`static/`，或用 `rip.toml` 的 `[build] assets` 声明额外目录。
- `-o <目录>` 可改输出目录，`-o` 指的是目录而不是文件名。
- 工作目录必须是项目根，否则 `http.static` 和 SQLite 相对路径会失效。
- Linux 用 systemd，Windows 用 NSSM 或 `sc.exe` 托管。
