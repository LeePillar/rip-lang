# Rip 标准库 API 速查

来源：`doc/RipLang-help.md` 与 `doc/RipLang-level.md`。签名以源码/文档为准，未列出的函数不要臆造。

## 模块总览

| 模块 | 用途 |
| --- | --- |
| `std/http` | Web 服务端、路由、中间件、出站请求 |
| `std/json` | JSON 编解码 |
| `std/db` | SQLite / MySQL / PostgreSQL / SQL Server |
| `std/redis` | Redis 客户端、分布式锁、集群限流 |
| `std/time` | 时间、日期、定时器、IANA 时区 |
| `std/fs` / `std/path` | 文件、目录、路径 |
| `std/process` / `std/sys` | 子进程、环境变量、系统信息 |
| `std/log` / `std/dev` | 分级日志 / 调试输出（release 剔除） |
| `std/crypto` | 哈希、编码、随机、AES、RSA、密码哈希 |
| `std/encode` | URL / HTML / CSV / 查询串编解码 |
| `std/xml` / `std/regex` | XML、正则 |
| `std/rand` / `std/uuid` / `std/compress` | 随机、UUID、gzip |
| `std/net` / `std/ws` | TCP/UDP、WebSocket |
| `std/task` / `std/sync` | 后台/定时任务、互斥锁/原子/通道 |
| `std/cache` / `std/queue` | 进程级 TTL+LRU 缓存、SQLite 持久化队列 |
| `std/session` / `std/jwt` / `std/validate` / `std/template` | Web 通用件 |
| `std/cli` | 命令行参数 |
| `std/llm` | 大模型统一调用 |
| `std/zip` / `std/mail` | ZIP 归档、SMTP 邮件 |
| `std/reflect` | 运行时类型信息、按名调用方法 |
| `rust/<crate>` | Rust crate 互操作，如 `use rust::serde_json as json;` |

## std/http

```rp
http.serve(port: int, handler: func(http.request) -> http.response) -> void
http.router(routes: vec<http.route>) -> func(http.request) -> http.response
http.get / post / put / delete / patch / options / head(path, handler) -> http.route
http.static(prefix: string, dir: string) -> http.route
http.group(prefix, middlewares, routes) -> http.route
http.chain(middlewares, handler) -> handler
http.cors(origin) / security_headers() / with_headers(map) / rate_limit(rate, burst)
http.openapi(handler, title, version) -> string
http.fetch(method, url, headers, body) -> result<http.client_response, string>
http.fetch_file(method, url, headers, path) -> result<http.client_response, string>
http.fetch_retry(method, url, headers, body, attempts) -> result<http.client_response, string>
http.download(url, headers, path) -> result<int, string>
http.save_file(dir: string, file: http.file) -> result<string, string>
http.metrics() -> string
```

`http.request` 字段：`method path params query headers body json form files cookies client_ip request_id ctx`。
`http.response` 字段：`status headers body json sse`（`body`/`json`/`sse` 三选一）。
路径 `:name` 为参数，`*` 为前缀匹配。注解路由先注册，显式 routes 后注册。

## std/db

```rp
db.open(dsn) -> result<db.conn, string>
conn.execute(sql, params) / execute_many(sql, rows) / query(sql, params) / query_one(sql, params)
conn.last_insert_id() / begin() / commit() / rollback() / close()
db.table(conn, "users").insert(values)
db.table(conn, "users").where("id", "=", 1).update(changes)
db.table(conn, "users").where("id", "=", 1).delete()
db.table(conn, "users").field("id, name").order("id", "desc").limit(10).select()
db.has_many(conn, 子表, 外键列, 父 id) / db.belongs_to(conn, 父表, id)
```

SQL 统一 `?` 占位符，运行时按方言改写。DSN：`storage/app.db`、`sqlite::memory:`、`mysql://...`、`postgres://...`、`mssql://...`。

## std/json / std/time / std/fs / std/path / std/process / std/sys

```rp
json.encode(any) -> string
json.decode(string) -> result<any, string>

time.now() -> time.datetime            // .timestamp_ms() .format(pattern) .to_string()
time.timestamp_ms() / timestamp_s() -> int
time.format(timestamp_ms, pattern) -> string
time.format_zone(timestamp_ms, pattern, zone) -> result<string, string>
time.parse_zone(text, pattern, zone) -> result<int, string>
time.offset(timestamp_ms, zone) -> result<int, string>
time.zone_names() -> vec<string> / local_zone() -> string
time.sleep(ms) -> void

fs.read_file / write_file / append_file / exists / make_dir / remove_dir
fs.remove_file / copy_file / rename / file_size / list_dir / list_dir_all / metadata
fs.temp_file(prefix) / temp_dir(prefix)

path.join(a, b) / normalize(p) / absolute(p) / file_name(p) / parent(p) / extension(p) / stem(p)

process.run(cmd, args) -> [code: int, stdout: string, stderr: string]
process.run_shell(cmd) -> [code: int, stdout: string, stderr: string]

sys.env_get(name) -> option<string>
sys.args() -> vec<string>
sys.exit(code) -> void
sys.platform() -> string
sys.heap_usage() / sys.gc() -> map<string, int>
```

