# Rip Demo and Project Directory Structure Specification

> [中文](README.md) | **English**

A user management example with separate frontend and backend directories: login, then create, read, update, delete and search user information after logging in. The end of this document also specifies the directory structure conventions for building server-side / Web projects with Rip.

> `demo/` is a **verification example** written in the Rip language; it is not part of the language specification. Rip is a general-purpose language for everyone: examples follow the specification, and the specification does not follow the examples.
>
> Language rules are in the [Rip Language Development Specification](../doc/RipLang-level.html) and the [Rip Language Manual](../doc/RipLang-help.html).

---

# Part I. Example (demo/)

## Directory

```text
demo/
├── client/                     # Vue 3 frontend (no build step, runs directly in the browser)
│   ├── index.html              # login page + user management page
│   ├── app.js                  # API calls, token management, list / search / dialogs
│   ├── style.css
│   └── vue.global.prod.js      # Vue 3 production build (local file, no network needed)
├── rip.ps1                     # convenience command: locates rip.exe automatically
├── stdlib/                     # self-check example for system packages (encode / xml / crypto / net / ws)
│   └── main.rp
└── server/                     # Rip backend (MVC layering, multi-file project)
    ├── rip.toml                # name = "server"，entry = "main.rp"
    ├── main.rp                 # entry: func main() -> void
    ├── config.rp               # DSN, session lifetime
    ├── routes.rp               # explicit route table (static assets only)
    ├── models/                 # model layer: table schema and data access (chained ORM)
    │   ├── schema.rp           # create tables (per dialect), seed administrator
    │   ├── user.rp             # users table: find / count / page / insert / update / delete
    │   └── session.rp          # sessions table: login state
    ├── services/               # service layer: business rules, returns service_result
    │   ├── result.rp           # service_result and svc_ok / svc_created / svc_fail
    │   ├── auth_service.rp     # login / logout / current user
    │   └── user_service.rp     # list / detail / create / update / delete
    ├── controllers/            # controller layer: parse requests, call services, assemble responses
    │   ├── auth_controller.rp
    │   ├── user_controller.rp
    │   ├── file_controller.rp   # file upload
    │   └── home_controller.rp
    ├── middleware/             # authentication middleware
    │   └── auth.rp
    ├── tasks/                  # scheduled tasks (#[interval] annotation, dedicated thread)
    │   └── maintenance.rp
    ├── support/                # common utilities: request reading, response wrapping, password / token / validation
    │   ├── request.rp
    │   ├── response.rp
    │   ├── security.rp
    │   └── database.rp         # open a connection per request, hand it to the handler, then close it
    ├── migrations/
    │   └── 0001_create_users_and_sessions.sql
    ├── storage/                # app.db generated at runtime
    ├── uploads/                # upload directory (created automatically at startup)
    └── test.ps1                # API automation tests
```

Project mode: `demo/server` contains a `rip.toml`, and `rip run` recursively collects every `.rp` file and compiles them together; each directory becomes an independent module (`models`, `services`, `controllers`, `middleware`, `tasks`, `support`), and functions are called across files directly by name without imports.

## Starting

This needs the `rip` command; if you get an error saying the term `rip` is not recognized, then `rip.exe` is not on `PATH`. If the repository has no configured command, use [rip.ps1](rip.ps1) under `demo` (it locates `rip.exe` in `target/debug` or `target/release` automatically):

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

By default it only listens on `127.0.0.1`; the startup log prints the actual address:

```text
Rip HTTP server listening on http://127.0.0.1:8080
```

For LAN access or production deployment, specify the bind address explicitly (an IP or a locally resolvable domain name):

```powershell
rip run main.rp --host 0.0.0.0 --port 8080
```

Open <http://127.0.0.1:8080> in a browser; the default administrator is:

```text
admin@example.com / admin123
```

## Routing: Automatic Registration via Annotations

API routes are declared directly on controller functions; `http.router` registers them automatically when it is built, so `routes.rp` no longer repeats the APIs:

```rp
#[get("/api/users")]
func users_index_action(read req: http.request) -> http.response {
    return with_conn(req, users_index_handle);
}
```

- Annotations are a Rip language feature ([Rip Language Development Specification](../doc/RipLang-level.html) §2.6, §11.2.1), not comments; writing `/// #[get(...)]` does not register a route.
- The handler signature is fixed as `func(read req: http.request) -> http.response`; the framework injects no extra parameters.
- The database connection is opened per request by `with_conn` in `support/database.rp`, which calls the handler and then closes it; the handler body is written as `xxx_handle(read conn: db.conn, read req: http.request)`.
- Annotated routes are registered first, and the explicit routes in `routes.rp` (only `http.static("/", "../client")`) are registered afterwards, so the static wildcard does not shadow the APIs.
- Duplicate declarations of the same `(method, path)`, mismatched handler signatures, and unknown attribute names are all compile errors, and `rip check` fails immediately.

## Testing

```powershell
cd demo/server
powershell -ExecutionPolicy Bypass -File .\test.ps1
```

The tests use a separate `storage/tests/app_test.db`, which is recreated before every run; the demo's production database `storage/app.db` is **never deleted or modified**, and its data is always preserved. The script starts a temporary instance on port 18081 and covers: static assets, successful/failed login, unauthorized 401, create, detail, update, search by name/email/phone, filtering by status, duplicate email 409, validation failure 400, invalid token 401, 404 after deletion, and token invalidation after logout.

## Building and Packaging

`rip build` merges the server project (`main.rp` plus every `.rp` under `models/`, `services/`, `controllers/`, `middleware/`, `support/`, `tasks/`, etc.) into a single executable, and collects the files required for delivery into `dist/`. **For delivery you only need to hand the whole `dist/` directory to the customer**; the target machine does not need `rip` installed.

This project is a server with separated frontend and backend, so `client/` is only used for local verification and **is not included in the delivery package**:

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