## std/log / std/dev

```rp
log.debug / info / warn / error(text) -> void
log.set_level("debug" | "info" | "warn" | "error")
log.set_format("text" | "json")
log.set_file(path) -> result<bool, string>
log.set_rotate(max_bytes, keep)

dev.log(value) / dev.log_json(value) / dev.assert(cond) / dev.trace(msg)
dev.time_start(name) / dev.time_end(name) / dev.breakpoint()
```

## std/crypto / std/encode

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
```

## std/task / std/sync / std/cache / std/queue

```rp
task.spawn("函数名") -> bool          // 独立线程执行一次无参函数
task.sleep(ms) -> void

sync.mutex() / sync.atomic(init) / sync.channel(capacity)   // 详见手册 5.10

cache.set(key, value, ttl_ms) / get(key) -> option<string>
cache.delete(key) / clear() / len() / configure(max_entries) / stats()

queue.enqueue(dsn, queue, payload, delay_ms) -> result<int, string>
queue.reserve(dsn, queue, lease_ms) -> result<option<queue.job>, string>
queue.ack(dsn, id) / fail(dsn, id, message) / release_expired(dsn) / stats(dsn, queue)
```

队列至少一次投递，处理函数必须幂等；`cache` 只存 `string`，结构化数据先 `json.encode`。

## std/llm

```rp
type llm.message = [role: string, content: string];
type llm.options = [model: string, temperature: float, max_tokens: int, top_p: float, stream: bool];
type llm.response = [content: string, model: string, prompt_tokens: int, completion_tokens: int, finish_reason: string];

llm.new_client(config) -> llm.client
client.chat(msgs, opts) -> result<llm.response, string>
client.chat_stream(msgs, opts, on_chunk: func(string) -> void) -> result<llm.response, string>
client.chat_json(msgs, opts, target: type) -> result<any, string>
```

## Web 通用件

```rp
session.save(data, secret, ttl_secs) -> string
session.cookie(token, ttl_secs) / load(req, secret) -> map<string, any> / clear()
session.csrf_token(secret) / csrf_check(req, secret)

jwt.sign(payload, secret, ttl_secs) -> string
jwt.verify(token, secret) -> result<map<string, any>, string>

validate.required(value) -> option<string>
validate.int_range(value, min, max) / string_len(value, min, max) / email(value)

template.render(source, data) -> string
template.escape(text) / clear_cache()
template.register(name, f) -> void   // 模板里用 {{ f(参数…) }} 调用；仅限模块级具名函数
```

## std/net / std/ws / std/redis

```rp
net.tcp_connect(host, port, timeout_ms) -> result<net.conn, string>
net.tcp_send(conn, data) / tcp_recv(conn, max_bytes, timeout_ms) / tcp_recv_line(...) / tcp_close(conn)
net.tcp_listen(host, port) / tcp_accept(listener, timeout_ms) / udp_bind / udp_send / udp_recv

ws.serve(port, func(ws.conn) -> void) -> void
ws.connect(url, timeout_ms) -> result<ws.conn, string>
ws.send(conn, text) / recv(conn, timeout_ms) / ping(conn) / close(conn)

redis.connect(url) -> result<redis.conn, string>
redis.lock(conn, key, ttl_ms) -> result<option<string>, string>
redis.unlock(conn, key, token) / rate_limit(conn, key, limit, window_ms)
conn.get / set / setex / setnx / del / exists / expire / incr / decr
conn.hget / hset / hdel / lpush / rpush / lpop / rpop / brpop / llen / eval / close
```

## 其他

```rp
xml.parse(text) -> result<xml.node, string>
xml.stringify / pretty / name / text / attrs / children / find / find_all / escape / unescape

regex.is_match(pat, text) -> result<bool, string>
regex.find / find_all / captures / replace

rand.int(min, max) / rand.float() / rand.bool()
uuid.v4() / uuid.nil()
compress.gzip(text) / gunzip(base64)
zip.extract / create ...
mail.send(...)            // 详见手册 5.11
reflect.type_name(value) / reflect.invoke(...)
cli.args() / cli.command(...) / cli.flag(...) / cli.option(...) / cli.positionals()
```