The output directory and executable name can be configured in [rip.toml](file:///e:/bak/rip/demo/server/rip.toml#L6-L10), or overridden on the command line:

```powershell
rip build main.rp -o release --name user_server
```

Runtime self-messages (startup log, runtime errors, task logs, `std/log` tags) are in English by default; add `[runtime] log = "chinese"` to `rip.toml` to switch to Chinese, and `rip build` writes that setting into the artifact, so no `rip.toml` is needed after delivery. This demo explicitly keeps it as `english`.

Start it inside the output directory (the database path is resolved relative to the working directory, so it must be run from that directory):

```powershell
cd demo/server/dist
.\server.exe --port 8080
```

Neither `dist/` nor `build/` is collected into the project's compilation unit. Verified in practice: `dist` contains `server.exe`, `README.md`, `storage/app.db` and `migrations/`; after startup `/api/me` returns 401 and the existing administrator and user data are still there.

## Third-Party Packages

`packages/` at the project root (the name `package/` is also accepted) holds packages written by others; a `use` path is simply the path relative to the package directory (with `/` replaced by `::`):

```text
demo/server/
├── main.rp
├── rip.toml
└── packages/
    ├── utils/http.rp        # use utils::http;
    └── vendor/logger.rpa    # use vendor::logger as logger;
```

- A package may contain Rip source files (`.rp`) or precompiled packages (`.rpa`); both can be mixed, and at compile time they are merged with the project source into the same compilation unit.
- Use `rip pack packages/utils/http` to compile a source package into a `.rpa`; after that the `.rp` can be deleted and the package still works. `rip build` reports an error when `rip-version` is inconsistent.
- Package names can have multiple segments (`utils/http`); `pub` functions inside a package are called by their global function names, after importing the alias with `use utils::http;`.
- At delivery the packages are already embedded in the executable; `[build] layout = "typed"` additionally copies the packages to `dist/lib/packages/` for inspection or replacement after delivery. This demo uses the default `flat` layout.

## Concurrency and Clustering

- `http.serve` starts worker threads based on the number of CPU cores by default, and this can be overridden by `[server] workers` in [rip.toml](file:///e:/bak/rip/demo/server/rip.toml#L28-L39). Each thread has its own interpreter, and module-level `static` values are not shared across requests.
- keep-alive, read/write timeouts, request body and header limits, and connection queue limits are supported; a full queue returns `503`, and an oversized request body returns `413`.
- Built-in probes: `GET /healthz` (liveness) and `GET /readyz` (readiness, returns `503` while shutting down); setting `[server] metrics_path` to `"/metrics"` exposes Prometheus metrics, or you can mount the route yourself with `http.metrics()`.
- After Ctrl+C / SIGTERM it stops accepting new connections, finishes the already-queued requests, and then exits, which suits containers and K8s rolling releases.
- For multi-instance deployments, enable `[cluster]` in `rip.toml`: `redis` provides the foundation for sessions / caches / distributed locks, and `task_lock = true` makes `#[interval]` tasks elect a leader through a Redis lease so that every machine does not run them repeatedly. See `std/redis` for usage (`redis.lock`, `redis.rate_limit`).
- Single-instance deployments do not need Redis; just leave `[cluster]` commented out. Server concurrency, timeouts and probes are all independent of whether clustering is used.

## Scheduled Tasks and Background Tasks

Scheduled tasks are declared with an annotation, scheduled automatically when the service starts, and run on a dedicated thread:

```rp
#[interval("1m")]
func session_cleanup() -> void {
    // 每分钟清理过期会话
}
```

- `#[interval("...")]` supports `ms` / `s` / `m` / `h` / `d`, and the function must take no parameters.
- [tasks/maintenance.rp](file:///e:/bak/rip/demo/server/tasks/maintenance.rp) in this demo cleans up expired sessions every minute.
- For one-off background tasks use `std/task`: `task.spawn("function_name")` runs asynchronously and returns immediately, and `task.sleep(milliseconds)` puts the current thread to sleep.
- Every task has its own interpreter state and database connection, and does not share Rip variables with HTTP requests.
- When state must be shared across threads, use the named primitives in `std/sync` (`sync.mutex` / `sync.atomic` / `sync.channel`): the same name within a process refers to the same object, and remember to pair locks with `defer unlock()`.

## API

| Method | Path | Description |
| --- | --- | --- |
| POST | `/api/login` | `{account, password}` → `{token, expires_at, user}` |
| POST | `/api/logout` | Delete the session of the current token |
| GET | `/api/me` | Current logged-in user |
| GET | `/api/users` | `page`, `limit`, `keyword` (name/email/phone), `status` |
| POST | `/api/users` | Create a user |
| GET | `/api/users/:id` | User details |
| PUT | `/api/users/:id` | Update a user (fields may be submitted partially) |
| DELETE | `/api/users/:id` | Delete a user (the currently logged-in user cannot be deleted) |
| POST | `/api/upload` | Upload a file as `multipart/form-data`, returns the path on disk |

Unified response: `{"code":0,"message":"ok","data":...}`; on error it is `{"code":<HTTP status>,"message":"..."}`.

Authentication: apart from login, every `/api/*` endpoint requires `Authorization: Bearer <token>`.

## File Upload

At runtime a `multipart/form-data` request is parsed into `req.files` (`vec<http.file>`), and the controller writes files to disk with `http.save_file(directory, file)`:

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

- The upload directory is determined by `config_upload_dir()` in [config.rp](file:///e:/bak/rip/demo/server/config.rp): `RIP_UPLOAD_DIR` takes precedence, defaulting to `uploads`; a relative path is resolved against the working directory. The runtime has no built-in upload directory and does not restrict where files may be written.
- `main.rp` creates that directory at startup with `fs.make_dir`, so it is created automatically even if the host directory does not exist.
- `http.file` fields: `field`, `filename`, `content_type`, `size`, `data_base64`; `http.save_file` decodes the data and sanitizes the file name (removing path separators and keeping only letters, digits and `. - _`), then returns the path on disk.
- `rip build` only copies the paths declared in `[build] copy`; this demo declares `storage` and `uploads`, so starting inside `dist` after delivery keeps writing to the same directory and files survive restarts/upgrades. To switch to third-party storage such as OSS / COS, just upload with `http.fetch` in code; neither compilation nor packaging configuration needs to change.

```powershell
curl.exe -H "Authorization: Bearer <token>" -F "file=@logo.png" http://127.0.0.1:8080/api/upload
# {"code":0,"message":"uploaded","files":["uploads\\logo.png"],"failed":0}
```

To store files elsewhere, set an absolute path: `$env:RIP_UPLOAD_DIR = "D:\data\uploads"`.

## Password Hashing and Encryption (std/crypto)

- [support/security.rp](file:///e:/bak/rip/demo/server/support/security.rp) no longer implements hashing itself: `password_hash` / `password_verify` call `crypto.password_hash` / `crypto.password_verify` directly, with the stored format `ript$iterations$salt$digest` (PBKDF2-HMAC-SHA256, 210000 iterations + 16-byte random salt), so the same password produces a different result every time.
- Session tokens are generated with `crypto.sha256` and `crypto.random_hex`.
- When an old database is found to contain passwords without the `ripx$` prefix, the seed administrator password is automatically re-hashed during startup (skipped when it already has the `ript$` prefix) (log: `upgrade admin password to std/crypto hash`).
- Other commonly used functions: `md5`, `sha1`, `sha256`, `sha512`, `hmac_sha256`, `base64_encode` / `base64_decode`, `hex_encode` / `hex_decode`, `random_hex`.
- Symmetric encryption uses `crypto.aes_encrypt` / `crypto.aes_decrypt` (AES-256-GCM, the key being an arbitrary string); signing uses `crypto.rsa_generate` / `crypto.rsa_sign` / `crypto.rsa_verify` (RSA PKCS#1 v1.5 + SHA-256); RSA encryption/decryption uses `crypto.rsa_encrypt` / `crypto.rsa_decrypt`.
- For the full signatures see the [Rip Language Development Specification](../doc/RipLang-level.html) §11.6.

## HTTPS / TLS

No code changes to `http.serve` are needed; to switch to HTTPS, provide a PEM certificate chain and private key on the command line at startup:

```powershell
rip run main.rp --cert server.pem --key server.key --port 8443
# Rip HTTP server listening on https://127.0.0.1:8443
```

- `--cert` is a PEM certificate chain (the first entry is the site certificate), and `--key` is a PEM private key (PKCS#8 / PKCS#1).
- Both must be given together; if one is missing, startup reports `HTTPS requires both --cert and --key`.
- The certificate and private key are not included in the `rip build` package; at deployment time place them at a given path on the target machine and point to them with the arguments.
- `rip dev` also supports `--cert` / `--key`.

## Other System Packages

`demo/stdlib/main.rp` is a self-check example for the newer system packages; run it with:

```powershell
rip run demo/stdlib/main.rp
```

It covers:

| Package | Capabilities | Example |
| --- | --- | --- |
| `std/encode` | URL / HTML / CSV / query-string encoding and decoding | `encode.url_encode`, `encode.csv_encode`, `encode.query_parse` |
| `std/xml` | XML parsing, lookup, serialization | `xml.parse`, `xml.find_all`, `xml.pretty` |
| `std/crypto` | Digests, Base64/Hex, AES-256-GCM, RSA, password hashing | `crypto.aes_encrypt`, `crypto.rsa_sign` |
| `std/net` | TCP connect / listen, UDP send / receive | `net.tcp_listen`, `net.udp_bind` |
| `std/ws` | WebSocket server and client | `ws.serve`, `ws.connect`, `ws.send` |
| `std/http` | Outbound requests: third-party APIs, webhooks, object storage | `http.fetch`, `http.fetch_file` |

`std/net` and `std/ws` are synchronous blocking APIs: put the server listen loop on a dedicated thread with `task.spawn("function_name")`. For full signatures and rules see the [Rip Language Development Specification](../doc/RipLang-level.html) §11.7–§11.10 and the "Standard Library Components" chapter of the [Rip Language Manual](../doc/RipLang-help.html).

## Database

The default is SQLite at `storage/app.db`; tables are created and the administrator is inserted on first startup, and every later startup reuses the same file so the data is kept long-term (tables are created with `CREATE TABLE IF NOT EXISTS`, so the database is never wiped). To switch to another database, only the DSN changes:

```powershell
$env:RIP_DB = "mysql://root:123456@127.0.0.1:3306/rip_demo"
rip run main.rp --port 8080
```

SQLite (a path or `sqlite::memory:`), MySQL/MariaDB, PostgreSQL and SQL Server are supported. Table creation statements generate auto-increment columns and text types in the dialect matching `db.driver`; `migrations/0001_create_users_and_sessions.sql` is a structural record, and when switching databases rewrite it according to the dialect notes inside.

## Call Chain

`controllers` parse the request → `services` apply business rules → `models` go through the chained ORM → `support` handles request reading / response wrapping / validation. Controllers only parse parameters and assemble responses; business rules are not written in controllers.

---

# Part II. Project Directory Structure Specification

> Applies to: Rip v1.3 toolchain 0.1.0　Package manifest: `rip.toml`　Entry: `<project directory name>.rp` or `main.rp`
>
> This part defines the standard directory, naming and entry conventions for building server-side / Web projects with Rip. In project mode the toolchain recursively collects `.rp` files and compiles them together; a directory is a package, and the layered structure below takes effect directly.

## 1. Modules and Directories

A Rip module declaration is the directory name (Rust-style `mod`):

- The first line of every `.rp` file must be `mod directory_name;`.
- The `mod` name must equal the name of the directory containing the file, exactly, using a single-segment `snake_case`.
- All `.rp` files in the same directory must declare the same `mod` name.
- A subdirectory is an independent module, and its `mod` name likewise equals the subdirectory name.
- `mod` names do not accept dotted names; `module` is not a keyword.
- Mark helpers intended only for the current module with `self`: `self func helper(...)`; calls from other modules fail to compile.

```text
myapp/
├── myapp.rp           # mod myapp;
├── models/
│   └── user.rp        # mod models;
└── tests/
    └── test_user.rp   # mod tests;
```

A missing `mod` declaration, a `mod` name that differs from the directory name, or inconsistent `mod` names in the same directory are all rejected outright by `rip check` / `rip run` / `rip build`.

## 2. Standard Structure

```text
myapp/
├── rip.toml                  # package manifest: name, version, entry, dependencies
├── main.rp                   # program entry (mod myapp), must contain func main() -> void
├── config.rp                 # configuration loading (mod myapp)
├── routes.rp                 # explicit route table (optional, static asset fallback)
├── models/                   # data models (mod models)
│   └── user.rp
├── services/                 # business logic (mod services)
│   └── user_service.rp
├── controllers/              # HTTP controllers (mod controllers)
│   └── user_controller.rp
├── middleware/               # middleware (mod middleware)
│   └── auth.rp
├── support/                  # common utilities (mod support)
│   ├── request.rp
│   └── response.rp
├── public/                   # static assets, used by http.static
│   ├── index.html
│   └── assets/
├── storage/                  # runtime data: SQLite files (if used), logs
│   ├── app.db
│   └── logs/
├── uploads/                  # runtime uploaded files (not compiled, copied with the package when building)
├── tests/                    # test .rp files (mod tests), rip test collects them recursively
│   ├── test_user.rp
│   └── test_api.rp
├── migrations/               # SQL migration scripts, named by sequence number
│   ├── 0001_create_users.sql
│   └── 0002_add_user_status.sql
└── docs/                     # project documentation (optional)
```

## 3. Directory Responsibilities

| Directory | Responsibility | Forbidden |
| --- | --- | --- |
| Project root | Entry point and global configuration | Binaries, logs, database files |
| `models/` | Type definitions, table schema mapping, basic CRUD | Writing HTTP responses |
| `services/` | Business rules, transaction orchestration | Parsing HTTP requests directly |
| `controllers/` | Request validation, calling services, assembling responses | Writing complex SQL |
| `middleware/` | Authentication, logging, rate limiting | Carrying business logic |
| `support/` | Stateless utilities for requests / responses / validation | Carrying business rules |
| `public/` | Frontend static files | Storing secrets |
| `storage/` | Runtime artifacts | Committing to version control |
| `uploads/` | Runtime uploaded files | Committing to version control, storing executable code |
| `tests/` | Test scripts | Storing production code |
| `migrations/` | Table creation / alteration SQL | Modifying historical migration files |

## 4. Entry Conventions

- `[module] entry` in `rip.toml` specifies the entry.
- When it is not configured, `rip run` looks for, in order: `<current directory name>.rp`, `main.rp`, `src/main.rp`, `main.rpp`, `src/main.rpp`.
- You can also pass the directory to the CLI directly: `rip run demo/server` starts `demo/server/main.rp` according to `entry` in `rip.toml`.
- The entry file must contain `func main() -> void`.
- The HTTP service is started inside `main`; do not let `main` return early.
- Database connections and table schema initialization are done before `http.serve`.
- Routes can be declared directly with annotations such as `#[get("/path")]` on functions in `controllers/`, and `http.router` registers them automatically; an explicit route table may also be kept, and the two can be mixed. Annotated handlers receive only `http.request`; dependencies such as connections are held by the application itself.

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

| Field | Required | Description |
| --- | --- | --- |
| `module.name` | Yes | Package name, identical to the project directory name, `snake_case` |
| `module.version` | Yes | Semantic version |
| `module.entry` | No | Entry file, looked up by the entry conventions by default |
| `dependencies` | No | Rip packages |
| `rust-dependencies` | No | Rust interop packages |

## 6. Naming Conventions

| Object | Convention | Example |
| --- | --- | --- |
| Directory | `snake_case`, the directory name is the package name | `models/`, `user_service/` |
| Source file | `snake_case.rp` | `user_controller.rp` |
| Module declaration | Equal to the containing directory name, single-segment `snake_case` | `mod user_service;` |
| Type | `snake_case` | `user`, `user_profile` |
| Function | `snake_case` | `get_user`, `register_user` |
| Variable | `snake_case` | `user_id` |
| Constant / static | `SCREAMING_SNAKE_CASE` | `MAX_PAGE_SIZE` |
| Database table | Plural `snake_case` | `users`, `user_roles` |
| Database field | `snake_case` | `created_at` |
| Migration file | `0001_action.sql` | `0001_create_users.sql` |

## 7. Layered Code Style

### 7.1 Model Layer

`models/user.rp`:

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

### 7.2 Service Layer

`services/user_service.rp`:

```rp
mod services;
```

The service layer receives already-validated fields, completes business rules, transactions and multiple data accesses, and returns `result<T, string>` or a unified result structure; it does not produce `http.response`.

### 7.3 Controller Layer

`controllers/user_controller.rp`:

```rp
mod controllers;
```

Controllers only: read the `http.request`, validate the input parameters, call services, and assemble the `http.response` with a unified JSON structure:

```json
{"code": 0, "message": "ok", "data": {}}
```

## 8. Database Conventions

`std/db` selects the driver by DSN prefix:

| Database | DSN | Description |
| --- | --- | --- |
| SQLite | `storage/app.db`, `sqlite::memory:` | File-based, suitable for development and small-scale deployments |
| MySQL / MariaDB | `mysql://user:pass@host:3306/app` | Commonly used in production |
| PostgreSQL | `postgres://user:pass@host:5432/app` | Commonly used in production |
| SQL Server | `mssql://sa:pass@host:1433/app` or an ADO connection string | Commonly used in enterprise intranets |

Conventions:

- Connections are opened in one place, at the entry point or in a connection utility under `support/`, to avoid reopening them on every request.
- The DSN is injected through environment variables and never hard-coded in source; SQLite files go in `storage/`, not in the source directory.
- SQL always uses `?` placeholders, which the driver layer rewrites to `$1` / `@P1` automatically; do not hand-write dialect placeholders.
- Create tables with the SQL under `migrations/`, maintaining a dialect version per target database (auto-increment columns, time types and string types differ).
- Prefer the chained ORM for queries; use native SQL for complex aggregations and joined reports.
- All external input must be passed as parameters; do not concatenate SQL strings.
- A unique constraint conflict returns `409`, validation failure returns `400`, a missing record returns `404`, and a database error returns `500`.
- Passwords are stored only as hashes, never in plain text.

## 9. Configuration and Environment

- SQLite for development: `storage/app.db`; for testing use `sqlite::memory:` or `storage/app_test.db`.
- In production, provide the full DSN through environment variables, e.g. `RIP_DB=mysql://user:pass@10.0.0.5:3306/app`.
- Sensitive configuration is not written into source; read it with `sys.env_get("KEY")`.
- Override the port with `--port`; do not hard-code the production port in source.

```rp
func open_database() -> result<db.conn, string> {
    let dsn: string = sys.env_get("RIP_DB").unwrap_or("storage/app.db");
    return db.open(dsn);
}
```

When switching databases only the DSN changes; business code and the chained ORM need no changes, and only DDL and a few dialect functions need dialect handling.

## 10. Testing Conventions

- Test files go in `tests/`, their names start with `test_`, and they all use `mod tests;`.
- `rip test` runs `.rp` files containing `main` recursively.
- Database tests use a separate file; delete the old database before running, or use an in-memory database.
- HTTP integration tests: start the service first, then assert status codes and JSON fields with `curl` / `Invoke-WebRequest`.
- Tests must cover: successful creation, duplicate creation, invalid parameters, query, update, delete, and query after delete.

## 11. Building and Deployment

```bash
rip build main.rp
```

- The artifact is the `dist/` delivery directory at the project root; do not commit it to version control.
- Delivery only requires copying `dist/`: it contains the executable, the auto-generated `README.md` (application name, version, startup method), static assets, `migrations/` and `storage/`.
- Put static assets in `public/`, `assets/`, `static/`, or declare extra directories with `[build] assets` in `rip.toml`.
- `-o <directory>` changes the output directory; `-o` refers to a directory, not a file name.
- The working directory must be the project root, otherwise `http.static` and SQLite relative paths break.
- On Linux use systemd; on Windows host it with NSSM or `sc.exe`.
