//! 标准库与 Rust 包内置函数。

use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::methods::{now_ms, make_response};
use crate::rt::{Flow, FlowResult, Runtime};
use crate::tls::HttpStream;
use crate::value::{
    format_value, json_to_value, value_eq, value_to_json, FuncKind, Function, MapData, Route,
    RouteTable, Value,
};
use std::rc::Rc;

use aes_gcm::aead::Aead as _;
use aes_gcm::{Aes256Gcm, KeyInit as _, Nonce};
use base64::Engine as _;
use chrono::Offset as _;
use chrono::TimeZone as _;
use hmac::Mac as _;
use rsa::pkcs1::{DecodeRsaPrivateKey, DecodeRsaPublicKey};
use rsa::pkcs8::{DecodePrivateKey, DecodePublicKey, EncodePrivateKey, EncodePublicKey, LineEnding};
use rsa::rand_core::OsRng;
use rsa::{Oaep, Pkcs1v15Sign, RsaPrivateKey, RsaPublicKey};
use sha2::Digest as _;
use unicode_normalization::UnicodeNormalization;
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

impl Runtime {
    pub fn call_module(
        &mut self,
        path: &str,
        name: &str,
        args: Vec<Value>,
        line: u32,
    ) -> FlowResult<Value> {
        // 内建调用前检查一次截止时间：超时后不再发起新的 IO / SQL。
        self.check_deadline(line)?;
        let _profile = crate::profile::enter_module(path, name);
        match path {
            "std/dev" => self.dev_module(name, args, line),
            "std/json" => self.json_module(name, args, line),
            "std/time" => self.time_module(name, args, line),
            "std/sys" => self.sys_module(name, args, line),
            "std/fs" => self.fs_module(name, args, line),
            "std/db" => self.db_module(name, args, line),
            "std/redis" => self.redis_module(name, args, line),
            "std/log" => self.log_module(name, args, line),
            "std/llm" => self.llm_module(name, args, line),
            "std/http" => self.http_module(name, args, line),
            "std/task" => self.task_module(name, args, line),
            "std/sync" => self.sync_module(name, args, line),
            "std/crypto" => self.crypto_module(name, args, line),
            "std/net" => self.net_module(name, args, line),
            "std/ws" => self.ws_module(name, args, line),
            "std/xml" => self.xml_module(name, args, line),
            "std/encode" => self.encode_module(name, args, line),
            "std/path" => self.path_module(name, args, line),
            "std/process" => self.process_module(name, args, line),
            "std/regex" => self.regex_module(name, args, line),
            "std/rand" => self.rand_module(name, args, line),
            "std/uuid" => self.uuid_module(name, args, line),
            "std/compress" => self.compress_module(name, args, line),
            "std/zip" => self.zip_module(name, args, line),
            "std/mail" => self.mail_module(name, args, line),
            "std/session" => self.session_module(name, args, line),
            "std/validate" => self.validate_module(name, args, line),
            "std/template" => self.template_module(name, args, line),
            "std/jwt" => self.jwt_module(name, args, line),
            "std/str" => self.str_module(name, args, line),
            "std/reflect" => self.reflect_module(name, args, line),
            "std/vec" => self.vec_module(name, args, line),
            "std/map" => self.map_module(name, args, line),
            "std/cli" => self.cli_module(name, args, line),
            "std/cache" => self.cache_module(name, args, line),
            "std/queue" => self.queue_module(name, args, line),
            "rust/serde_json" => self.serde_json_module(name, args, line),
            // 非系统包（`packages/` 下的第三方包）在编译期与项目源码合并，
            // 包内函数以全局函数名存在，按别名调用时回落到全局查找。
            _ => {
                #[cfg(feature = "interp")]
                if let Some(decl) = self.funcs.get(name).cloned() {
                    if decl.is_async {
                        return self.call_async(&decl.name, args, line);
                    }
                    return self.call_decl(&decl, args, line);
                }
                // 外部原生包（Rust 插件）：签名来自 `.rp.decl`，实现来自动态库。
                if let Some(result) = self.call_native_package(path, name, args, line) {
                    return result;
                }
                self.error(
                    format!("package \"{}\" is not available in this build", path),
                    line,
                )
            }
        }
    }

    fn dev_module(
        &mut self,
        name: &str,
        args: Vec<Value>,
        line: u32,
    ) -> FlowResult<Value> {
        match name {
            "log" | "trace" => {
                let text: Vec<String> = args.iter().map(format_value).collect();
                crate::debug::emit_line(format!("[{}] {}", name, text.join(" ")));
                Ok(Value::Null)
            }
            "log_json" => {
                for arg in &args {
                    let json = value_to_json(arg);
                    crate::debug::emit_line(
                        serde_json::to_string_pretty(&json)
                            .unwrap_or_else(|_| "null".to_string()),
                    );
                }
                Ok(Value::Null)
            }
            // 显式断点：调试会话中在下一条语句前暂停；非调试运行保持无副作用。
            "breakpoint" => {
                if let Some(debugger) = &self.debugger {
                    debugger.request_pause();
                }
                Ok(Value::Null)
            }
            "assert" => {
                let ok = matches!(args.first(), Some(Value::Bool(true)));
                if ok {
                    Ok(Value::Null)
                } else {
                    self.error("assertion failed", line)
                }
            }
            "time_start" => {
                let label = self.string_arg(&args, 0, name, line)?;
                self.timers.insert(label, Instant::now());
                Ok(Value::Null)
            }
            "time_end" => {
                let label = self.string_arg(&args, 0, name, line)?;
                if let Some(start) = self.timers.remove(&label) {
                    let elapsed = start.elapsed().as_secs_f64() * 1000.0;
                    self.say(
                        format!("[time] {} = {:.2}ms", label, elapsed),
                        format!("[耗时] {} = {:.2}ms", label, elapsed),
                    );
                }
                Ok(Value::Null)
            }
            _ => self.error(format!("std/dev has no function '{}'", name), line),
        }
    }

    fn json_module(
        &mut self,
        name: &str,
        args: Vec<Value>,
        line: u32,
    ) -> FlowResult<Value> {
        match name {
            "encode" => {
                let value = args.first().cloned().unwrap_or(Value::Null);
                // 快速路径：直接序列化，跳过 serde_json::Value 中间树。
                Ok(Value::string(crate::value::value_to_json_string(&value)))
            }
            "decode" => {
                let text = self.string_arg(&args, 0, name, line)?;
                match serde_json::from_str(&text) {
                    Ok(value) => Ok(Value::Ok(Box::new(json_to_value(&value)))),
                    Err(error) => Ok(Value::Err(Box::new(Value::string(error.to_string())))),
                }
            }
            _ => self.error(format!("std/json has no function '{}'", name), line),
        }
    }

    fn time_module(
        &mut self,
        name: &str,
        args: Vec<Value>,
        line: u32,
    ) -> FlowResult<Value> {
        match name {
            "now" => {
                let timestamp = now_ms();
                Ok(Value::ary(
                    "time.datetime",
                    vec![("timestamp_ms".to_string(), Value::Int(timestamp))],
                ))
            }
            "timestamp_ms" => Ok(Value::Int(now_ms())),
            "timestamp_s" => Ok(Value::Int(now_ms() / 1000)),
            // time.format(毫秒时间戳, 格式)：格式为 strftime 风格，如 "%Y-%m-%d %H:%M:%S"。
            "format" => {
                let ms = self.int_arg(&args, 0, name, line)?;
                let pattern = self.string_arg(&args, 1, name, line)?;
                match chrono::Local.timestamp_millis_opt(ms).single() {
                    Some(time) => Ok(Value::string(time.format(&pattern).to_string())),
                    None => self.error(format!("invalid timestamp {}", ms), line),
                }
            }
            // time.parse(文本, 格式)：按 strftime 格式解析为毫秒时间戳。
            "parse" => {
                let text = self.string_arg(&args, 0, name, line)?;
                let pattern = self.string_arg(&args, 1, name, line)?;
                match chrono::NaiveDateTime::parse_from_str(&text, &pattern) {
                    Ok(naive) => match naive.and_local_timezone(chrono::Local).single() {
                        Some(time) => Ok(Value::Ok(Box::new(Value::Int(time.timestamp_millis())))),
                        None => Ok(Value::Err(Box::new(Value::string(
                            "timestamp is ambiguous in local time".to_string(),
                        )))),
                    },
                    Err(error) => Ok(Value::Err(Box::new(Value::string(error.to_string())))),
                }
            }
            // time.format_utc(毫秒时间戳, 格式)：按 UTC 格式化，签名、日志时间戳常用。
            "format_utc" => {
                let ms = self.int_arg(&args, 0, name, line)?;
                let pattern = self.string_arg(&args, 1, name, line)?;
                match chrono::Utc.timestamp_millis_opt(ms).single() {
                    Some(time) => Ok(Value::string(time.format(&pattern).to_string())),
                    None => self.error(format!("invalid timestamp {}", ms), line),
                }
            }
            "sleep" => {
                let ms = self.int_arg(&args, 0, name, line)?;
                if ms > 0 {
                    std::thread::sleep(Duration::from_millis(ms as u64));
                }
                Ok(Value::Null)
            }
            // time.zone_names()：IANA 时区名清单（含夏令时规则）。
            "zone_names" => Ok(Value::vec(
                chrono_tz::TZ_VARIANTS
                    .iter()
                    .map(|zone| Value::string(zone.name()))
                    .collect(),
            )),
            // time.local_zone()：系统时区名，未配置时返回空串。
            "local_zone" => Ok(Value::string(local_zone_name())),
            // time.offset(毫秒时间戳, 时区)：该时刻相对 UTC 的偏移秒数（夏令时已计入）。
            "offset" => {
                let ms = self.int_arg(&args, 0, name, line)?;
                let zone = self.string_arg(&args, 1, name, line)?;
                let result = parse_timezone(&zone).and_then(|tz| {
                    let time = chrono::DateTime::<chrono::Utc>::from_timestamp_millis(ms)
                        .ok_or_else(|| format!("invalid timestamp {}", ms))?;
                    Ok(time
                        .with_timezone(&tz)
                        .offset()
                        .fix()
                        .local_minus_utc() as i64)
                });
                Ok(int_result(result))
            }
            // time.format_zone(毫秒时间戳, 格式, 时区)：按时区格式化。
            "format_zone" => {
                let ms = self.int_arg(&args, 0, name, line)?;
                let pattern = self.string_arg(&args, 1, name, line)?;
                let zone = self.string_arg(&args, 2, name, line)?;
                let result = parse_timezone(&zone).and_then(|tz| {
                    let time = chrono::DateTime::<chrono::Utc>::from_timestamp_millis(ms)
                        .ok_or_else(|| format!("invalid timestamp {}", ms))?;
                    Ok(time.with_timezone(&tz).format(&pattern).to_string())
                });
                Ok(text_result(result))
            }
            // time.parse_zone(文本, 格式, 时区)：按时区把本地时间解析为毫秒时间戳。
            "parse_zone" => {
                let text = self.string_arg(&args, 0, name, line)?;
                let pattern = self.string_arg(&args, 1, name, line)?;
                let zone = self.string_arg(&args, 2, name, line)?;
                let result = parse_timezone(&zone).and_then(|tz| {
                    let naive = parse_naive(&text, &pattern)?;
                    match tz.from_local_datetime(&naive) {
                        chrono::LocalResult::Single(time) => Ok(time.timestamp_millis()),
                        // 夏令时回拨会出现两个同一本地时刻：取较早的一个。
                        chrono::LocalResult::Ambiguous(earlier, _) => Ok(earlier.timestamp_millis()),
                        chrono::LocalResult::None => Err(format!(
                            "'{}' does not exist in {} (daylight saving gap)",
                            text, zone
                        )),
                    }
                });
                Ok(int_result(result))
            }
            _ => self.error(format!("std/time has no function '{}'", name), line),
        }
    }

    fn sys_module(
        &mut self,
        name: &str,
        args: Vec<Value>,
        line: u32,
    ) -> FlowResult<Value> {
        match name {
            "env_get" => {
                let key = self.string_arg(&args, 0, name, line)?;
                Ok(match std::env::var(&key) {
                    Ok(value) => Value::Some(Box::new(Value::string(value))),
                    Err(_) => Value::None,
                })
            }
            "args" => Ok(Value::vec(
                self.cli_args.iter().cloned().map(Value::string).collect(),
            )),
            "exit" => {
                let code = args
                    .first()
                    .and_then(|v| v.as_int())
                    .unwrap_or(0);
                std::process::exit(code as i32);
            }
            "platform" => Ok(Value::string(if cfg!(windows) {
                "windows"
            } else if cfg!(target_os = "macos") {
                "macos"
            } else {
                "linux"
            })),
            // 堆统计：调用即开启计数（之后分配的内存才计入）。
            "heap_usage" => {
                crate::heap::enable();
                Ok(Value::map(vec![
                    (
                        Value::string("used_bytes"),
                        Value::Int(crate::heap::usage() as i64),
                    ),
                    (
                        Value::string("peak_bytes"),
                        Value::Int(crate::heap::peak() as i64),
                    ),
                    (
                        Value::string("limit_bytes"),
                        Value::Int(self.heap_limit as i64),
                    ),
                    (
                        Value::string("tracked_nodes"),
                        Value::Int(crate::gc::tracked_nodes() as i64),
                    ),
                ]))
            }
            // 立即做一次循环回收，返回扫描与回收数量。
            "gc" => {
                let stats = crate::gc::collect();
                Ok(Value::map(vec![
                    (Value::string("scanned"), Value::Int(stats.scanned as i64)),
                    (
                        Value::string("collected"),
                        Value::Int(stats.collected as i64),
                    ),
                    (
                        Value::string("tracked_nodes"),
                        Value::Int(crate::gc::tracked_nodes() as i64),
                    ),
                ]))
            }
            _ => self.error(format!("std/sys has no function '{}'", name), line),
        }
    }

    /// 会话 cookie 是否追加 `Secure`：本机 TLS（`--cert`/`--key`）开启时直接追加；
    /// 否则在 `[server] trust_proxy = true` 时按请求的 `X-Forwarded-Proto` 最右侧值
    /// （由最贴近源站的代理写入，客户端伪造不了）判定，覆盖"反代 HTTPS + 源站 HTTP"部署。
    fn cookie_secure(&self, request: Option<&Value>) -> bool {
        if self.tls_cert.is_some() && self.tls_key.is_some() {
            return true;
        }
        let trust = self
            .server_config
            .as_ref()
            .is_some_and(|config| config.trust_proxy);
        if !trust {
            return false;
        }
        request_header_ci(request, "x-forwarded-proto")
            .and_then(|value| {
                value
                    .split(',')
                    .next_back()
                    .map(|item| item.trim().to_ascii_lowercase())
            })
            .is_some_and(|proto| proto == "https")
    }

    /// `std/session`：签名 cookie 会话与 CSRF 令牌。
    fn session_module(
        &mut self,
        name: &str,
        args: Vec<Value>,
        line: u32,
    ) -> FlowResult<Value> {
        match name {
            "load" => {
                let request = match args.first() {
                    Some(value @ Value::Ary(_)) => value.clone(),
                    _ => return self.error("session.load expects an http.request", line),
                };
                let secret = self.string_arg(&args, 1, name, line)?;
                let token = request_field(&request, "cookies")
                    .and_then(|cookies| match cookies {
                        Value::Map(map) => {
                            map.borrow().get(&Value::string(SESSION_COOKIE)).and_then(|value| value.as_str())
                        }
                        _ => None,
                    })
                    .unwrap_or_default();
                let payload = verify_session(&token, &secret)
                    .unwrap_or_else(|| serde_json::json!({}));
                Ok(json_to_value(&payload))
            }
            "save" => {
                let data = args.first().cloned().unwrap_or(Value::Null);
                let secret = self.string_arg(&args, 1, name, line)?;
                let ttl = self.int_arg(&args, 2, name, line)?;
                Ok(Value::string(sign_session(
                    &value_to_json(&data),
                    &secret,
                    ttl,
                )))
            }
            "cookie" => {
                let value = self.string_arg(&args, 0, name, line)?;
                let ttl = self.int_arg(&args, 1, name, line)?;
                let secure = self.cookie_secure(args.get(2));
                Ok(Value::string(session_cookie_header(&value, ttl, secure)))
            }
            "clear" => Ok(Value::string(format!(
                "{}=; Path=/; HttpOnly; SameSite=Lax; Max-Age=0{}",
                SESSION_COOKIE,
                if self.cookie_secure(args.get(0)) {
                    "; Secure"
                } else {
                    ""
                }
            ))),
            "csrf_token" => {
                let secret = self.string_arg(&args, 0, name, line)?;
                Ok(Value::string(csrf_token(&secret)))
            }
            "csrf_check" => {
                let request = match args.first() {
                    Some(value @ Value::Ary(_)) => value.clone(),
                    _ => return self.error("session.csrf_check expects an http.request", line),
                };
                let secret = self.string_arg(&args, 1, name, line)?;
                let token = csrf_token_from_request(&request).unwrap_or_default();
                Ok(Value::Bool(
                    !token.is_empty() && csrf_verify(&token, &secret),
                ))
            }
            _ => self.error(format!("std/session has no function '{}'", name), line),
        }
    }

    /// `std/validate`：把一个字段校验成期望类型，失败返回可读错误。
    fn validate_module(
        &mut self,
        name: &str,
        args: Vec<Value>,
        line: u32,
    ) -> FlowResult<Value> {
        match name {
            "required" => {
                let missing = match args.first() {
                    None | Some(Value::Null) | Some(Value::None) => true,
                    Some(Value::Str(text)) => text.is_empty(),
                    _ => false,
                };
                Ok(if missing {
                    Value::Some(Box::new(Value::string("is required")))
                } else {
                    Value::None
                })
            }
            "int_range" => {
                let min = self.int_arg(&args, 1, name, line)?;
                let max = self.int_arg(&args, 2, name, line)?;
                let value = args.first().and_then(|value| value.as_int());
                Ok(match value {
                    Some(value) if value >= min && value <= max => Value::Ok(Box::new(Value::Int(value))),
                    Some(value) => Value::Err(Box::new(Value::string(format!(
                        "must be between {} and {} (found {})",
                        min, max, value
                    )))),
                    None => Value::Err(Box::new(Value::string("must be an int"))),
                })
            }
            "string_len" => {
                let min = self.int_arg(&args, 1, name, line)?;
                let max = self.int_arg(&args, 2, name, line)?;
                let value = args.first().and_then(|value| value.as_str());
                Ok(match value {
                    Some(value)
                        if value.chars().count() as i64 >= min
                            && value.chars().count() as i64 <= max =>
                    {
                        Value::Ok(Box::new(Value::string(value)))
                    }
                    Some(value) => Value::Err(Box::new(Value::string(format!(
                        "length must be between {} and {} (found {})",
                        min,
                        max,
                        value.chars().count()
                    )))),
                    None => Value::Err(Box::new(Value::string("must be a string"))),
                })
            }
            "email" => {
                let value = args.first().and_then(|value| value.as_str());
                Ok(match value {
                    Some(value) if is_email(&value) => Value::Ok(Box::new(Value::string(value))),
                    _ => Value::Err(Box::new(Value::string("must be an email address"))),
                })
            }
            _ => self.error(format!("std/validate has no function '{}'", name), line),
        }
    }

    /// `std/template`：服务端 HTML 渲染，默认自动转义。
    fn template_module(
        &mut self,
        name: &str,
        args: Vec<Value>,
        line: u32,
    ) -> FlowResult<Value> {
        match name {
            "render" => {
                let source = self.string_arg(&args, 0, name, line)?;
                let data = args.get(1).cloned().unwrap_or(Value::Null);
                match crate::template::render(&source, &data) {
                    Ok(text) => Ok(Value::string(text)),
                    Err(message) => self.error(message, line),
                }
            }
            "escape" => {
                let text = self.string_arg(&args, 0, name, line)?;
                Ok(Value::string(crate::template::escape(&text)))
            }
            "clear_cache" => {
                crate::template::clear_cache();
                Ok(Value::Null)
            }
            _ => self.error(format!("std/template has no function '{}'", name), line),
        }
    }

    /// `std/jwt`：HS256 签发与校验。
    fn jwt_module(
        &mut self,
        name: &str,
        args: Vec<Value>,
        line: u32,
    ) -> FlowResult<Value> {
        match name {
            "sign" => {
                let payload = args.first().cloned().unwrap_or(Value::Null);
                let secret = self.string_arg(&args, 1, name, line)?;
                let ttl = self.int_arg(&args, 2, name, line)?;
                match crate::jwt::sign(&value_to_json(&payload), &secret, ttl) {
                    Ok(token) => Ok(Value::string(token)),
                    Err(message) => self.error(message, line),
                }
            }
            "verify" => {
                let token = self.string_arg(&args, 0, name, line)?;
                let secret = self.string_arg(&args, 1, name, line)?;
                let aud = args.get(2).and_then(|value| value.as_str());
                let iss = args.get(3).and_then(|value| value.as_str());
                Ok(match crate::jwt::verify_claims(&token, &secret, aud.as_deref(), iss.as_deref()) {
                    Ok(claims) => Value::Ok(Box::new(json_to_value(&claims))),
                    Err(message) => Value::Err(Box::new(Value::string(message))),
                })
            }
            _ => self.error(format!("std/jwt has no function '{}'", name), line),
        }
    }

    /// `std/str`：字符串处理的函数式接口（与内建方法等价，便于管道式书写）。
    fn str_module(
        &mut self,
        name: &str,
        args: Vec<Value>,
        line: u32,
    ) -> FlowResult<Value> {
        let text = |args: &Vec<Value>| -> String {
            args.first().and_then(|value| value.as_str()).unwrap_or_default()
        };
        match name {
            "split" => {
                let value = self.string_arg(&args, 0, name, line)?;
                let sep = self.string_arg(&args, 1, name, line)?;
                let items: Vec<Value> = if sep.is_empty() {
                    value.chars().map(|ch| Value::string(ch.to_string())).collect()
                } else {
                    value.split(sep.as_str()).map(Value::string).collect()
                };
                Ok(Value::vec(items))
            }
            "join" => {
                let items = match args.first() {
                    Some(Value::Vec(items)) => items.borrow().clone(),
                    _ => return self.error("str.join expects a vec<string>", line),
                };
                let sep = self.string_arg(&args, 1, name, line)?;
                let joined = items
                    .iter()
                    .map(|item| item.as_str().unwrap_or_default())
                    .collect::<Vec<_>>()
                    .join(&sep);
                Ok(Value::string(joined))
            }
            "trim" => Ok(Value::string(text(&args).trim().to_string())),
            "trim_start" => Ok(Value::string(text(&args).trim_start().to_string())),
            "trim_end" => Ok(Value::string(text(&args).trim_end().to_string())),
            "contains" => {
                let needle = self.string_arg(&args, 1, name, line)?;
                Ok(Value::Bool(text(&args).contains(&needle)))
            }
            "starts_with" => {
                let prefix = self.string_arg(&args, 1, name, line)?;
                Ok(Value::Bool(text(&args).starts_with(&prefix)))
            }
            "ends_with" => {
                let suffix = self.string_arg(&args, 1, name, line)?;
                Ok(Value::Bool(text(&args).ends_with(&suffix)))
            }
            "replace" => {
                let pattern = self.string_arg(&args, 1, name, line)?;
                let with = self.string_arg(&args, 2, name, line)?;
                let value = text(&args);
                if pattern.is_empty() {
                    return Ok(Value::string(value));
                }
                Ok(Value::string(value.replace(&pattern, &with)))
            }
            "lower" => Ok(Value::string(text(&args).to_lowercase())),
            "upper" => Ok(Value::string(text(&args).to_uppercase())),
            // 按 Unicode 字符切片，越界自动收敛，不会 panic。
            "slice" => {
                let value = text(&args);
                let start = self.int_arg(&args, 1, name, line)?.max(0) as usize;
                let end = args
                    .get(2)
                    .and_then(|value| value.as_int())
                    .map(|value| value.max(0) as usize)
                    .unwrap_or(usize::MAX);
                let sliced: String = value
                    .chars()
                    .skip(start)
                    .take(end.saturating_sub(start))
                    .collect();
                Ok(Value::string(sliced))
            }
            "repeat" => {
                let value = text(&args);
                let times = self.int_arg(&args, 1, name, line)?.clamp(0, 1_000_000) as usize;
                // 上限保护：避免一次调用把进程内存打满。
                if value.len().saturating_mul(times) > 32 * 1024 * 1024 {
                    return self.error("str.repeat result is too large", line);
                }
                Ok(Value::string(value.repeat(times)))
            }
            "pad_start" | "pad_end" => {
                let value = text(&args);
                let width = self.int_arg(&args, 1, name, line)?.max(0) as usize;
                let pad = args
                    .get(2)
                    .and_then(|value| value.as_str())
                    .unwrap_or_else(|| " ".to_string());
                Ok(Value::string(pad_to(&value, width, &pad, name == "pad_start")))
            }
            "index_of" => {
                let needle = self.string_arg(&args, 1, name, line)?;
                let value = text(&args);
                let index = value
                    .find(&needle)
                    .map(|byte| value[..byte].chars().count() as i64)
                    .unwrap_or(-1);
                Ok(Value::Int(index))
            }
            "len" => Ok(Value::Int(text(&args).chars().count() as i64)),
            "is_empty" => Ok(Value::Bool(text(&args).is_empty())),
            "lines" => Ok(Value::vec(
                text(&args)
                    .lines()
                    .map(|line| Value::string(line.to_string()))
                    .collect(),
            )),
            "split_once" => {
                let sep = self.string_arg(&args, 1, name, line)?;
                Ok(match text(&args).split_once(sep.as_str()) {
                    Some((left, right)) => Value::Some(Box::new(Value::vec(vec![
                        Value::string(left.to_string()),
                        Value::string(right.to_string()),
                    ]))),
                    None => Value::None,
                })
            }
            // Unicode 归一化：nfc / nfd / nfkc / nfkd。
            "normalize" => {
                let value = text(&args);
                let form = self
                    .string_arg(&args, 1, name, line)?
                    .trim()
                    .to_ascii_lowercase();
                let normalized = match form.as_str() {
                    "nfc" => value.nfc().collect::<String>(),
                    "nfd" => value.nfd().collect::<String>(),
                    "nfkc" => value.nfkc().collect::<String>(),
                    "nfkd" => value.nfkd().collect::<String>(),
                    _ => {
                        return Ok(Value::Err(Box::new(Value::string(format!(
                            "unknown normalization form '{}' (use nfc/nfd/nfkc/nfkd)",
                            form
                        )))))
                    }
                };
                Ok(Value::Ok(Box::new(Value::string(normalized))))
            }
            // 字素簇：用户可见的一个字符（emoji、组合音标不会被拆开）。
            "graphemes" => Ok(Value::vec(
                text(&args)
                    .graphemes(true)
                    .map(Value::string)
                    .collect(),
            )),
            "grapheme_len" => Ok(Value::Int(text(&args).graphemes(true).count() as i64)),
            // 终端显示宽度：东亚全角字符记 2，组合字符记 0。
            "width" => Ok(Value::Int(UnicodeWidthStr::width(text(&args).as_str()) as i64)),
            _ => self.error(format!("std/str has no function '{}'", name), line),
        }
    }

    /// `std/vec`：不修改原 vec 的函数式辅助（修改请用内建方法）。
    fn vec_module(
        &mut self,
        name: &str,
        args: Vec<Value>,
        line: u32,
    ) -> FlowResult<Value> {
        let items = |args: &Vec<Value>| -> Vec<Value> {
            match args.first() {
                Some(Value::Vec(items)) => items.borrow().clone(),
                _ => Vec::new(),
            }
        };
        match name {
            "len" => Ok(Value::Int(match args.first() {
                Some(Value::Vec(items)) => items.borrow().len() as i64,
                Some(Value::Ary(ary)) => ary.borrow().fields.len() as i64,
                _ => 0,
            })),
            "is_empty" => Ok(Value::Bool(items(&args).is_empty())),
            "contains" => {
                let needle = args.get(1).cloned().unwrap_or(Value::Null);
                Ok(Value::Bool(
                    items(&args).iter().any(|item| value_eq(item, &needle)),
                ))
            }
            "index_of" => {
                let needle = args.get(1).cloned().unwrap_or(Value::Null);
                let position = items(&args)
                    .iter()
                    .position(|item| value_eq(item, &needle))
                    .map(|index| index as i64)
                    .unwrap_or(-1);
                Ok(Value::Int(position))
            }
            "first" | "last" => {
                let items = items(&args);
                let value = if name == "first" {
                    items.first().cloned()
                } else {
                    items.last().cloned()
                };
                Ok(match value {
                    Some(value) => Value::Some(Box::new(value)),
                    None => Value::None,
                })
            }
            "sum" => {
                let mut total: i64 = 0;
                for item in items(&args) {
                    total = total.saturating_add(item.as_int().unwrap_or(0));
                }
                Ok(Value::Int(total))
            }
            "range" => {
                let end = self.int_arg(&args, 0, name, line)?;
                Ok(Value::vec(
                    (0..end.max(0)).map(Value::Int).collect(),
                ))
            }
            "range_between" => {
                let start = self.int_arg(&args, 0, name, line)?;
                let end = self.int_arg(&args, 1, name, line)?;
                Ok(Value::vec(
                    (start..end).take(10_000_000).map(Value::Int).collect(),
                ))
            }
            _ => self.error(format!("std/vec has no function '{}'", name), line),
        }
    }

    /// `std/map`：只读查询与不可变合并（写入请用内建方法 `set` / `delete`）。
    fn map_module(
        &mut self,
        name: &str,
        args: Vec<Value>,
        line: u32,
    ) -> FlowResult<Value> {
        let map = |args: &Vec<Value>| -> Option<Rc<std::cell::RefCell<MapData>>> {
            match args.first() {
                Some(Value::Map(map)) => Some(Rc::clone(map)),
                _ => None,
            }
        };
        match name {
            "keys" => {
                let Some(map) = map(&args) else {
                    return self.error("map.keys expects a map", line);
                };
                let keys: Vec<Value> = map
                    .borrow()
                    .entries
                    .iter()
                    .map(|(key, _)| {
                        Value::string(key.as_str().unwrap_or_else(|| format_value(key)))
                    })
                    .collect();
                Ok(Value::vec(keys))
            }
            "values" => {
                let Some(map) = map(&args) else {
                    return self.error("map.values expects a map", line);
                };
                let values: Vec<Value> = map
                    .borrow()
                    .entries
                    .iter()
                    .map(|(_, value)| value.clone())
                    .collect();
                Ok(Value::vec(values))
            }
            "len" => {
                let Some(map) = map(&args) else {
                    return self.error("map.len expects a map", line);
                };
                let len = map.borrow().entries.len() as i64;
                Ok(Value::Int(len))
            }
            "has" => {
                let Some(map) = map(&args) else {
                    return self.error("map.has expects a map", line);
                };
                let key = args.get(1).cloned().unwrap_or(Value::Null);
                let found = map.borrow().has(&key);
                Ok(Value::Bool(found))
            }
            "get" => {
                let Some(map) = map(&args) else {
                    return self.error("map.get expects a map", line);
                };
                let key = args.get(1).cloned().unwrap_or(Value::Null);
                let value = map.borrow().get(&key);
                Ok(match value {
                    Some(value) => Value::Some(Box::new(value)),
                    None => Value::None,
                })
            }
            "merge" => {
                let Some(base) = map(&args) else {
                    return self.error("map.merge expects a map", line);
                };
                let extra = match args.get(1) {
                    Some(Value::Map(map)) => map.borrow().clone(),
                    _ => return self.error("map.merge expects a second map", line),
                };
                let mut merged = base.borrow().clone();
                for (key, value) in extra.entries {
                    merged.set(key, value);
                }
                Ok(Value::map_data(merged))
            }
            "without" => {
                let Some(base) = map(&args) else {
                    return self.error("map.without expects a map", line);
                };
                let key = args.get(1).cloned().unwrap_or(Value::Null);
                let mut copy = base.borrow().clone();
                copy.delete(&key);
                Ok(Value::map_data(copy))
            }
            _ => self.error(format!("std/map has no function '{}'", name), line),
        }
    }

    /// `std/cli`：命令行参数解析（`--flag`、`--name value`、`--name=value`）。
    fn cli_module(
        &mut self,
        name: &str,
        args: Vec<Value>,
        line: u32,
    ) -> FlowResult<Value> {
        match name {
            "args" => Ok(Value::vec(
                self.cli_args.iter().cloned().map(Value::string).collect(),
            )),
            "command" => Ok(Value::string(
                self.cli_args
                    .iter()
                    .find(|arg| !arg.starts_with('-'))
                    .cloned()
                    .unwrap_or_default(),
            )),
            "flag" => {
                let flag = self.string_arg(&args, 0, name, line)?;
                let flag = flag.trim_start_matches('-').to_string();
                let with_value = format!("{}=", flag);
                Ok(Value::Bool(self.cli_args.iter().any(|arg| {
                    let arg = arg.trim_start_matches('-');
                    arg == flag || arg.starts_with(&with_value)
                })))
            }
            "option" => {
                let key = self.string_arg(&args, 0, name, line)?;
                let default = args
                    .get(1)
                    .and_then(|value| value.as_str())
                    .unwrap_or_default();
                Ok(Value::string(cli_option(&self.cli_args, &key).unwrap_or(default)))
            }
            // 位置参数：跳过选项本身；`--name value` 形式会一并跳过取值。
            // 判定规则：下一个参数不以 `-` 开头就视为取值（无法区分布尔开关时按常规取舍）。
            "positionals" => {
                let mut out = Vec::new();
                let mut index = 0;
                while index < self.cli_args.len() {
                    let arg = &self.cli_args[index];
                    if arg.starts_with('-') && arg.len() > 1 {
                        index += 1;
                        if !arg.contains('=')
                            && self
                                .cli_args
                                .get(index)
                                .is_some_and(|value| !value.starts_with('-'))
                        {
                            index += 1;
                        }
                        continue;
                    }
                    out.push(Value::string(arg.clone()));
                    index += 1;
                }
                Ok(Value::vec(out))
            }
            _ => self.error(format!("std/cli has no function '{}'", name), line),
        }
    }

    /// `std/cache`：进程级 TTL + LRU 缓存，值为字符串（建议存 JSON）。
    fn cache_module(
        &mut self,
        name: &str,
        args: Vec<Value>,
        line: u32,
    ) -> FlowResult<Value> {
        match name {
            "set" => {
                let key = self.string_arg(&args, 0, name, line)?;
                let value = self.string_arg(&args, 1, name, line)?;
                let ttl = args.get(2).and_then(|value| value.as_int()).unwrap_or(0);
                crate::cache::set(&key, value, ttl);
                Ok(Value::Null)
            }
            "get" => {
                let key = self.string_arg(&args, 0, name, line)?;
                Ok(match crate::cache::get(&key) {
                    Some(value) => Value::Some(Box::new(Value::string(value))),
                    None => Value::None,
                })
            }
            "delete" => {
                let key = self.string_arg(&args, 0, name, line)?;
                Ok(Value::Bool(crate::cache::delete(&key)))
            }
            "clear" => {
                crate::cache::clear();
                Ok(Value::Null)
            }
            "len" => Ok(Value::Int(crate::cache::len() as i64)),
            "configure" => {
                let max_entries = self.int_arg(&args, 0, name, line)?;
                Ok(Value::Int(
                    crate::cache::configure(max_entries.max(1) as usize) as i64,
                ))
            }
            "stats" => {
                let (hits, misses, evictions, entries) = crate::cache::snapshot();
                let mut map = MapData::default();
                map.set(Value::string("hits"), Value::Int(hits as i64));
                map.set(Value::string("misses"), Value::Int(misses as i64));
                map.set(Value::string("evictions"), Value::Int(evictions as i64));
                map.set(Value::string("entries"), Value::Int(entries as i64));
                Ok(Value::map_data(map))
            }
            _ => self.error(format!("std/cache has no function '{}'", name), line),
        }
    }

    /// `std/queue`：SQLite 持久化任务队列（至少一次投递）。
    fn queue_module(
        &mut self,
        name: &str,
        args: Vec<Value>,
        line: u32,
    ) -> FlowResult<Value> {
        match name {
            "enqueue" => {
                let dsn = self.string_arg(&args, 0, name, line)?;
                let queue = self.string_arg(&args, 1, name, line)?;
                let payload = self.string_arg(&args, 2, name, line)?;
                let delay = args.get(3).and_then(|value| value.as_int()).unwrap_or(0);
                Ok(match crate::queue::enqueue(&dsn, &queue, &payload, delay) {
                    Ok(id) => Value::Ok(Box::new(Value::Int(id))),
                    Err(message) => Value::Err(Box::new(Value::string(message))),
                })
            }
            "reserve" => {
                let dsn = self.string_arg(&args, 0, name, line)?;
                let queue = self.string_arg(&args, 1, name, line)?;
                let lease = args
                    .get(2)
                    .and_then(|value| value.as_int())
                    .unwrap_or(30_000);
                match crate::queue::reserve(&dsn, &queue, lease) {
                    Ok(Some((id, payload, attempts))) => {
                        let job = Value::ary(
                            "queue.job",
                            vec![
                                ("id".to_string(), Value::Int(id)),
                                ("queue".to_string(), Value::string(queue)),
                                ("payload".to_string(), Value::string(payload)),
                                ("attempts".to_string(), Value::Int(attempts)),
                            ],
                        );
                        Ok(Value::Ok(Box::new(Value::Some(Box::new(job)))))
                    }
                    Ok(None) => Ok(Value::Ok(Box::new(Value::None))),
                    Err(message) => Ok(Value::Err(Box::new(Value::string(message)))),
                }
            }
            "ack" => {
                let dsn = self.string_arg(&args, 0, name, line)?;
                let id = self.int_arg(&args, 1, name, line)?;
                Ok(match crate::queue::ack(&dsn, id) {
                    Ok(done) => Value::Ok(Box::new(Value::Bool(done))),
                    Err(message) => Value::Err(Box::new(Value::string(message))),
                })
            }
            "fail" => {
                let dsn = self.string_arg(&args, 0, name, line)?;
                let id = self.int_arg(&args, 1, name, line)?;
                let message_text = args
                    .get(2)
                    .and_then(|value| value.as_str())
                    .unwrap_or_default();
                Ok(match crate::queue::fail(&dsn, id, &message_text) {
                    Ok(state) => Value::Ok(Box::new(Value::string(state))),
                    Err(message) => Value::Err(Box::new(Value::string(message))),
                })
            }
            "release_expired" => {
                let dsn = self.string_arg(&args, 0, name, line)?;
                Ok(match crate::queue::release_expired(&dsn) {
                    Ok(count) => Value::Ok(Box::new(Value::Int(count))),
                    Err(message) => Value::Err(Box::new(Value::string(message))),
                })
            }
            "stats" => {
                let dsn = self.string_arg(&args, 0, name, line)?;
                let queue = self.string_arg(&args, 1, name, line)?;
                match crate::queue::stats(&dsn, &queue) {
                    Ok((pending, running, failed)) => {
                        let mut map = MapData::default();
                        map.set(Value::string("pending"), Value::Int(pending));
                        map.set(Value::string("running"), Value::Int(running));
                        map.set(Value::string("failed"), Value::Int(failed));
                        Ok(Value::Ok(Box::new(Value::map_data(map))))
                    }
                    Err(message) => Ok(Value::Err(Box::new(Value::string(message)))),
                }
            }
            _ => self.error(format!("std/queue has no function '{}'", name), line),
        }
    }

    fn fs_module(
        &mut self,
        name: &str,
        args: Vec<Value>,
        line: u32,
    ) -> FlowResult<Value> {
        // RIP_FS_DENY：运维路径拒绝开关，分发前统一检查路径参数。
        for &index in crate::ext::fs_path_arg_indexes(name) {
            if let Some(text) = args.get(index).and_then(Value::as_str) {
                if let Some(message) = crate::ext::fs_deny_error(&text) {
                    return self.error(message, line);
                }
            }
        }
        match name {
            "read_file" => {
                let path = self.string_arg(&args, 0, name, line)?;
                Ok(match fs::read_to_string(&path) {
                    Ok(content) => Value::Ok(Box::new(Value::string(content))),
                    Err(error) => Value::Err(Box::new(Value::string(error.to_string()))),
                })
            }
            "write_file" => {
                let path = self.string_arg(&args, 0, name, line)?;
                let content = self.string_arg(&args, 1, name, line)?;
                Ok(match fs::write(&path, content.as_bytes()) {
                    Ok(()) => Value::Ok(Box::new(Value::Int(content.len() as i64))),
                    Err(error) => Value::Err(Box::new(Value::string(error.to_string()))),
                })
            }
            "exists" => {
                let path = self.string_arg(&args, 0, name, line)?;
                Ok(Value::Bool(std::path::Path::new(&path).exists()))
            }
            "make_dir" => {
                let path = self.string_arg(&args, 0, name, line)?;
                Ok(match fs::create_dir_all(&path) {
                    Ok(()) => Value::Ok(Box::new(Value::Bool(true))),
                    Err(error) => Value::Err(Box::new(Value::string(error.to_string()))),
                })
            }
            "remove_file" => {
                let path = self.string_arg(&args, 0, name, line)?;
                Ok(match fs::remove_file(&path) {
                    Ok(()) => Value::Ok(Box::new(Value::Bool(true))),
                    Err(error) => Value::Err(Box::new(Value::string(error.to_string()))),
                })
            }
            "copy_file" => {
                let from = self.string_arg(&args, 0, name, line)?;
                let to = self.string_arg(&args, 1, name, line)?;
                Ok(match fs::copy(&from, &to) {
                    Ok(bytes) => Value::Ok(Box::new(Value::Int(bytes as i64))),
                    Err(error) => Value::Err(Box::new(Value::string(error.to_string()))),
                })
            }
            "file_size" => {
                let path = self.string_arg(&args, 0, name, line)?;
                Ok(match fs::metadata(&path) {
                    Ok(meta) => Value::Ok(Box::new(Value::Int(meta.len() as i64))),
                    Err(error) => Value::Err(Box::new(Value::string(error.to_string()))),
                })
            }
            "list_dir" => {
                let path = self.string_arg(&args, 0, name, line)?;
                match fs::read_dir(&path) {
                    Ok(entries) => {
                        let mut names = Vec::new();
                        for entry in entries.flatten() {
                            names.push(Value::string(
                                entry.file_name().to_string_lossy().to_string(),
                            ));
                        }
                        Ok(Value::Ok(Box::new(Value::vec(names))))
                    }
                    Err(error) => {
                        Ok(Value::Err(Box::new(Value::string(error.to_string()))))
                    }
                }
            }
            _ => self.fs_ext(name, args, line),
        }
    }

    fn log_module(
        &mut self,
        name: &str,
        args: Vec<Value>,
        line: u32,
    ) -> FlowResult<Value> {
        match name {
            "debug" | "info" | "warn" | "error" => {
                let rank = match name {
                    "debug" => 0,
                    "info" => 1,
                    "warn" => 2,
                    _ => 3,
                };
                let configured = match self.log_level.to_ascii_lowercase().as_str() {
                    "debug" => 0,
                    "info" => 1,
                    "warn" => 2,
                    "error" => 3,
                    _ => 1,
                };
                if rank < configured {
                    return Ok(Value::Null);
                }
                let message = args
                    .first()
                    .map(format_value)
                    .unwrap_or_default();
                let label = if self.language.is_chinese() {
                    match name {
                        "debug" => "调试",
                        "info" => "信息",
                        "warn" => "警告",
                        _ => "错误",
                    }
                } else {
                    match name {
                        "debug" => "DEBUG",
                        "info" => "INFO",
                        "warn" => "WARN",
                        _ => "ERROR",
                    }
                };
                // 结构化日志：时间、级别、消息；text 为人类可读，json 便于采集。
                let line = if self.log_format == "json" {
                    let mut record = serde_json::json!({
                        "time": chrono::Local::now().format("%Y-%m-%dT%H:%M:%S%.3f%:z").to_string(),
                        "level": name.to_ascii_uppercase(),
                        "message": message,
                    });
                    if let Some(request_id) = &self.current_request_id {
                        record["request_id"] = serde_json::Value::String(request_id.clone());
                    }
                    // trace_id 让日志与 OTLP 追踪对上，不用再按时间人工对齐。
                    if let Some(trace_id) = &self.current_trace_id {
                        record["trace_id"] = serde_json::Value::String(trace_id.clone());
                    }
                    serde_json::to_string(&record).unwrap_or_default()
                } else {
                    format!(
                        "{} [{}] {}",
                        chrono::Local::now().format("%Y-%m-%dT%H:%M:%S%.3f"),
                        label,
                        message
                    )
                };
                crate::debug::emit_line(line.clone());
                if let Some(file) = &mut self.log_file {
                    let _ = writeln!(file, "{}", line);
                    let _ = file.flush();
                }
                self.rotate_log_if_needed();
                Ok(Value::Null)
            }
            "set_level" => {
                if let Some(level) = args.first().and_then(|v| v.as_str()) {
                    self.log_level = level.to_string();
                }
                Ok(Value::Null)
            }
            "set_format" => {
                if let Some(format) = args.first().and_then(|v| v.as_str()) {
                    self.log_format = format.to_ascii_lowercase();
                }
                Ok(Value::Null)
            }
            "set_file" => {
                let path = self.string_arg(&args, 0, name, line)?;
                match std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&path)
                {
                    Ok(file) => {
                        self.log_file = Some(file);
                        self.log_path = Some(path);
                        Ok(Value::Ok(Box::new(Value::Bool(true))))
                    }
                    Err(error) => Ok(Value::Err(Box::new(Value::string(format!(
                        "cannot open log file '{}': {}",
                        path, error
                    ))))),
                }
            }
            // `log.set_rotate(max_bytes, keep)`：单文件超过上限后切成 `.1`、`.2` …
            // `max_bytes <= 0` 关闭轮转；`keep` 为保留的历史文件数（至少 1）。
            "set_rotate" => {
                let max_bytes = self.int_arg(&args, 0, name, line)?.max(0) as u64;
                let keep = args
                    .get(1)
                    .and_then(|value| value.as_int())
                    .unwrap_or(3)
                    .clamp(1, 64) as usize;
                self.log_max_bytes = max_bytes;
                self.log_backups = keep;
                Ok(Value::Null)
            }
            _ => self.error(format!("std/log has no function '{}'", name), line),
        }
    }

    /// 日志文件超过上限时轮转：`app.log` → `app.log.1` → `app.log.2` …
    fn rotate_log_if_needed(&mut self) {
        if self.log_max_bytes == 0 {
            return;
        }
        let Some(path) = self.log_path.clone() else {
            return;
        };
        let size = self
            .log_file
            .as_ref()
            .and_then(|file| file.metadata().ok())
            .map(|meta| meta.len())
            .unwrap_or(0);
        if size < self.log_max_bytes {
            return;
        }
        // 先释放句柄，Windows 上被占用的文件无法重命名。
        self.log_file = None;
        let backups = self.log_backups.max(1);
        let _ = std::fs::remove_file(format!("{}.{}", path, backups));
        for index in (1..backups).rev() {
            let _ = std::fs::rename(
                format!("{}.{}", path, index),
                format!("{}.{}", path, index + 1),
            );
        }
        let _ = std::fs::rename(&path, format!("{}.1", path));
        self.log_file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .ok();
    }

    fn llm_module(
        &mut self,
        name: &str,
        args: Vec<Value>,
        line: u32,
    ) -> FlowResult<Value> {
        match name {
            "new_client" => {
                let config = match args.first() {
                    Some(Value::Ary(ary)) => ary.clone(),
                    _ => return self.error("new_client expects llm.config", line),
                };
                let fields = config.borrow().fields.clone();
                Ok(Value::ary("llm.client", fields))
            }
            _ => self.error(format!("std/llm has no function '{}'", name), line),
        }
    }

    fn serde_json_module(
        &mut self,
        name: &str,
        args: Vec<Value>,
        line: u32,
    ) -> FlowResult<Value> {
        match name {
            "to_string" | "to_string_pretty" => {
                let value = args.first().cloned().unwrap_or(Value::Null);
                let json = value_to_json(&value);
                let result = if name == "to_string_pretty" {
                    serde_json::to_string_pretty(&json)
                } else {
                    serde_json::to_string(&json)
                };
                Ok(match result {
                    Ok(text) => Value::Ok(Box::new(Value::string(text))),
                    Err(error) => {
                        Value::Err(Box::new(Value::string(error.to_string())))
                    }
                })
            }
            "from_str" => {
                let text = self.string_arg(&args, 0, name, line)?;
                Ok(match serde_json::from_str(&text) {
                    Ok(value) => Value::Ok(Box::new(json_to_value(&value))),
                    Err(error) => Value::Err(Box::new(Value::string(error.to_string()))),
                })
            }
            _ => self.error(
                format!("rust/serde_json has no function '{}'", name),
                line,
            ),
        }
    }

    /// `std/crypto`：哈希、编码、随机数与密码哈希。
    fn crypto_module(
        &mut self,
        name: &str,
        args: Vec<Value>,
        line: u32,
    ) -> FlowResult<Value> {
        match name {
            "md5" => {
                let text = self.string_arg(&args, 0, name, line)?;
                Ok(Value::string(hex_lower(&{
                    let digest = md5::Md5::digest(text.as_bytes());
                    digest.to_vec()
                })))
            }
            "sha256" => {
                let text = self.string_arg(&args, 0, name, line)?;
                Ok(Value::string(hex_lower(&{
                    let digest = sha2::Sha256::digest(text.as_bytes());
                    digest.to_vec()
                })))
            }
            "sha1" => {
                let text = self.string_arg(&args, 0, name, line)?;
                Ok(Value::string(hex_lower(&{
                    let digest = sha1::Sha1::digest(text.as_bytes());
                    digest.to_vec()
                })))
            }
            "sha512" => {
                let text = self.string_arg(&args, 0, name, line)?;
                Ok(Value::string(hex_lower(&{
                    let digest = sha2::Sha512::digest(text.as_bytes());
                    digest.to_vec()
                })))
            }
            "hmac_sha256" => {
                let key = self.string_arg(&args, 0, name, line)?;
                let text = self.string_arg(&args, 1, name, line)?;
                let mut mac = match <hmac::Hmac<sha2::Sha256> as hmac::Mac>::new_from_slice(
                    key.as_bytes(),
                ) {
                    Ok(mac) => mac,
                    Err(_) => return self.error("invalid hmac key", line),
                };
                mac.update(text.as_bytes());
                Ok(Value::string(hex_lower(&mac.finalize().into_bytes().to_vec())))
            }
            // crypto.hmac_sha256_hex(十六进制密钥, 文本)：密钥按原始字节处理，结果十六进制。
            // 用于派生链式密钥（如 AWS SigV4），中间密钥不是合法 UTF-8，不能当字符串传。
            "hmac_sha256_hex" => {
                let key_hex = self.string_arg(&args, 0, name, line)?;
                let text = self.string_arg(&args, 1, name, line)?;
                let key = match hex_decode(&key_hex) {
                    Some(bytes) => bytes,
                    None => {
                        return Ok(Value::Err(Box::new(Value::string(
                            "invalid hex key".to_string(),
                        ))))
                    }
                };
                let mut mac = match <hmac::Hmac<sha2::Sha256> as hmac::Mac>::new_from_slice(&key) {
                    Ok(mac) => mac,
                    Err(_) => return self.error("invalid hmac key", line),
                };
                mac.update(text.as_bytes());
                Ok(Value::Ok(Box::new(Value::string(hex_lower(
                    &mac.finalize().into_bytes().to_vec(),
                )))))
            }
            // crypto.sha256_file(路径)：对文件原始字节取 SHA-256，大文件不必读进内存字符串。
            "sha256_file" => {
                let path = self.string_arg(&args, 0, name, line)?;
                match std::fs::read(&path) {
                    Ok(bytes) => Ok(Value::Ok(Box::new(Value::string(hex_lower(
                        &sha2::Sha256::digest(&bytes).to_vec(),
                    ))))),
                    Err(error) => Ok(Value::Err(Box::new(Value::string(format!(
                        "cannot read '{}': {}",
                        path, error
                    ))))),
                }
            }
            "base64_encode" => {
                let text = self.string_arg(&args, 0, name, line)?;
                Ok(Value::string(base64_encode(text.as_bytes())))
            }
            "base64_decode" => {
                let text = self.string_arg(&args, 0, name, line)?;
                match base64_decode(&text) {
                    Some(bytes) => Ok(Value::Ok(Box::new(Value::string(
                        String::from_utf8_lossy(&bytes).to_string(),
                    )))),
                    None => Ok(Value::Err(Box::new(Value::string(
                        "invalid base64 input".to_string(),
                    )))),
                }
            }
            "hex_encode" => {
                let text = self.string_arg(&args, 0, name, line)?;
                Ok(Value::string(hex_lower(text.as_bytes())))
            }
            "hex_decode" => {
                let text = self.string_arg(&args, 0, name, line)?;
                match hex_decode(&text) {
                    Some(bytes) => Ok(Value::Ok(Box::new(Value::string(
                        String::from_utf8_lossy(&bytes).to_string(),
                    )))),
                    None => Ok(Value::Err(Box::new(Value::string(
                        "invalid hex input".to_string(),
                    )))),
                }
            }
            "random_hex" => {
                let bytes = self.int_arg(&args, 0, name, line)?;
                if bytes <= 0 || bytes > 1024 {
                    return Ok(Value::Err(Box::new(Value::string(
                        "random_hex expects 1..1024 bytes".to_string(),
                    ))));
                }
                let mut buffer = vec![0u8; bytes as usize];
                if getrandom::getrandom(&mut buffer).is_err() {
                    return Ok(Value::Err(Box::new(Value::string(
                        "cannot read system random source".to_string(),
                    ))));
                }
                Ok(Value::Ok(Box::new(Value::string(hex_lower(&buffer)))))
            }
            // PHP 风格密码哈希：加盐 + 多轮 SHA-256，格式 ripx$轮数$盐$摘要。
            "password_hash" => {
                let password = self.string_arg(&args, 0, name, line)?;
                match password_hash(&password) {
                    Ok(hashed) => Ok(Value::Ok(Box::new(Value::string(hashed)))),
                    Err(message) => Ok(Value::Err(Box::new(Value::string(message)))),
                }
            }
            "password_verify" => {
                let password = self.string_arg(&args, 0, name, line)?;
                let hashed = self.string_arg(&args, 1, name, line)?;
                Ok(Value::Bool(password_verify(&password, &hashed)))
            }
            // AES-256-GCM：密钥经 PBKDF2+随机盐派生（V2 格式），输出 base64(RIPV2: || 盐 || 随机 nonce || 密文+标签)。
            "aes_encrypt" => {
                let text = self.string_arg(&args, 0, name, line)?;
                let key = self.string_arg(&args, 1, name, line)?;
                let mut salt = [0u8; 16];
                let mut nonce = [0u8; 12];
                if getrandom::getrandom(&mut salt).is_err()
                    || getrandom::getrandom(&mut nonce).is_err()
                {
                    return Ok(Value::Err(Box::new(Value::string(
                        "cannot read system random source".to_string(),
                    ))));
                }
                let cipher = match Aes256Gcm::new_from_slice(&derive_key_v2(&key, &salt)) {
                    Ok(cipher) => cipher,
                    Err(_) => {
                        return self.error("cannot initialize AES-256-GCM", line);
                    }
                };
                match cipher.encrypt(Nonce::from_slice(&nonce), text.as_bytes()) {
                    Ok(mut payload) => {
                        let mut output = AES_V2_PREFIX.to_vec();
                        output.extend_from_slice(&salt);
                        output.extend_from_slice(&nonce);
                        output.append(&mut payload);
                        Ok(Value::Ok(Box::new(Value::string(base64_encode(&output)))))
                    }
                    Err(_) => Ok(Value::Err(Box::new(Value::string(
                        "AES-GCM encryption failed".to_string(),
                    )))),
                }
            }
            "aes_decrypt" => {
                let encoded = self.string_arg(&args, 0, name, line)?;
                let key = self.string_arg(&args, 1, name, line)?;
                let bytes = match base64_decode(&encoded) {
                    Some(bytes) => bytes,
                    None => {
                        return Ok(Value::Err(Box::new(Value::string(
                            "invalid base64 input".to_string(),
                        ))))
                    }
                };
                // V2 格式：前缀 + 盐(16) + nonce(12)；否则按旧格式（nonce || 密文，SHA-256 派生）解密。
                let (cipher_key, nonce, payload) = if bytes.starts_with(AES_V2_PREFIX) {
                    let rest = &bytes[AES_V2_PREFIX.len()..];
                    if rest.len() < 16 + 12 + 16 {
                        return Ok(Value::Err(Box::new(Value::string(
                            "AES-GCM payload is too short".to_string(),
                        ))));
                    }
                    let (salt, rest) = rest.split_at(16);
                    let (nonce, payload) = rest.split_at(12);
                    (derive_key_v2(&key, salt), nonce, payload)
                } else {
                    if bytes.len() < 12 + 16 {
                        return Ok(Value::Err(Box::new(Value::string(
                            "AES-GCM payload is too short".to_string(),
                        ))));
                    }
                    let (nonce, payload) = bytes.split_at(12);
                    (derive_key(&key), nonce, payload)
                };
                let cipher = match Aes256Gcm::new_from_slice(&cipher_key) {
                    Ok(cipher) => cipher,
                    Err(_) => {
                        return self.error("cannot initialize AES-256-GCM", line);
                    }
                };
                match cipher.decrypt(Nonce::from_slice(nonce), payload) {
                    Ok(plain) => Ok(Value::Ok(Box::new(Value::string(
                        String::from_utf8_lossy(&plain).to_string(),
                    )))),
                    Err(_) => Ok(Value::Err(Box::new(Value::string(
                        "AES-GCM decryption failed".to_string(),
                    )))),
                }
            }
            // RSA：PKCS#8 / PKCS#1 PEM；签名 PKCS#1 v1.5 + SHA-256。
            "rsa_generate" => {
                let bits = self.int_arg(&args, 0, name, line)?;
                if !(1024..=8192).contains(&bits) || bits % 8 != 0 {
                    return self.error("rsa_generate bits must be 1024..=8192 and a multiple of 8", line);
                }
                let private = match RsaPrivateKey::new(&mut OsRng, bits as usize) {
                    Ok(private) => private,
                    Err(error) => {
                        return Ok(Value::Err(Box::new(Value::string(error.to_string()))))
                    }
                };
                let private_pem = match private.to_pkcs8_pem(LineEnding::LF) {
                    Ok(pem) => pem.to_string(),
                    Err(error) => {
                        return Ok(Value::Err(Box::new(Value::string(error.to_string()))))
                    }
                };
                let public_pem = match private.to_public_key().to_public_key_pem(LineEnding::LF) {
                    Ok(pem) => pem,
                    Err(error) => {
                        return Ok(Value::Err(Box::new(Value::string(error.to_string()))))
                    }
                };
                Ok(Value::Ok(Box::new(Value::map(vec![
                    (Value::string("private_key_pem"), Value::string(private_pem)),
                    (Value::string("public_key_pem"), Value::string(public_pem)),
                ]))))
            }
            "rsa_sign" => {
                let text = self.string_arg(&args, 0, name, line)?;
                let pem = self.string_arg(&args, 1, name, line)?;
                let private = match parse_private_key(&pem) {
                    Ok(private) => private,
                    Err(message) => return Ok(Value::Err(Box::new(Value::string(message)))),
                };
                let digest = sha2::Sha256::digest(text.as_bytes());
                match private.sign(Pkcs1v15Sign::new::<sha2::Sha256>(), &digest) {
                    Ok(signature) => Ok(Value::Ok(Box::new(Value::string(base64_encode(
                        &signature,
                    ))))),
                    Err(error) => Ok(Value::Err(Box::new(Value::string(error.to_string())))),
                }
            }
            "rsa_verify" => {
                let text = self.string_arg(&args, 0, name, line)?;
                let signature = self.string_arg(&args, 1, name, line)?;
                let pem = self.string_arg(&args, 2, name, line)?;
                let public = match parse_public_key(&pem) {
                    Ok(public) => public,
                    Err(_) => return Ok(Value::Bool(false)),
                };
                let signature = match base64_decode(&signature) {
                    Some(signature) => signature,
                    None => return Ok(Value::Bool(false)),
                };
                let digest = sha2::Sha256::digest(text.as_bytes());
                Ok(Value::Bool(
                    public
                        .verify(Pkcs1v15Sign::new::<sha2::Sha256>(), &digest, &signature)
                        .is_ok(),
                ))
            }
            "rsa_encrypt" => {
                let text = self.string_arg(&args, 0, name, line)?;
                let pem = self.string_arg(&args, 1, name, line)?;
                let public = match parse_public_key(&pem) {
                    Ok(public) => public,
                    Err(message) => return Ok(Value::Err(Box::new(Value::string(message)))),
                };
                match public.encrypt(&mut OsRng, Oaep::new::<sha2::Sha256>(), text.as_bytes()) {
                    Ok(cipher) => {
                        Ok(Value::Ok(Box::new(Value::string(base64_encode(&cipher)))))
                    }
                    Err(error) => Ok(Value::Err(Box::new(Value::string(error.to_string())))),
                }
            }
            "rsa_decrypt" => {
                let encoded = self.string_arg(&args, 0, name, line)?;
                let pem = self.string_arg(&args, 1, name, line)?;
                let private = match parse_private_key(&pem) {
                    Ok(private) => private,
                    Err(message) => return Ok(Value::Err(Box::new(Value::string(message)))),
                };
                let cipher = match base64_decode(&encoded) {
                    Some(cipher) => cipher,
                    None => {
                        return Ok(Value::Err(Box::new(Value::string(
                            "invalid base64 input".to_string(),
                        ))))
                    }
                };
                match private.decrypt(Oaep::new::<sha2::Sha256>(), &cipher) {
                    Ok(plain) => Ok(Value::Ok(Box::new(Value::string(
                        String::from_utf8_lossy(&plain).to_string(),
                    )))),
                    Err(error) => Ok(Value::Err(Box::new(Value::string(error.to_string())))),
                }
            }
            _ => self.error(format!("std/crypto has no function '{}'", name), line),
        }
    }

    /// `std/task`：后台任务。每个任务在独立线程里运行，拥有自己的解释器状态。
    fn task_module(
        &mut self,
        name: &str,
        args: Vec<Value>,
        line: u32,
    ) -> FlowResult<Value> {
        match name {
            // task.spawn("函数名")：异步执行一次，立即返回。
            "spawn" => {
                if args.len() != 1 {
                    return self.error("task.spawn expects one function name", line);
                }
                let target = match &args[0] {
                    Value::Str(text) => text.to_string(),
                    other => {
                        return self.error(
                            format!("task.spawn expects string, got {}", format_value(other)),
                            line,
                        )
                    }
                };
                if !self.has_func(&target) {
                    return self.error(format!("unknown function '{}'", target), line);
                }
                let ok = self.spawn_task(target, None, line);
                return Ok(Value::Bool(ok));
            }
            // task.sleep(毫秒)：让当前线程休眠。
            "sleep" => {
                if args.len() != 1 {
                    return self.error("task.sleep expects one number", line);
                }
                let ms = match &args[0] {
                    Value::Int(value) => *value,
                    other => {
                        return self.error(
                            format!("task.sleep expects int, got {}", format_value(other)),
                            line,
                        )
                    }
                };
                if ms < 0 || ms > 86_400_000 {
                    return self.error("task.sleep out of range (0..86400000)", line);
                }
                std::thread::sleep(Duration::from_millis(ms as u64));
                Ok(Value::Null)
            }
            other => self.error(format!("std/task has no function '{}'", other), line),
        }
    }

    fn http_module(
        &mut self,
        name: &str,
        args: Vec<Value>,
        line: u32,
    ) -> FlowResult<Value> {
        match name {
            // http.save_file(目录, files[i])：把上传文件写入指定目录，返回保存路径。
            "save_file" => {
                let dir = self.string_arg(&args, 0, name, line)?;
                let file = match args.get(1) {
                    Some(file @ Value::Ary(_)) => file.clone(),
                    _ => return self.error("save_file expects (string, http.file)", line),
                };
                let (filename, data) = match &file {
                    Value::Ary(ary) => {
                        let ary = ary.borrow();
                        let filename = ary
                            .get("filename")
                            .and_then(|v| v.as_str())
                            .unwrap_or_default();
                        let encoded = ary
                            .get("data_base64")
                            .and_then(|v| v.as_str())
                            .unwrap_or_default();
                        (filename, encoded)
                    }
                    _ => (String::new(), String::new()),
                };
                let bytes = match base64_decode(&data) {
                    Some(bytes) => bytes,
                    None => {
                        return Ok(Value::Err(Box::new(Value::string(
                            "invalid file data".to_string(),
                        ))))
                    }
                };
                let safe = safe_file_name(&filename);
                if safe.is_empty() {
                    return Ok(Value::Err(Box::new(Value::string(
                        "file has no usable name".to_string(),
                    ))));
                }
                if let Err(error) = fs::create_dir_all(&dir) {
                    return Ok(Value::Err(Box::new(Value::string(error.to_string()))));
                }
                let path = std::path::Path::new(&dir).join(&safe);
                // 拒绝覆盖符号链接或目录：避免目录里预先放置的链接把写入引到别处。
                if let Ok(meta) = fs::symlink_metadata(&path) {
                    if meta.file_type().is_symlink() || meta.is_dir() {
                        return Ok(Value::Err(Box::new(Value::string(
                            "refusing to overwrite a symlink or directory".to_string(),
                        ))));
                    }
                }
                match fs::write(&path, &bytes) {
                    Ok(()) => Ok(Value::Ok(Box::new(Value::string(
                        path.to_string_lossy().to_string(),
                    )))),
                    Err(error) => Ok(Value::Err(Box::new(Value::string(error.to_string())))),
                }
            }
            // http.fetch_retry(方法, URL, 请求头, 请求体, 次数)：连接失败或 5xx 时退避重试。
            "fetch_retry" => {
                let method = self.string_arg(&args, 0, name, line)?;
                let url = self.string_arg(&args, 1, name, line)?;
                let headers = header_pairs(args.get(2));
                let body = args
                    .get(3)
                    .and_then(|value| value.as_str())
                    .unwrap_or_default()
                    .to_string();
                let attempts = self.int_arg(&args, 4, name, line)?.clamp(1, 5);
                let mut last_error = String::new();
                let mut last_response: Option<(i64, Vec<(String, String)>, String)> = None;
                for attempt in 0..attempts {
                    if attempt > 0 {
                        std::thread::sleep(Duration::from_millis(100 * attempt as u64));
                    }
                    let budget = self.remaining_time();
                    match crate::client::fetch(&method, &url, &headers, body.as_bytes(), budget) {
                        Ok((status, received, received_body)) if status < 500 => {
                            return Ok(Value::Ok(Box::new(client_response_value(
                                status,
                                received,
                                received_body,
                            ))));
                        }
                        Ok(response) => {
                            last_response = Some(response);
                            last_error.clear();
                        }
                        Err(message) => {
                            last_response = None;
                            last_error = message;
                        }
                    }
                }
                match last_response {
                    Some((status, received, received_body)) => Ok(Value::Ok(Box::new(
                        client_response_value(status, received, received_body),
                    ))),
                    None => Ok(Value::Err(Box::new(Value::string(last_error)))),
                }
            }
            // http.fetch(方法, URL, 请求头, 请求体)：出站访问第三方服务，返回响应对象。
            "fetch" | "fetch_file" => {
                let method = self.string_arg(&args, 0, name, line)?;
                let url = self.string_arg(&args, 1, name, line)?;
                let headers = self.with_traceparent(header_pairs(args.get(2)));
                let budget = self.remaining_time();
                let fetched = if name == "fetch_file" {
                    let path = self.string_arg(&args, 3, name, line)?;
                    crate::client::fetch_file(&method, &url, &headers, &path, budget)
                } else {
                    let body = args
                        .get(3)
                        .and_then(|value| value.as_str())
                        .unwrap_or_default();
                    crate::client::fetch(&method, &url, &headers, body.as_bytes(), budget)
                };
                match fetched {
                    Ok((status, received, received_body)) => Ok(Value::Ok(Box::new(
                        client_response_value(status, received, received_body),
                    ))),
                    Err(message) => Ok(Value::Err(Box::new(Value::string(message)))),
                }
            }
            // http.download(URL, 请求头, 目标文件)：GET 并把原始字节写入文件，返回字节数。
            "download" => {
                let url = self.string_arg(&args, 0, name, line)?;
                let headers = self.with_traceparent(header_pairs(args.get(1)));
                let path = self.string_arg(&args, 2, name, line)?;
                let budget = self.remaining_time();
                match crate::client::fetch_bytes("GET", &url, &headers, &[], budget) {
                    Ok((status, _, body)) if (200..300).contains(&status) => {
                        match fs::write(&path, &body) {
                            Ok(()) => Ok(Value::Ok(Box::new(Value::Int(body.len() as i64)))),
                            Err(error) => Ok(Value::Err(Box::new(Value::string(format!(
                                "cannot write '{}': {}",
                                path, error
                            ))))),
                        }
                    }
                    Ok((status, _, _)) => Ok(Value::Err(Box::new(Value::string(format!(
                        "download failed with status {}",
                        status
                    ))))),
                    Err(message) => Ok(Value::Err(Box::new(Value::string(message)))),
                }
            }
            "serve" => {
                let port = match self.http_port {
                    Some(port) => port as i64,
                    None => self.int_arg(&args, 0, name, line)?,
                };
                if !(0..=65535).contains(&port) {
                    return self.error("port must be 0..65535", line);
                }
                let handler = match args.get(1) {
                    Some(handler @ Value::Func(_)) => handler.clone(),
                    _ => return self.error("serve expects a handler function", line),
                };
                self.serve_http(port as u16, handler, line)
            }
            "router" => {
                let routes_value = match args.first() {
                    Some(Value::Vec(items)) => items.borrow().clone(),
                    _ => return self.error("router expects vec<http.route>", line),
                };
                // 注解路由先注册（按函数名排序保证确定性），显式 routes 后注册。
                let mut annotated: Vec<Route> = Vec::new();
                if self.native.is_some() {
                    // AOT 二进制没有 AST，注解路由来自生成的模块描述。
                    let (routes, funcs) = match &self.native {
                        Some(module) => (module.routes.clone(), module.funcs.clone()),
                        None => (
                            Arc::new(Vec::new()),
                            Arc::new(std::collections::HashMap::new()),
                        ),
                    };
                    for route in routes.iter() {
                        let entry = match funcs.get(&route.handler) {
                            Some(entry) => *entry,
                            None => continue,
                        };
                        annotated.push(Route {
                            method: route.method.clone(),
                            path: route.path.clone(),
                            handler: Value::func_rc(Rc::new(Function {
                                name: route.handler.clone(),
                                kind: FuncKind::Native {
                                    arity: entry.arity,
                                    body: Rc::new(
                                        move |host: &mut Runtime, args: Vec<Value>, line: u32| {
                                            (entry.body)(host, args, line)
                                        },
                                    ),
                                },
                            })),
                        });
                    }
                } else {
                    #[cfg(feature = "interp")]
                    {
                        let mut names: Vec<String> = self.funcs.keys().cloned().collect();
                        names.sort();
                        for func_name in names {
                            let decl = match self.funcs.get(&func_name) {
                                Some(decl) => decl.clone(),
                                None => continue,
                            };
                            for attribute in &decl.attributes {
                                let method = match annotation_method(&attribute.name) {
                                    Some(method) => method,
                                    None => continue,
                                };
                                if attribute.args.len() != 1 {
                                    continue;
                                }
                                annotated.push(Route {
                                    method: method.to_string(),
                                    path: attribute.args[0].clone(),
                                    handler: Value::func_rc(Rc::new(Function {
                                        name: func_name.clone(),
                                        kind: FuncKind::Decl(decl.clone()),
                                    })),
                                });
                            }
                        }
                    }
                }
                let mut routes = annotated;
                for route in routes_value {
                    let ary = match &route {
                        Value::Ary(ary) => ary.borrow(),
                        _ => return self.error("router expects http.route values", line),
                    };
                    let method = ary
                        .get("method")
                        .and_then(|v| v.as_str())
                        .unwrap_or_else(|| "GET".to_string());
                    let path = ary
                        .get("path")
                        .and_then(|v| v.as_str())
                        .unwrap_or_else(|| "/".to_string());
                    let handler = match ary.get("handler") {
                        Some(handler @ Value::Func(_)) => handler,
                        _ => return self.error("route handler must be a function", line),
                    };
                    routes.push(Route {
                        method,
                        path,
                        handler,
                    });
                }
                Ok(Value::func_rc(Rc::new(Function {
                    name: "<router>".to_string(),
                    kind: FuncKind::Router(Rc::new(RouteTable::new(routes))),
                })))
            }
            "get" | "post" | "put" | "delete" | "patch" | "options" | "head" => {
                let path = self.string_arg(&args, 0, name, line)?;
                let handler = match args.get(1) {
                    Some(handler @ Value::Func(_)) => handler.clone(),
                    _ => return self.error("route handler must be a function", line),
                };
                Ok(make_route(name.to_uppercase(), path, handler))
            }
            "static" => {
                let prefix = self.string_arg(&args, 0, name, line)?;
                let dir = self.string_arg(&args, 1, name, line)?;
                let handler = Value::func_rc(Rc::new(Function {
                    name: "<static>".to_string(),
                    kind: FuncKind::Static {
                        dir,
                        prefix: prefix.clone(),
                    },
                }));
                let pattern = if prefix == "/" {
                    "/*".to_string()
                } else {
                    format!("{}/*", prefix.trim_end_matches('/'))
                };
                Ok(make_route("*".to_string(), pattern, handler))
            }
            "metrics" => Ok(Value::string(
                self.metrics
                    .as_ref()
                    .map(|metrics| metrics.prometheus())
                    .unwrap_or_default(),
            )),
            // 中间件链：`http.chain([auth, log], router)`；首次请求时组合并缓存。
            "chain" => {
                let middlewares = match args.first() {
                    Some(Value::Vec(items)) => items.borrow().clone(),
                    None => Vec::new(),
                    _ => return self.error("http.chain expects a vec of middlewares", line),
                };
                let handler = match args.get(1) {
                    Some(handler @ Value::Func(_)) => handler.clone(),
                    _ => return self.error("http.chain expects a handler", line),
                };
                for middleware in &middlewares {
                    if !matches!(middleware, Value::Func(_)) {
                        return self.error("middleware must be a function", line);
                    }
                }
                Ok(Value::func_rc(Rc::new(Function {
                    name: "<chain>".to_string(),
                    kind: FuncKind::Chain {
                        middlewares,
                        handler,
                        composed: std::cell::OnceCell::new(),
                    },
                })))
            }
            // 路由分组：`http.group("/api", [auth], [http.get(...)])`。
            "group" => {
                let prefix = self.string_arg(&args, 0, name, line)?;
                let middlewares = match args.get(1) {
                    Some(Value::Vec(items)) => items.borrow().clone(),
                    None => Vec::new(),
                    _ => return self.error("http.group expects a vec of middlewares", line),
                };
                let routes_value = match args.get(2) {
                    Some(Value::Vec(items)) => items.borrow().clone(),
                    _ => return self.error("http.group expects a vec of routes", line),
                };
                let mut routes = Vec::with_capacity(routes_value.len());
                for route in routes_value {
                    let ary = match &route {
                        Value::Ary(ary) => ary.borrow(),
                        _ => return self.error("http.group expects http.route values", line),
                    };
                    let method = ary
                        .get("method")
                        .and_then(|v| v.as_str())
                        .unwrap_or_else(|| "GET".to_string());
                    let path = ary
                        .get("path")
                        .and_then(|v| v.as_str())
                        .unwrap_or_else(|| "/".to_string());
                    let handler = match ary.get("handler") {
                        Some(handler @ Value::Func(_)) => handler,
                        _ => return self.error("route handler must be a function", line),
                    };
                    routes.push(Route {
                        method,
                        path,
                        handler,
                    });
                }
                let router = Value::func_rc(Rc::new(Function {
                    name: "<router>".to_string(),
                    kind: FuncKind::Router(Rc::new(RouteTable::new(routes))),
                }));
                let inner = if middlewares.is_empty() {
                    router
                } else {
                    Value::func_rc(Rc::new(Function {
                        name: "<chain>".to_string(),
                        kind: FuncKind::Chain {
                            middlewares,
                            handler: router,
                            composed: std::cell::OnceCell::new(),
                        },
                    }))
                };
                let trimmed = prefix.trim_end_matches('/');
                let mounted = Value::func_rc(Rc::new(Function {
                    name: "<group>".to_string(),
                    kind: FuncKind::Mounted {
                        prefix: trimmed.to_string(),
                        inner,
                    },
                }));
                let pattern = if trimmed.is_empty() {
                    "/*".to_string()
                } else {
                    format!("{}/*", trimmed)
                };
                Ok(make_route("*".to_string(), pattern, mounted))
            }
            // CORS 中间件工厂：`http.chain([http.cors("*")], router)`。
            "cors" => {
                let origin = self.string_arg(&args, 0, name, line)?;
                Ok(Value::func_rc(Rc::new(Function {
                    name: "<cors>".to_string(),
                    kind: FuncKind::Middleware(crate::value::MiddlewareSpec::Cors { origin }),
                })))
            }
            // 安全响应头中间件工厂。
            "security_headers" => Ok(Value::func_rc(Rc::new(Function {
                name: "<security>".to_string(),
                kind: FuncKind::Middleware(crate::value::MiddlewareSpec::Headers {
                    headers: security_header_pairs(),
                }),
            }))),
            // 自定义响应头中间件工厂：`http.with_headers(["X-Trace" = "1"])`。
            "with_headers" => {
                let headers = match args.first() {
                    Some(Value::Map(map)) => {
                        let map = map.borrow();
                        let mut pairs = Vec::with_capacity(map.entries.len());
                        for (key, value) in &map.entries {
                            pairs.push((
                                key.as_str().unwrap_or_default(),
                                value.as_str().unwrap_or_default(),
                            ));
                        }
                        pairs
                    }
                    _ => return self.error("http.with_headers expects a map", line),
                };
                let headers = headers
                    .into_iter()
                    .filter(|(key, _)| !key.is_empty())
                    .map(|(key, value)| (key.to_string(), value.to_string()))
                    .collect();
                Ok(Value::func_rc(Rc::new(Function {
                    name: "<headers>".to_string(),
                    kind: FuncKind::Middleware(crate::value::MiddlewareSpec::Headers { headers }),
                })))
            }
            // 路由级限流中间件：`http.rate_limit(20, 40)` 按客户端 IP 独立计数。
            "rate_limit" => {
                let rate = self.int_arg(&args, 0, name, line)?.clamp(1, 1_000_000) as u32;
                let burst = args
                    .get(1)
                    .and_then(|value| value.as_int())
                    .unwrap_or(0)
                    .clamp(0, 1_000_000) as u32;
                let limiter = crate::server::RateLimiter::new(rate, burst, true)
                    .expect("rate is clamped to at least 1");
                Ok(Value::func_rc(Rc::new(Function {
                    name: "<rate-limit>".to_string(),
                    kind: FuncKind::Middleware(crate::value::MiddlewareSpec::RateLimit {
                        limiter: Arc::new(limiter),
                    }),
                })))
            }
            // OpenAPI 3.0：从路由表生成 JSON，`handler` 传 `http.router(...)` 的返回值。
            "openapi" => {
                let handler = match args.first() {
                    Some(value) => value.clone(),
                    None => return self.error("http.openapi expects a handler", line),
                };
                let title = args
                    .get(1)
                    .and_then(|value| value.as_str())
                    .unwrap_or_else(|| "Rip API".to_string());
                let version = args
                    .get(2)
                    .and_then(|value| value.as_str())
                    .unwrap_or_else(|| "1.0.0".to_string());
                Ok(Value::string(openapi_json(&handler, &title, &version)))
            }
            _ => self.error(format!("std/http has no function '{}'", name), line),
        }
    }

    fn serve_http(&mut self, port: u16, handler: Value, line: u32) -> FlowResult<Value> {
        // 开发默认只监听回环地址；对外提供服务时用 `rip run --host 0.0.0.0` 或指定本机 IP / 域名。
        // 其余参数来自 `rip.toml` 的 `[server]`，命令行优先。
        let mut config = self
            .server_config
            .clone()
            .unwrap_or_else(crate::server::ServerConfig::default);
        if let Some(host) = &self.http_host {
            config.host = host.clone();
        }
        // --cert / --key 同时给出时启用 TLS；只给一个视为配置错误。
        let tls = match (&self.tls_cert, &self.tls_key) {
            (Some(cert), Some(key)) => match crate::tls::server_config(cert, key) {
                Ok(config) => Some(config),
                Err(message) => return self.error(message, line),
            },
            (None, None) => None,
            _ => {
                return self.error(
                    "HTTPS requires both --cert and --key",
                    line,
                )
            }
        };
        let spec = match handler_spec(&handler) {
            Some(spec) => spec,
            None => {
                return self.error(
                    "http.serve handler must be a named function or a router",
                    line,
                )
            }
        };
        let listener = match TcpListener::bind((config.host.as_str(), port)) {
            Ok(listener) => listener,
            Err(error) => {
                return self.error(
                    format!("cannot bind {}:{}: {}", config.host, port, error),
                    line,
                );
            }
        };
        let shown = if config.host.contains(':') && !config.host.starts_with('[') {
            format!("[{}]", config.host)
        } else {
            config.host.clone()
        };
        let scheme = if tls.is_some() { "https" } else { "http" };
        crate::server::install_signal_handler();

        let metrics = Arc::new(crate::server::Metrics::default());
        self.metrics = Some(Arc::clone(&metrics));
        // 追踪导出：未配置端点时 `configure` 只关掉开关，热路径没有额外开销。
        crate::trace::configure(
            &config.otlp_endpoint,
            &config.otlp_service,
            config.otlp_sample_ratio,
        );
        if crate::trace::enabled() {
            self.say(
                format!("OTLP tracing enabled: {}", config.otlp_endpoint),
                format!("已启用 OTLP 追踪导出：{}", config.otlp_endpoint),
            );
        }
        // 限流器跨所有连接共享，因此在这里创建后分发给各工作线程。
        let limiter = crate::server::RateLimiter::new(
            config.rate_limit,
            config.rate_limit_burst,
            config.rate_limit_per_ip,
        )
        .map(Arc::new);
        // 可信代理配置：一次解析成 CIDR 列表，所有连接共用。
        let trust = Arc::new(crate::server::ProxyTrust::new(
            config.trust_proxy,
            &config.trusted_proxies,
        ));
        if config.trust_proxy && config.trusted_proxies.is_empty() {
            self.say(
                "[server] warning: trust_proxy=true without trusted_proxies lets clients \
                 spoof X-Forwarded-For (per-IP rate limit and audit can be bypassed); \
                 configure trusted_proxies CIDR ranges."
                    .to_string(),
                "[server] 警告：trust_proxy=true 且未配置 trusted_proxies，X-Forwarded-For \
                 可被客户端伪造，按 IP 限流与访问审计可被绕过；建议配置可信代理网段。"
                    .to_string(),
            );
        }
        let workers = config.worker_count();
        self.say(
            format!(
                "Rip HTTP server listening on {}://{}:{} ({} workers, keep-alive {})",
                scheme,
                shown,
                port,
                workers,
                config.keep_alive.as_millis()
            ),
            format!(
                "Rip HTTP 服务器已启动：{}://{}:{}（{} 个工作线程，keep-alive {}ms）",
                scheme,
                shown,
                port,
                workers,
                config.keep_alive.as_millis()
            ),
        );
        // 明文 HTTP 走事件驱动：连接由 tokio 管理，不占用解释器线程。
        if tls.is_none() {
            let result = crate::server_async::run(
                listener,
                config,
                spec,
                Arc::clone(&metrics),
                limiter,
                Arc::clone(&trust),
                self.seed(),
                self.language,
            );
            return match result {
                Ok(()) => Ok(Value::Null),
                Err(error) => self.error(error, line),
            };
        }
        // 有界队列：连接堆积到上限后直接拒绝，形成背压而不是无限占用内存。
        let (sender, receiver) =
            crossbeam_channel::bounded::<TcpStream>(config.max_connections.max(16));
        // 排队深度：工作线程据此决定是否复用连接。
        // 队列里还有等着的连接时，响应带 `Connection: close`，让当前线程尽快去服务下一个，
        // 避免 keep-alive 把并发度锁死在“工作线程数”上。
        let pending = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let mut worker_handles = Vec::with_capacity(workers);
        for index in 0..workers {
            let receiver = receiver.clone();
            let worker_config = config.clone();
            let worker_spec = spec.clone();
            let worker_metrics = Arc::clone(&metrics);
            let worker_tls = tls.clone();
            let worker_pending = Arc::clone(&pending);
            let worker_limiter = limiter.clone();
            let worker_trust = Arc::clone(&trust);
            let seed = self.seed();
            let language = self.language;
            match std::thread::Builder::new()
                .name(format!("rip-http-{}", index))
                .spawn(move || {
                    // 每个工作线程拥有独立运行时：Rip 变量不跨线程共享。
                    let mut runtime = seed.build();
                    runtime.set_language(language);
                    if let Err(error) = runtime.init_entry() {
                        eprintln!("{}", error.message);
                        return;
                    }
                    runtime.serve_connections(
                        receiver,
                        worker_tls,
                        worker_config,
                        worker_spec,
                        worker_metrics,
                        worker_pending,
                        worker_limiter,
                        worker_trust,
                    );
                })
            {
                Ok(handle) => worker_handles.push(handle),
                Err(error) => {
                    return self.error(
                        format!("cannot start http worker {}: {}", index, error),
                        line,
                    )
                }
            }
        }
        // 停机唤醒：accept 阻塞时用一个本地连接把它叫醒。
        let wake_port = port;
        std::thread::Builder::new()
            .name("rip-http-wake".to_string())
            .spawn(move || {
                while !crate::server::shutdown_requested() {
                    std::thread::sleep(Duration::from_millis(100));
                }
                let _ = TcpStream::connect(("127.0.0.1", wake_port));
                let _ = TcpStream::connect(("::1", wake_port));
            })
            .ok();

        let max_connections = config.max_connections;
        for incoming in listener.incoming() {
            if crate::server::shutdown_requested() {
                self.say(
                    "[http] shutdown signal received, draining connections",
                    "[http] 收到停机信号，正在处理剩余连接",
                );
                break;
            }
            match incoming {
                Ok(stream) => {
                    let _ = stream.set_nodelay(true);
                    if let Err(crossbeam_channel::TrySendError::Full(stream)) = sender.try_send(stream)
                    {
                        metrics.rejected.fetch_add(1, Ordering::Relaxed);
                        let _ = write_rejection(stream);
                    }
                    pending.store(sender.len(), Ordering::Relaxed);
                }
                Err(error) => eprintln!(
                    "{}",
                    if self.language.is_chinese() {
                        format!("[http] 接受连接失败：{}", error)
                    } else {
                        format!("[http] accept error: {}", error)
                    }
                ),
            }
        }

        // 关闭队列入口，等工作线程把已排队的连接处理完。
        drop(sender);
        for handle in worker_handles {
            let _ = handle.join();
        }
        self.say(
            format!("[http] server stopped (max connections {})", max_connections),
            format!("[http] 服务器已停止（最大连接数 {}）", max_connections),
        );
        Ok(Value::Null)
    }

    /// 工作线程主循环：从队列取连接，逐个处理。
    pub(crate) fn serve_connections(
        &mut self,
        receiver: crossbeam_channel::Receiver<TcpStream>,
        tls: Option<Arc<rustls::ServerConfig>>,
        config: crate::server::ServerConfig,
        spec: crate::server::HandlerSpec,
        metrics: Arc<crate::server::Metrics>,
        pending: Arc<std::sync::atomic::AtomicUsize>,
        limiter: Option<Arc<crate::server::RateLimiter>>,
        trust: Arc<crate::server::ProxyTrust>,
    ) {
        self.rate_limiter = limiter;
        self.proxy_trust = Some(trust);
        let handler = match self.handler_from_spec(&spec) {
            Some(handler) => handler,
            None => {
                self.say(
                    "[http] handler is not available in this worker",
                    "[http] 工作线程无法解析处理函数",
                );
                return;
            }
        };
        self.metrics = Some(Arc::clone(&metrics));
        // 停机后仍需把已排队的连接处理完，但不能无限等：超过 drain_timeout 就放弃。
        let mut drain_deadline: Option<Instant> = None;
        loop {
            if crate::server::shutdown_requested() {
                let deadline = *drain_deadline
                    .get_or_insert_with(|| Instant::now() + config.drain_timeout);
                if Instant::now() >= deadline {
                    break;
                }
            }
            let stream = match receiver.recv_timeout(Duration::from_millis(200)) {
                Ok(stream) => stream,
                Err(crossbeam_channel::RecvTimeoutError::Timeout) => continue,
                Err(crossbeam_channel::RecvTimeoutError::Disconnected) => break,
            };
            // 取走一个连接后同步排队深度，避免残留值让后续连接永远不复用。
            let _ = pending.try_update(Ordering::Relaxed, Ordering::Relaxed, |value| {
                value.checked_sub(1)
            });
            let _ = stream.set_read_timeout(Some(config.read_timeout));
            let _ = stream.set_write_timeout(Some(config.write_timeout));
            let client_ip = stream
                .peer_addr()
                .map(|addr| addr.ip().to_string())
                .unwrap_or_default();
            let connection = match &tls {
                Some(tls_config) => match HttpStream::tls(tls_config.clone(), stream) {
                    Ok(connection) => connection,
                    Err(error) => {
                        self.say(
                            format!("[https] {}", error),
                            format!("[https] TLS 握手失败：{}", error),
                        );
                        continue;
                    }
                },
                None => HttpStream::plain(stream),
            };
            metrics
                .active_connections
                .fetch_add(1, Ordering::Relaxed);
            // 单连接内的任何 panic 都被回收，工作线程继续服务后续连接。
            let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                self.handle_connection(connection, client_ip, &handler, &config, &metrics, &pending)
            }));
            metrics
                .active_connections
                .fetch_sub(1, Ordering::Relaxed);
            match outcome {
                Ok(Ok(())) => {}
                Ok(Err(error)) => self.say(
                    format!("[http] {}", error),
                    format!(
                        "[http] 请求处理失败：{}",
                        crate::i18n::localize(self.language, &error)
                    ),
                ),
                Err(_) => {
                    // panic 展开会跳过正常清理，这里重置调用栈与请求 ID。
                    self.reset_after_panic();
                    self.say(
                        "[http] worker recovered from a panic",
                        "[http] 工作线程从 panic 中恢复",
                    );
                }
            }
        }
    }

    /// 按处理函数描述重建 `Value::Func`（工作线程各有自己的解释器）。
    pub(crate) fn handler_from_spec(&self, spec: &crate::server::HandlerSpec) -> Option<Value> {
        use crate::server::HandlerSpec;
        match spec {
            HandlerSpec::Func(name) => {
                #[cfg(feature = "interp")]
                if let Some(decl) = self.funcs.get(name) {
                    return Some(Value::func_rc(Rc::new(Function {
                        name: name.clone(),
                        kind: FuncKind::Decl(decl.clone()),
                    })));
                }
                // AOT 二进制：按名字从原生函数表重建处理函数。
                let entry = self
                    .native
                    .as_ref()
                    .and_then(|module| module.funcs.get(name).copied())?;
                Some(Value::func_rc(Rc::new(Function {
                    name: name.clone(),
                    kind: FuncKind::Native {
                        arity: entry.arity,
                        body: Rc::new(move |host: &mut Runtime, args: Vec<Value>, line: u32| {
                            (entry.body)(host, args, line)
                        }),
                    },
                })))
            }
            HandlerSpec::Static { dir, prefix } => Some(Value::func_rc(Rc::new(Function {
                name: "<static>".to_string(),
                kind: FuncKind::Static {
                    dir: dir.clone(),
                    prefix: prefix.clone(),
                },
            }))),
            HandlerSpec::Middleware(spec) => Some(Value::func_rc(Rc::new(Function {
                name: "<middleware>".to_string(),
                kind: FuncKind::Middleware(spec.clone()),
            }))),
            HandlerSpec::Chain {
                middlewares,
                handler,
            } => {
                let mut built = Vec::with_capacity(middlewares.len());
                for middleware in middlewares {
                    built.push(self.handler_from_spec(middleware)?);
                }
                Some(Value::func_rc(Rc::new(Function {
                    name: "<chain>".to_string(),
                    kind: FuncKind::Chain {
                        middlewares: built,
                        handler: self.handler_from_spec(handler)?,
                        composed: std::cell::OnceCell::new(),
                    },
                })))
            }
            HandlerSpec::Mounted { prefix, inner } => Some(Value::func_rc(Rc::new(Function {
                name: "<group>".to_string(),
                kind: FuncKind::Mounted {
                    prefix: prefix.clone(),
                    inner: self.handler_from_spec(inner)?,
                },
            }))),
            HandlerSpec::Router(routes) => {
                let mut built = Vec::with_capacity(routes.len());
                for route in routes {
                    built.push(Route {
                        method: route.method.clone(),
                        path: route.path.clone(),
                        handler: self.handler_from_spec(&route.handler)?,
                    });
                }
                Some(Value::func_rc(Rc::new(Function {
                    name: "<router>".to_string(),
                    kind: FuncKind::Router(Rc::new(RouteTable::new(built))),
                })))
            }
        }
    }

    /// 单个连接的处理循环：支持 keep-alive、请求大小上限与内置探针。
    fn handle_connection(
        &mut self,
        stream: HttpStream,
        client_ip: String,
        handler: &Value,
        config: &crate::server::ServerConfig,
        metrics: &crate::server::Metrics,
        pending: &std::sync::atomic::AtomicUsize,
    ) -> Result<(), String> {
        let mut reader = BufReader::new(stream);
        let mut served = 0usize;
        loop {
            let started = Instant::now();
            // 首个请求用 read_timeout，之后用 keep-alive 空闲超时；两者都作为
            // 整个请求（头 + 体）的预算，挡住慢速逐字节发送的连接。
            let budget = if served == 0 {
                config.read_timeout
            } else {
                config.keep_alive
            };
            let outcome = {
                let mut limited = DeadlineReader {
                    inner: &mut reader,
                    deadline: started + budget,
                };
                read_request(&mut limited, config)
            };
            let head = match outcome {
                ReadOutcome::Request(head) => head,
                ReadOutcome::Closed => return Ok(()),
                ReadOutcome::TooLarge => {
                    let response = make_response(413, "payload too large");
                    let _ = write_http_response(reader.get_mut(), &response, false, false, "");
                    return Ok(());
                }
                ReadOutcome::BadRequest(message) => {
                    let response = make_response(400, &message);
                    let _ = write_http_response(reader.get_mut(), &response, false, false, "");
                    return Ok(());
                }
            };
            let request_id = head
                .headers
                .iter()
                .find(|(key, _)| key.eq_ignore_ascii_case("x-request-id"))
                .map(|(_, value)| value.clone())
                // 客户端给的 ID 会原样写进响应头，必须过滤 CR/LF 与超长值。
                .filter(|value| !value.is_empty() && value.len() <= 200 && is_header_value(value))
                .unwrap_or_else(next_request_id);
            // 上游带 W3C traceparent 时沿用其 trace id，便于跨服务串联。
            // 开启 OTLP 导出后在这里创建服务端 span，并把 span id 注入子 span。
            let mut span = crate::trace::start_http_span(&head.headers);
            let trace_id = span
                .as_ref()
                .map(|span| span.trace_id.clone())
                .or_else(|| {
                    head.headers
                        .iter()
                        .find(|(key, _)| key.eq_ignore_ascii_case("traceparent"))
                        .and_then(|(_, value)| crate::server::trace_id_from_traceparent(value))
                });
            let method = head.method.clone();
            let path = head.path.clone();
            let keep_alive = head.keep_alive
                && served + 1 < config.max_requests
                && !crate::server::shutdown_requested()
                // 还有连接在排队时优先服务新连接，避免 keep-alive 占住工作线程。
                && pending.load(Ordering::Relaxed) == 0;
            let head_only = head.method.eq_ignore_ascii_case("HEAD");
            // 反向代理之后用 X-Forwarded-For / X-Real-IP 还原真实客户端 IP。
            let client_ip = match &self.proxy_trust {
                Some(trust) => trust.resolve(&client_ip, &head.headers),
                None => client_ip.clone(),
            };
            let accept_encoding = head
                .headers
                .iter()
                .find(|(key, _)| key.eq_ignore_ascii_case("accept-encoding"))
                .map(|(_, value)| value.clone());

            let mut route = "unmatched".to_string();
            let response = if !config.health_path.is_empty() && path == config.health_path {
                route = config.health_path.clone();
                make_response(200, "ok")
            } else if !config.ready_path.is_empty() && path == config.ready_path {
                route = config.ready_path.clone();
                if crate::server::shutdown_requested() {
                    make_response(503, "shutting down")
                } else {
                    make_response(200, "ready")
                }
            } else if !config.metrics_path.is_empty() && path == config.metrics_path {
                route = config.metrics_path.clone();
                metrics_response(&metrics.prometheus())
            } else if self
                .rate_limiter
                .as_ref()
                .is_some_and(|limiter| !limiter.allow(&client_ip))
            {
                // 超过配额：直接 429，不进入处理函数。
                metrics.rate_limited.fetch_add(1, Ordering::Relaxed);
                route = "rate_limited".to_string();
                rate_limited_response()
            } else {
                let request = build_request(*head, client_ip.clone(), &request_id);
                metrics.in_flight.fetch_add(1, Ordering::Relaxed);
                let _in_flight = InFlightGuard(&metrics.in_flight);
                self.current_request_id = Some(request_id.clone());
                self.current_trace_id = trace_id.clone();
                self.current_span_id = span.as_ref().map(|span| span.span_id.clone());
                self.current_route = None;
                self.set_deadline(if config.request_timeout.is_zero() {
                    None
                } else {
                    Some(Instant::now() + config.request_timeout)
                });
                let response = match handler {
                    Value::Func(func) => match self.call_value(func, vec![request], 0) {
                        Ok(response) => response,
                        Err(Flow::Error(error)) => {
                            // 内部错误只写服务端日志，不回传给客户端。
                            eprintln!("[http] {} handler error: {}", request_id, error.message);
                            make_response(500, "internal server error")
                        }
                        Err(_) => make_response(500, "handler failed"),
                    },
                    _ => make_response(500, "handler is not callable"),
                };
                let timed_out = self.timed_out();
                if let Some(hit) = self.current_route.take() {
                    route = hit;
                }
                self.clear_deadline();
                self.current_request_id = None;
                self.current_trace_id = None;
                self.current_span_id = None;
                if timed_out {
                    make_response(504, "gateway timeout")
                } else {
                    response
                }
            };

            let mut parts = response_parts(&response);
            // SSE：响应头先发出，随后在同一线程上运行生产函数逐帧推送。
            let mut sse_producer: Option<Value> = None;
            if let Some(producer) = take_sse(&response) {
                parts = into_sse_parts(parts);
                sse_producer = Some(producer);
            }
            maybe_compress(&mut parts, accept_encoding.as_deref(), &method);
            let elapsed = started.elapsed().as_millis() as u64;
            metrics.record(parts.status, elapsed);
            metrics.record_route(&route, parts.status, elapsed);
            if config.access_log {
                crate::server::access_log(&crate::server::access_log_line(
                    &method,
                    &path,
                    parts.status,
                    parts.content_length(),
                    elapsed,
                    &request_id,
                    &route,
                    trace_id.as_deref().unwrap_or(""),
                    &client_ip,
                ));
            }
            if let Some(producer) = sse_producer {
                // 同步路径下连接与生产函数在同一线程：write_parts 只写响应头
                // （流式响应无 Content-Length、Connection: close），随后把整个
                // 连接交给 sink，生产函数逐帧直写 socket，写完即断开连接。
                let mut stream = reader.into_inner();
                let _ = write_parts(&mut stream, &mut parts, false, head_only, &request_id);
                let sink = Rc::new(crate::value::SseSink::new(stream));
                run_sse_producer(self, producer, sink, &request_id);
                if let Some(span) = span.take() {
                    crate::trace::finish_http_span(span, &method, &path, &route, 200, &client_ip);
                }
                crate::gc::collect_if_due();
                return Ok(());
            }
            if let Some(span) = span.take() {
                crate::trace::finish_http_span(
                    span,
                    &method,
                    &path,
                    &route,
                    parts.status,
                    &client_ip,
                );
            }
            write_parts(reader.get_mut(), &mut parts, keep_alive, head_only, &request_id)?;
            // 请求结束时按分配间隔回收环：无环时这一步几乎零开销。
            crate::gc::collect_if_due();
            served += 1;
            if !keep_alive {
                return Ok(());
            }
            // 之后的空闲等待用 keep_alive 超时，而不是首个请求的 read_timeout。
            let _ = reader.get_mut().set_read_timeout(Some(config.keep_alive));
        }
    }

    pub(crate) fn serve_static(
        &mut self,
        dir: &str,
        prefix: &str,
        args: Vec<Value>,
        line: u32,
    ) -> FlowResult<Value> {
        let request = match args.first() {
            Some(request @ Value::Ary(_)) => request.clone(),
            _ => return self.error("static handler expects http.request", line),
        };
        let path = match &request {
            Value::Ary(ary) => ary
                .borrow()
                .get("path")
                .and_then(|v| v.as_str())
                .unwrap_or_else(|| "/".to_string()),
            _ => "/".to_string(),
        };
        let relative = path
            .strip_prefix(prefix)
            .unwrap_or(&path)
            .trim_start_matches('/');
        // 只接受普通路径段：拒绝 `..`、盘符/UNC 前缀、绝对路径与点文件。
        let mut safe = std::path::PathBuf::new();
        for component in std::path::Path::new(relative).components() {
            match component {
                std::path::Component::Normal(part) => {
                    let text = part.to_string_lossy();
                    if text.starts_with('.') {
                        return Ok(make_response(403, "forbidden"));
                    }
                    safe.push(part);
                }
                _ => return Ok(make_response(403, "forbidden")),
            }
        }
        let root = match std::path::Path::new(dir).canonicalize() {
            Ok(root) => root,
            Err(_) => return Ok(make_response(404, "not found")),
        };
        // canonicalize 后再确认仍在 docroot 内，避免符号链接指向目录之外。
        let file_path = match root.join(&safe).canonicalize() {
            Ok(path) => path,
            Err(_) => return Ok(make_response(404, "not found")),
        };
        if !file_path.starts_with(&root) || file_path.is_dir() {
            return Ok(make_response(404, "not found"));
        }
        let metadata = match fs::metadata(&file_path) {
            Ok(metadata) => metadata,
            Err(_) => return Ok(make_response(404, "not found")),
        };
        let size = metadata.len();
        let modified = metadata.modified().ok();
        // ETag 用「大小 + 修改时间」：无需读文件即可判定，且文件变化必然改变 ETag。
        let mtime_secs = modified
            .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|span| span.as_secs())
            .unwrap_or(0);
        let etag = format!("\"{:x}-{:x}\"", size, mtime_secs);
        let last_modified = modified.map(http_date);

        let mut headers = MapData::default();
        headers.set(
            Value::string("Content-Type"),
            Value::string(content_type_for(&file_path)),
        );
        headers.set(Value::string("ETag"), Value::string(etag.clone()));
        headers.set(
            Value::string("Accept-Ranges"),
            Value::string("bytes".to_string()),
        );
        headers.set(
            Value::string("Cache-Control"),
            Value::string(cache_control_for(&file_path).to_string()),
        );
        if let Some(last_modified) = &last_modified {
            headers.set(
                Value::string("Last-Modified"),
                Value::string(last_modified.clone()),
            );
        }

        // 条件请求：命中则回 304，省掉文件内容传输。
        let not_modified = match request_header(&request, "if-none-match") {
            Some(value) => etag_matches(&value, &etag),
            None => match (
                request_header(&request, "if-modified-since"),
                last_modified.as_deref(),
            ) {
                (Some(value), Some(last_modified)) => value.trim() == last_modified,
                _ => false,
            },
        };
        if not_modified {
            return Ok(static_response(304, headers, &file_path, 0, 0));
        }

        // Range 请求：视频/大文件拖动进度条时只传需要的片段。
        if let Some(value) = request_header(&request, "range") {
            match parse_range(&value, size) {
                RangeSpec::Partial(start, end) => {
                    headers.set(
                        Value::string("Content-Range"),
                        Value::string(format!("bytes {}-{}/{}", start, end, size)),
                    );
                    return Ok(static_response(
                        206,
                        headers,
                        &file_path,
                        start,
                        end - start + 1,
                    ));
                }
                RangeSpec::Unsatisfiable => {
                    headers.set(
                        Value::string("Content-Range"),
                        Value::string(format!("bytes */{}", size)),
                    );
                    return Ok(static_response(416, headers, &file_path, 0, 0));
                }
                // 多段范围不实现：按完整文件返回（合法回退）。
                RangeSpec::Full => {}
            }
        }
        Ok(static_response(200, headers, &file_path, 0, size))
    }
}

/// 构造静态文件响应：只带文件元信息，正文由连接任务流式发送。
fn static_response(
    status: i64,
    headers: MapData,
    path: &std::path::Path,
    offset: u64,
    length: u64,
) -> Value {
    Value::ary(
        "http.response",
        vec![
            ("status".to_string(), Value::Int(status)),
            ("headers".to_string(), Value::map_data(headers)),
            ("body".to_string(), Value::string(String::new())),
            // 内部字段：连接任务按 offset/length 从磁盘读取，不占用解释器内存。
            (
                "_file".to_string(),
                Value::string(path.to_string_lossy().to_string()),
            ),
            ("_file_offset".to_string(), Value::Int(offset as i64)),
            ("_file_len".to_string(), Value::Int(length as i64)),
            ("json".to_string(), Value::None),
            ("sse".to_string(), Value::None),
        ],
    )
}

/// 429 响应：带 `Retry-After`，告诉客户端一秒后重试。
fn rate_limited_response() -> Value {
    let mut headers = MapData::default();
    headers.set(
        Value::string("Content-Type"),
        Value::string("text/plain; charset=utf-8"),
    );
    headers.set(Value::string("Retry-After"), Value::string("1"));
    Value::ary(
        "http.response",
        vec![
            ("status".to_string(), Value::Int(429)),
            ("headers".to_string(), Value::map_data(headers)),
            ("body".to_string(), Value::string("too many requests".to_string())),
            ("json".to_string(), Value::None),
            ("sse".to_string(), Value::None),
        ],
    )
}

/// 按 Unicode 字符数补位；`pad` 为空时按空格补。
fn pad_to(text: &str, width: usize, pad: &str, start: bool) -> String {
    let current = text.chars().count();
    if current >= width {
        return text.to_string();
    }
    let pad = if pad.is_empty() { " " } else { pad };
    let mut padding = String::new();
    while padding.chars().count() < width - current {
        padding.push_str(pad);
    }
    let padding: String = padding.chars().take(width - current).collect();
    if start {
        format!("{}{}", padding, text)
    } else {
        format!("{}{}", text, padding)
    }
}

fn int_result(result: Result<i64, String>) -> Value {
    match result {
        Ok(value) => Value::Ok(Box::new(Value::Int(value))),
        Err(message) => Value::Err(Box::new(Value::string(message))),
    }
}

fn text_result(result: Result<String, String>) -> Value {
    match result {
        Ok(value) => Value::Ok(Box::new(Value::string(value))),
        Err(message) => Value::Err(Box::new(Value::string(message))),
    }
}

/// 解析 IANA 时区名；大小写不敏感，返回具体时区规则（含夏令时）。
fn parse_timezone(name: &str) -> Result<chrono_tz::Tz, String> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Err("timezone name must not be empty".to_string());
    }
    if let Ok(zone) = trimmed.parse::<chrono_tz::Tz>() {
        return Ok(zone);
    }
    chrono_tz::TZ_VARIANTS
        .iter()
        .copied()
        .find(|zone| zone.name().eq_ignore_ascii_case(trimmed))
        .ok_or_else(|| format!("unknown timezone '{}'", name))
}

/// 系统本地时区名：优先 `TZ` 环境变量，再取操作系统设置。
fn local_zone_name() -> String {
    std::env::var("TZ")
        .ok()
        .filter(|value| !value.is_empty())
        .or_else(|| iana_time_zone::get_timezone().ok())
        .unwrap_or_default()
}

/// 解析不带时区的日期时间；允许只有日期的格式（补成当天 00:00）。
fn parse_naive(text: &str, pattern: &str) -> Result<chrono::NaiveDateTime, String> {
    if let Ok(naive) = chrono::NaiveDateTime::parse_from_str(text, pattern) {
        return Ok(naive);
    }
    match chrono::NaiveDate::parse_from_str(text, pattern) {
        Ok(date) => Ok(date.and_hms_opt(0, 0, 0).expect("midnight is valid")),
        Err(error) => Err(error.to_string()),
    }
}

/// 解析 `--key value` / `--key=value` / `-k value`，找不到返回 None。
fn cli_option(args: &[String], key: &str) -> Option<String> {
    let key = key.trim_start_matches('-');
    let with_value = format!("{}=", key);
    let long = format!("--{}", key);
    let short = format!("-{}", key);
    let mut index = 0;
    while index < args.len() {
        let arg = &args[index];
        if let Some(rest) = arg.strip_prefix("--") {
            if let Some(value) = rest.strip_prefix(&with_value) {
                return Some(value.to_string());
            }
        }
        if arg == &long || arg == &short {
            return args.get(index + 1).cloned();
        }
        index += 1;
    }
    None
}

/// 大小写不敏感地读取请求头。
fn request_header(request: &Value, name: &str) -> Option<String> {
    match request_field(request, "headers") {
        Some(Value::Map(map)) => {
            let map = map.borrow();
            for (key, value) in &map.entries {
                if key
                    .as_str()
                    .is_some_and(|text| text.eq_ignore_ascii_case(name))
                {
                    return value.as_str();
                }
            }
            None
        }
        _ => None,
    }
}

/// `If-None-Match` 匹配：支持逗号分隔列表、弱校验前缀 `W/` 与未加引号的 ETag。
fn etag_matches(header: &str, etag: &str) -> bool {
    let etag = etag.trim_matches('"');
    header.split(',').any(|candidate| {
        let candidate = candidate.trim();
        if candidate == "*" {
            return true;
        }
        candidate
            .trim_start_matches("W/")
            .trim_matches('"')
            == etag
    })
}

enum RangeSpec {
    /// 完整返回（无 Range、语法错误或多段范围）。
    Full,
    /// 闭区间 `start..=end`，已保证落在文件内。
    Partial(u64, u64),
    /// `start` 超出文件长度。
    Unsatisfiable,
}

/// 解析单段 `Range: bytes=...`。
fn parse_range(value: &str, size: u64) -> RangeSpec {
    let Some(list) = value.trim().strip_prefix("bytes=") else {
        return RangeSpec::Full;
    };
    // 多段范围用 multipart/byteranges 响应，这里回退为完整文件。
    if list.contains(',') {
        return RangeSpec::Full;
    }
    let Some((start_text, end_text)) = list.split_once('-') else {
        return RangeSpec::Full;
    };
    let (start_text, end_text) = (start_text.trim(), end_text.trim());
    if size == 0 {
        return RangeSpec::Unsatisfiable;
    }
    match (start_text.is_empty(), end_text.is_empty()) {
        // `bytes=-N`：最后 N 个字节。
        (true, false) => match end_text.parse::<u64>() {
            Ok(suffix) if suffix > 0 => {
                let start = size.saturating_sub(suffix);
                RangeSpec::Partial(start, size - 1)
            }
            _ => RangeSpec::Full,
        },
        // `bytes=N-`：从 N 到结尾。
        (false, true) => match start_text.parse::<u64>() {
            Ok(start) if start < size => RangeSpec::Partial(start, size - 1),
            Ok(_) => RangeSpec::Unsatisfiable,
            Err(_) => RangeSpec::Full,
        },
        (false, false) => match (start_text.parse::<u64>(), end_text.parse::<u64>()) {
            (Ok(start), Ok(end)) if start < size => {
                RangeSpec::Partial(start, end.min(size - 1).max(start))
            }
            (Ok(_), Ok(_)) => RangeSpec::Unsatisfiable,
            _ => RangeSpec::Full,
        },
        (true, true) => RangeSpec::Full,
    }
}

/// HTTP 日期格式（RFC 7231 IMF-fixdate）。
fn http_date(time: std::time::SystemTime) -> String {
    let datetime: chrono::DateTime<chrono::Utc> = time.into();
    datetime.format("%a, %d %b %Y %H:%M:%S GMT").to_string()
}

/// 静态资源缓存策略：HTML 每次协商，其余长缓存。
fn cache_control_for(path: &std::path::Path) -> &'static str {
    match path
        .extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| ext.to_ascii_lowercase())
        .as_deref()
    {
        Some("html") | Some("htm") => "no-cache",
        _ => "public, max-age=604800",
    }
}

#[cfg(feature = "interp")]
fn annotation_method(attribute: &str) -> Option<&'static str> {
    match attribute {
        "get" => Some("GET"),
        "post" => Some("POST"),
        "put" => Some("PUT"),
        "delete" => Some("DELETE"),
        "patch" => Some("PATCH"),
        "options" => Some("OPTIONS"),
        "head" => Some("HEAD"),
        _ => None,
    }
}

fn make_route(method: String, path: String, handler: Value) -> Value {
    Value::ary(
        "http.route",
        vec![
            ("method".to_string(), Value::string(method)),
            ("path".to_string(), Value::string(path)),
            ("handler".to_string(), handler),
        ],
    )
}

fn request_field(request: &Value, field: &str) -> Option<Value> {
    match request {
        Value::Ary(ary) => ary.borrow().get(field),
        _ => None,
    }
}

/// 从 `http.request` 的 headers 映射里按名字（不区分大小写）取头值。
fn request_header_ci(request: Option<&Value>, name: &str) -> Option<String> {
    let headers = request.and_then(|req| request_field(req, "headers"))?;
    match headers {
        Value::Map(map) => map.borrow().entries.iter().find_map(|(key, value)| {
            key.as_str()
                .filter(|item| item.eq_ignore_ascii_case(name))
                .and_then(|_| value.as_str())
        }),
        _ => None,
    }
}

fn csrf_token_from_request(request: &Value) -> Option<String> {
    if let Some(value) = request_field(request, "headers").and_then(|headers| match headers {
        Value::Map(map) => map
            .borrow()
            .get(&Value::string("x-csrf-token"))
            .and_then(|value| value.as_str()),
        _ => None,
    }) {
        return Some(value);
    }
    for field in ["form", "query"] {
        if let Some(value) = request_field(request, field).and_then(|map| match map {
            Value::Map(map) => map
                .borrow()
                .get(&Value::string("csrf_token"))
                .and_then(|value| value.as_str()),
            _ => None,
        }) {
            return Some(value);
        }
    }
    None
}

fn is_email(value: &str) -> bool {
    let Some((local, domain)) = value.split_once('@') else {
        return false;
    };
    !local.is_empty()
        && !domain.is_empty()
        && !value.contains(char::is_whitespace)
        && domain.contains('.')
        && !domain.starts_with('.')
        && !domain.ends_with('.')
}

/// 默认安全响应头：关闭嗅探、禁 iframe、限制引用与设备权限。
fn security_header_pairs() -> Vec<(String, String)> {
    vec![
        ("X-Content-Type-Options".to_string(), "nosniff".to_string()),
        ("X-Frame-Options".to_string(), "DENY".to_string()),
        ("Referrer-Policy".to_string(), "no-referrer".to_string()),
        (
            "Permissions-Policy".to_string(),
            "geolocation=(), microphone=(), camera=()".to_string(),
        ),
        (
            "Content-Security-Policy".to_string(),
            "default-src 'self'".to_string(),
        ),
        (
            "Strict-Transport-Security".to_string(),
            "max-age=31536000".to_string(),
        ),
    ]
}

const SESSION_COOKIE: &str = "rip_session";

fn hmac_bytes(secret: &str, data: &[u8]) -> Vec<u8> {
    let mut mac = <hmac::Hmac<sha2::Sha256> as hmac::Mac>::new_from_slice(secret.as_bytes())
        .expect("hmac accepts any key length");
    mac.update(data);
    mac.finalize().into_bytes().to_vec()
}

fn base64_url_encode(bytes: &[u8]) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

fn base64_url_decode(text: &str) -> Option<Vec<u8>> {
    base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(text.as_bytes())
        .ok()
}

/// 定长比较，避免签名校验被时序侧信道利用。
fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    let mut diff = 0u8;
    for (a, b) in left.iter().zip(right.iter()) {
        diff |= a ^ b;
    }
    diff == 0
}

/// 签发会话令牌：`过期毫秒.载荷base64.签名base64`。
fn sign_session(payload: &serde_json::Value, secret: &str, ttl_secs: i64) -> String {
    let body = serde_json::to_string(payload).unwrap_or_else(|_| "{}".to_string());
    let ttl_ms = if ttl_secs > 0 { ttl_secs * 1000 } else { 86_400_000 };
    let expires = now_ms() + ttl_ms;
    let unsigned = format!("{}.{}", expires, base64_url_encode(body.as_bytes()));
    let signature = base64_url_encode(&hmac_bytes(secret, unsigned.as_bytes()));
    format!("{}.{}", unsigned, signature)
}

fn verify_session(token: &str, secret: &str) -> Option<serde_json::Value> {
    let mut parts = token.split('.');
    let expires = parts.next()?;
    let payload = parts.next()?;
    let signature = parts.next()?;
    if parts.next().is_some() {
        return None;
    }
    let unsigned = format!("{}.{}", expires, payload);
    let expected = hmac_bytes(secret, unsigned.as_bytes());
    let provided = base64_url_decode(signature)?;
    if !constant_time_eq(&provided, &expected) {
        return None;
    }
    let expires: i64 = expires.parse().ok()?;
    if expires < now_ms() {
        return None;
    }
    let bytes = base64_url_decode(payload)?;
    serde_json::from_slice(&bytes).ok()
}

/// 会话 cookie 头：`ttl_secs` 同时决定 `Max-Age`（0 时默认一天）。
/// HTTPS 服务（`--cert` + `--key`）时追加 `Secure`，防止会话经明文信道泄露。
fn session_cookie_header(value: &str, ttl_secs: i64, secure: bool) -> String {
    let max_age = if ttl_secs > 0 { ttl_secs } else { 86_400 };
    let tail = if secure { "; Secure" } else { "" };
    format!(
        "{}={}; Path=/; HttpOnly; SameSite=Lax; Max-Age={}{}",
        SESSION_COOKIE, value, max_age, tail
    )
}

fn csrf_token(secret: &str) -> String {
    let mut nonce = [0u8; 16];
    if getrandom::getrandom(&mut nonce).is_err() {
        return String::new();
    }
    let text = base64_url_encode(&nonce);
    let signature = base64_url_encode(&hmac_bytes(secret, text.as_bytes()));
    format!("{}.{}", text, signature)
}

fn csrf_verify(token: &str, secret: &str) -> bool {
    match token.split_once('.') {
        Some((nonce, signature)) => {
            let expected = hmac_bytes(secret, nonce.as_bytes());
            match base64_url_decode(signature) {
                Some(provided) => constant_time_eq(&provided, &expected),
                None => false,
            }
        }
        None => false,
    }
}

fn percent_decode(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b'%' if i + 2 < bytes.len() => {
                match (hex_digit(bytes[i + 1]), hex_digit(bytes[i + 2])) {
                    (Some(high), Some(low)) => {
                        out.push(high * 16 + low);
                        i += 3;
                    }
                    _ => {
                        out.push(bytes[i]);
                        i += 1;
                    }
                }
            }
            byte => {
                out.push(byte);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).to_string()
}

/// multipart/form-data 的 boundary 参数。
fn multipart_boundary(content_type: &str) -> Option<String> {
    if !content_type.to_ascii_lowercase().contains("multipart/form-data") {
        return None;
    }
    for part in content_type.split(';').skip(1) {
        let part = part.trim();
        if let Some((key, value)) = part.split_once('=') {
            if key.trim().eq_ignore_ascii_case("boundary") {
                return Some(value.trim().trim_matches('"').to_string());
            }
        }
    }
    None
}

struct MultipartPart {
    name: String,
    filename: Option<String>,
    content_type: String,
    data: Vec<u8>,
}

/// multipart 正文与分段数量的硬上限：防止单请求吃满内存或生成海量小段。
const MAX_MULTIPART_BYTES: usize = 32 * 1024 * 1024;
const MAX_MULTIPART_PARTS: usize = 1024;

/// 按字节解析 multipart 正文，保留文件的原始二进制内容。
///
/// 定界符用两路（Two-Way）子串查找，最坏也是 O(n)：逐字节比较会在长 boundary
/// 的恶意请求体上退化成 O(n × m)，把工作线程占死。
fn parse_multipart(raw: &[u8], boundary: &str) -> Vec<MultipartPart> {
    let mut parts = Vec::new();
    // RFC 2046 规定 boundary 最长 70 字符；超出或为空都按非法正文处理。
    if boundary.is_empty() || boundary.len() > 70 || raw.len() > MAX_MULTIPART_BYTES {
        return parts;
    }
    let delimiter = format!("--{}", boundary).into_bytes();
    let positions: Vec<usize> = memchr::memmem::find_iter(raw, delimiter.as_slice()).collect();
    for (order, start) in positions.iter().enumerate() {
        if order >= MAX_MULTIPART_PARTS {
            break;
        }
        let mut cursor = start + delimiter.len();
        if raw.get(cursor) == Some(&b'-') && raw.get(cursor + 1) == Some(&b'-') {
            break;
        }
        if raw.get(cursor) == Some(&b'\r') {
            cursor += 1;
        }
        if raw.get(cursor) == Some(&b'\n') {
            cursor += 1;
        }
        let end = match positions.get(order + 1) {
            Some(next) => *next,
            None => break,
        };
        // 结束位置前的 CRLF 属于分隔符，不属于内容。
        let stop = if end >= 2 && &raw[end - 2..end] == b"\r\n" {
            end - 2
        } else {
            end
        };
        if cursor >= stop {
            continue;
        }
        let header_end = match find_bytes(&raw[cursor..stop], b"\r\n\r\n") {
            Some(offset) => cursor + offset,
            None => continue,
        };
        let headers = String::from_utf8_lossy(&raw[cursor..header_end]).to_string();
        let data_start = header_end + 4;
        let mut name = String::new();
        let mut filename = None;
        let mut content_type = String::new();
        for line in headers.lines() {
            let line = line.trim();
            if let Some(rest) = line
                .strip_prefix("Content-Disposition:")
                .or_else(|| line.strip_prefix("content-disposition:"))
            {
                for piece in rest.split(';').skip(1) {
                    let piece = piece.trim();
                    if let Some(value) = piece.strip_prefix("name=") {
                        name = value.trim().trim_matches('"').to_string();
                    } else if let Some(value) = piece.strip_prefix("filename=") {
                        filename = Some(value.trim().trim_matches('"').to_string());
                    }
                }
            } else if let Some(value) = line
                .strip_prefix("Content-Type:")
                .or_else(|| line.strip_prefix("content-type:"))
            {
                content_type = value.trim().to_string();
            }
        }
        parts.push(MultipartPart {
            name,
            filename,
            content_type,
            data: raw[data_start.min(stop)..stop].to_vec(),
        });
    }
    parts
}

fn find_bytes(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    (0..=haystack.len() - needle.len())
        .find(|index| &haystack[*index..*index + needle.len()] == needle)
}

/// 上传文件名只保留基本字符，防止路径穿越。
fn safe_file_name(name: &str) -> String {
    let base = name
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(name)
        .trim();
    let cleaned: String = base
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '.' || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    cleaned.trim_start_matches('.').to_string()
}

fn base64_encode(bytes: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

fn base64_decode(text: &str) -> Option<Vec<u8>> {
    base64::engine::general_purpose::STANDARD
        .decode(text.trim())
        .ok()
}

fn hex_lower(bytes: &[u8]) -> String {
    let mut text = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        text.push_str(&format!("{:02x}", byte));
    }
    text
}

fn hex_decode(text: &str) -> Option<Vec<u8>> {
    let trimmed = text.trim();
    if trimmed.len() % 2 != 0 {
        return None;
    }
    let mut bytes = Vec::with_capacity(trimmed.len() / 2);
    let chars: Vec<u8> = trimmed.bytes().collect();
    let mut index = 0;
    while index < chars.len() {
        let high = hex_digit(chars[index])?;
        let low = hex_digit(chars[index + 1])?;
        bytes.push(high * 16 + low);
        index += 2;
    }
    Some(bytes)
}

/// AES 密钥派生（旧格式）：任意字符串经 SHA-256 得到 32 字节密钥。
/// 仅用于解密历史密文；新加密一律走 `derive_key_v2`。
fn derive_key(key: &str) -> [u8; 32] {
    let digest = sha2::Sha256::digest(key.as_bytes());
    let mut bytes = [0u8; 32];
    bytes.copy_from_slice(&digest);
    bytes
}

/// 新密文格式前缀：`RIPV2: || 盐(16) || nonce(12) || 密文+标签`，再整体 base64。
const AES_V2_PREFIX: &[u8] = b"RIPV2:";
/// 派生迭代次数：与口令哈希同量级，抵御低熵密钥的离线爆破。
const AES_KEY_ROUNDS: u32 = 210_000;

/// AES 密钥派生（V2）：PBKDF2-HMAC-SHA256(密钥, 随机盐, 迭代) —— 无盐单轮 SHA-256 可被
/// 预计算/快速爆破，低熵口令场景不安全。
fn derive_key_v2(key: &str, salt: &[u8]) -> [u8; 32] {
    pbkdf2_sha256(key.as_bytes(), salt, AES_KEY_ROUNDS)
}

/// 接受 PKCS#8 或 PKCS#1 的 RSA 私钥 PEM。
fn parse_private_key(pem: &str) -> Result<RsaPrivateKey, String> {
    if let Ok(key) = RsaPrivateKey::from_pkcs8_pem(pem) {
        return Ok(key);
    }
    RsaPrivateKey::from_pkcs1_pem(pem)
        .map_err(|error| format!("invalid RSA private key: {}", error))
}

/// 接受 SPKI 或 PKCS#1 的 RSA 公钥 PEM。
fn parse_public_key(pem: &str) -> Result<RsaPublicKey, String> {
    if let Ok(key) = RsaPublicKey::from_public_key_pem(pem) {
        return Ok(key);
    }
    RsaPublicKey::from_pkcs1_pem(pem)
        .map_err(|error| format!("invalid RSA public key: {}", error))
}

/// PBKDF2-HMAC-SHA256 的迭代次数：OWASP 对 SHA-256 的建议量级。
const PASSWORD_ROUNDS: u32 = 210_000;
/// 校验时允许的最大迭代次数：防止伪造的哈希串把校验变成 CPU 炸弹。
const PASSWORD_MAX_ROUNDS: u32 = 2_000_000;

/// 密码哈希格式：`ript$迭代次数$盐$摘要`，摘要 = PBKDF2-HMAC-SHA256(密码, 盐, 迭代次数)。
fn password_hash(password: &str) -> Result<String, String> {
    let mut salt = [0u8; 16];
    getrandom::getrandom(&mut salt)
        .map_err(|error| format!("cannot read system random source: {}", error))?;
    let digest = pbkdf2_sha256(password.as_bytes(), &salt, PASSWORD_ROUNDS);
    Ok(format!(
        "ript${}${}${}",
        PASSWORD_ROUNDS,
        hex_lower(&salt),
        hex_lower(&digest)
    ))
}

fn password_verify(password: &str, hashed: &str) -> bool {
    let mut parts = hashed.split('$');
    if parts.next() != Some("ript") {
        return false;
    }
    let rounds: u32 = match parts.next().and_then(|value| value.parse().ok()) {
        Some(value) => value,
        None => return false,
    };
    if rounds == 0 || rounds > PASSWORD_MAX_ROUNDS {
        return false;
    }
    let salt = match parts.next().and_then(hex_decode) {
        Some(bytes) => bytes,
        None => return false,
    };
    let expected = match parts.next() {
        Some(text) => text,
        None => return false,
    };
    let actual = hex_lower(&pbkdf2_sha256(password.as_bytes(), &salt, rounds));
    // 定长比较，避免提前返回泄露差异位置。
    if actual.len() != expected.len() {
        return false;
    }
    let mut diff = 0u8;
    for (a, b) in actual.bytes().zip(expected.bytes()) {
        diff |= a ^ b;
    }
    diff == 0
}

/// PBKDF2-HMAC-SHA256（RFC 8018），输出正好一个 32 字节块。
///
/// HMAC 的 ipad / opad 只与口令有关：预先算好前缀状态，每轮只做两次压缩，
/// 比每轮重新构造 HMAC 快一倍左右。
fn pbkdf2_sha256(password: &[u8], salt: &[u8], rounds: u32) -> [u8; 32] {
    let mut key = [0u8; 64];
    if password.len() > 64 {
        let mut hasher = sha2::Sha256::new();
        hasher.update(password);
        key[..32].copy_from_slice(&hasher.finalize());
    } else {
        key[..password.len()].copy_from_slice(password);
    }
    let mut inner_pad = [0x36u8; 64];
    let mut outer_pad = [0x5cu8; 64];
    for index in 0..64 {
        inner_pad[index] ^= key[index];
        outer_pad[index] ^= key[index];
    }
    let inner_base = {
        let mut hasher = sha2::Sha256::new();
        hasher.update(inner_pad);
        hasher
    };
    let outer_base = {
        let mut hasher = sha2::Sha256::new();
        hasher.update(outer_pad);
        hasher
    };
    let compute = |data: &[u8]| -> [u8; 32] {
        let mut inner = inner_base.clone();
        inner.update(data);
        let inner_hash = inner.finalize();
        let mut outer = outer_base.clone();
        outer.update(&inner_hash);
        let mut digest = [0u8; 32];
        digest.copy_from_slice(&outer.finalize());
        digest
    };
    // U1 = HMAC(密码, 盐 || 块序号)；后续 Ui = HMAC(密码, U(i-1))，异或累加。
    let mut seed = Vec::with_capacity(salt.len() + 4);
    seed.extend_from_slice(salt);
    seed.extend_from_slice(&1u32.to_be_bytes());
    let mut block = compute(&seed);
    let mut out = block;
    for _ in 1..rounds {
        block = compute(&block);
        for (target, source) in out.iter_mut().zip(block.iter()) {
            *target ^= *source;
        }
    }
    out
}

fn hex_digit(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

/// 已解析的请求：只保留交给 `http.request` 的原始数据。
pub(crate) struct HttpRequestHead {
    pub method: String,
    pub path: String,
    pub target: String,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
    pub keep_alive: bool,
}

enum ReadOutcome {
    Request(Box<HttpRequestHead>),
    Closed,
    TooLarge,
    BadRequest(String),
}

enum LineRead {
    Line(String),
    Eof,
    Timeout,
    TooLong,
}

/// 带整体期限的读取包装。
///
/// socket 的读超时只限制单次 `read`：慢速客户端每 29 秒发一个字节就能一直占住
/// 工作线程。这里在每次读之前再检查一次请求的总期限，到时按超时处理。
struct DeadlineReader<'a, R: BufRead> {
    inner: &'a mut R,
    deadline: Instant,
}

impl<R: BufRead> DeadlineReader<'_, R> {
    fn check(&self) -> std::io::Result<()> {
        if Instant::now() >= self.deadline {
            return Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "request deadline exceeded",
            ));
        }
        Ok(())
    }
}

impl<R: BufRead> Read for DeadlineReader<'_, R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.check()?;
        self.inner.read(buf)
    }
}

impl<R: BufRead> BufRead for DeadlineReader<'_, R> {
    fn fill_buf(&mut self) -> std::io::Result<&[u8]> {
        self.check()?;
        self.inner.fill_buf()
    }

    fn consume(&mut self, amount: usize) {
        self.inner.consume(amount);
    }
}

/// 读取一行，带长度上限；超时按空闲断开处理。
fn read_line_limited(reader: &mut impl BufRead, limit: usize) -> LineRead {
    let mut buf: Vec<u8> = Vec::new();
    let mut byte = [0u8; 1];
    loop {
        match reader.read(&mut byte) {
            Ok(0) => {
                if buf.is_empty() {
                    return LineRead::Eof;
                }
                return LineRead::Line(String::from_utf8_lossy(&buf).to_string());
            }
            Ok(_) => {
                if byte[0] == b'\n' {
                    return LineRead::Line(String::from_utf8_lossy(&buf).to_string());
                }
                buf.push(byte[0]);
                if buf.len() > limit {
                    return LineRead::TooLong;
                }
            }
            Err(error) => {
                return match error.kind() {
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut => {
                        LineRead::Timeout
                    }
                    _ => LineRead::Eof,
                }
            }
        }
    }
}

/// 读取分块传输的请求体（`Transfer-Encoding: chunked`）。
fn read_chunked(reader: &mut impl BufRead, max: usize) -> Option<Vec<u8>> {
    let mut out: Vec<u8> = Vec::new();
    loop {
        let line = match read_line_limited(reader, 1024) {
            LineRead::Line(line) => line,
            _ => return None,
        };
        let size_text = line.trim().split(';').next().unwrap_or("").trim();
        let size = usize::from_str_radix(size_text, 16).ok()?;
        if size == 0 {
            loop {
                match read_line_limited(reader, 8 * 1024) {
                    LineRead::Line(trailer) if trailer.trim().is_empty() => break,
                    LineRead::Line(_) => continue,
                    _ => break,
                }
            }
            return Some(out);
        }
        // 长度用 checked_add：分块长度由客户端给出，直接相加会溢出后绕过上限。
        let total = match out.len().checked_add(size) {
            Some(total) if total <= max => total,
            _ => return None,
        };
        let mut chunk = vec![0u8; size];
        reader.read_exact(&mut chunk).ok()?;
        out.extend_from_slice(&chunk);
        debug_assert_eq!(out.len(), total);
        let _ = read_line_limited(reader, 8);
    }
}

/// 头部名是否合法（RFC 7230 token）。
pub(crate) fn is_header_name(name: &str) -> bool {
    !name.is_empty()
        && name.bytes().all(|byte| {
            byte.is_ascii_alphanumeric()
                || matches!(
                    byte,
                    b'!' | b'#'
                        | b'$'
                        | b'%'
                        | b'&'
                        | b'\''
                        | b'*'
                        | b'+'
                        | b'-'
                        | b'.'
                        | b'^'
                        | b'_'
                        | b'`'
                        | b'|'
                        | b'~'
                )
        })
}

/// 头部值不能含 CR/LF/NUL，否则可注入额外的响应头或拆分响应。
pub(crate) fn is_header_value(value: &str) -> bool {
    !value.bytes().any(|byte| byte == b'\r' || byte == b'\n' || byte == 0)
}

/// hop-by-hop 与服务器自己写入的头部，应用不能覆盖。
pub(crate) fn is_reserved_header(name: &str) -> bool {
    const RESERVED: [&str; 9] = [
        "content-length",
        "connection",
        "transfer-encoding",
        "keep-alive",
        "upgrade",
        "trailer",
        "proxy-connection",
        "te",
        "x-request-id",
    ];
    RESERVED.iter().any(|item| name.eq_ignore_ascii_case(item))
}

/// 解析 Content-Length：必须唯一（或全部相同）且为纯十进制数字。
pub(crate) fn parse_content_length(headers: &[(String, String)]) -> Result<Option<usize>, String> {
    let mut seen: Option<usize> = None;
    for (key, value) in headers {
        if !key.eq_ignore_ascii_case("content-length") {
            continue;
        }
        let text = value.trim();
        if text.is_empty() || !text.bytes().all(|byte| byte.is_ascii_digit()) {
            return Err("invalid Content-Length".to_string());
        }
        let parsed: usize = text
            .parse()
            .map_err(|_| "invalid Content-Length".to_string())?;
        match seen {
            Some(existing) if existing != parsed => {
                return Err("conflicting Content-Length".to_string())
            }
            _ => seen = Some(parsed),
        }
    }
    Ok(seen)
}

/// 解析 Transfer-Encoding：只接受 `chunked`（前面可出现 `identity`）。
pub(crate) fn parse_transfer_encoding(headers: &[(String, String)]) -> Result<bool, String> {
    let mut tokens: Vec<String> = Vec::new();
    for (key, value) in headers {
        if !key.eq_ignore_ascii_case("transfer-encoding") {
            continue;
        }
        for part in value.split(',') {
            tokens.push(part.trim().to_ascii_lowercase());
        }
    }
    if tokens.is_empty() {
        return Ok(false);
    }
    if tokens.last().map(|token| token.as_str()) != Some("chunked") {
        return Err("unsupported Transfer-Encoding".to_string());
    }
    if tokens[..tokens.len() - 1]
        .iter()
        .any(|token| token != "identity")
    {
        return Err("unsupported Transfer-Encoding".to_string());
    }
    Ok(true)
}

fn read_request(reader: &mut impl BufRead, config: &crate::server::ServerConfig) -> ReadOutcome {
    let request_line = match read_line_limited(reader, 8 * 1024) {
        LineRead::Line(line) => line,
        LineRead::Eof | LineRead::Timeout => return ReadOutcome::Closed,
        LineRead::TooLong => return ReadOutcome::BadRequest("request line too long".to_string()),
    };
    if request_line.trim().is_empty() {
        return ReadOutcome::Closed;
    }
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or("GET").to_string();
    let target = parts.next().unwrap_or("/").to_string();
    let version = parts.next().unwrap_or("HTTP/1.1").to_string();
    let (path, _) = match target.split_once('?') {
        Some((path, query)) => (path.to_string(), query.to_string()),
        None => (target.clone(), String::new()),
    };

    let mut headers: Vec<(String, String)> = Vec::new();
    let mut header_bytes = 0usize;
    loop {
        let line = match read_line_limited(reader, config.max_header_bytes) {
            LineRead::Line(line) => line,
            LineRead::Eof | LineRead::Timeout => {
                return ReadOutcome::BadRequest("unexpected end of headers".to_string())
            }
            LineRead::TooLong => return ReadOutcome::TooLarge,
        };
        header_bytes += line.len();
        if header_bytes > config.max_header_bytes {
            return ReadOutcome::TooLarge;
        }
        let trimmed = line.trim_end();
        if trimmed.is_empty() {
            break;
        }
        if headers.len() >= config.max_headers {
            return ReadOutcome::TooLarge;
        }
        // RFC 7230：头名必须是合法 token（冒号前不允许空白），无冒号的头行（含 obs-fold 续行）直接拒绝，
        // 避免与严格的前置代理对同一请求定界不一致（请求走私面）。
        let Some((key, value)) = trimmed.split_once(':') else {
            return ReadOutcome::BadRequest("malformed header line".to_string());
        };
        if !is_header_name(key) {
            return ReadOutcome::BadRequest("invalid header name".to_string());
        }
        headers.push((key.to_string(), value.trim().to_string()));
    }

    // 请求走私防护：CL 必须唯一且合法；TE 只接受 chunked；两者不允许同时出现。
    let chunked = match parse_transfer_encoding(&headers) {
        Ok(value) => value,
        Err(message) => return ReadOutcome::BadRequest(message),
    };
    if chunked && version.eq_ignore_ascii_case("HTTP/1.0") {
        return ReadOutcome::BadRequest(
            "Transfer-Encoding is not allowed in HTTP/1.0".to_string(),
        );
    }
    let length = match parse_content_length(&headers) {
        Ok(value) => value,
        Err(message) => return ReadOutcome::BadRequest(message),
    };
    if chunked && length.is_some() {
        return ReadOutcome::BadRequest(
            "both Content-Length and Transfer-Encoding are present".to_string(),
        );
    }
    let body = if chunked {
        match read_chunked(reader, config.max_body_bytes) {
            Some(body) => body,
            None => return ReadOutcome::TooLarge,
        }
    } else {
        let length = length.unwrap_or(0);
        if length > config.max_body_bytes {
            return ReadOutcome::TooLarge;
        }
        let mut body = vec![0u8; length];
        if length > 0 && reader.read_exact(&mut body).is_err() {
            return ReadOutcome::BadRequest("incomplete request body".to_string());
        }
        body
    };

    let connection = headers
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case("connection"))
        .map(|(_, value)| value.to_ascii_lowercase())
        .unwrap_or_default();
    let keep_alive = if connection.contains("close") {
        false
    } else if version.eq_ignore_ascii_case("HTTP/1.0") {
        connection.contains("keep-alive")
    } else {
        true
    };

    ReadOutcome::Request(Box::new(HttpRequestHead {
        method,
        path,
        target,
        headers,
        body,
        keep_alive,
    }))
}

/// 把请求数据转换成 `http.request`。
pub(crate) fn build_request(head: HttpRequestHead, client_ip: String, request_id: &str) -> Value {
    let HttpRequestHead {
        method,
        path,
        target,
        headers,
        body: raw_body,
        ..
    } = head;
    // multipart/form-data：解析出上传文件（保持二进制内容），表单字段并入 form。
    let content_type = headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("content-type"))
        .map(|(_, v)| v.clone())
        .unwrap_or_default();
    let cookie_header = headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("cookie"))
        .map(|(_, v)| v.clone());
    // 请求头直接移动进 header_map：避免每请求 2×头部数的 String 克隆。
    let mut header_map = MapData::default();
    for (k, v) in headers {
        header_map.set(Value::string(k), Value::string(v));
    }
    let mut files: Vec<Value> = Vec::new();
    let mut multipart_fields: Vec<(String, String)> = Vec::new();
    if let Some(boundary) = multipart_boundary(&content_type) {
        for part in parse_multipart(&raw_body, &boundary) {
            match part.filename {
                Some(filename) => files.push(Value::ary(
                    "http.file",
                    vec![
                        ("field".to_string(), Value::string(part.name)),
                        ("filename".to_string(), Value::string(filename)),
                        ("content_type".to_string(), Value::string(part.content_type)),
                        ("size".to_string(), Value::Int(part.data.len() as i64)),
                        (
                            "data_base64".to_string(),
                            Value::string(base64_encode(&part.data)),
                        ),
                    ],
                )),
                None => multipart_fields.push((
                    part.name,
                    String::from_utf8_lossy(&part.data).to_string(),
                )),
            }
        }
    }
    let body = String::from_utf8_lossy(&raw_body).to_string();

    let query_string = target
        .split_once('?')
        .map(|(_, query)| query.to_string())
        .unwrap_or_default();
    let mut query = MapData::default();
    if !query_string.is_empty() {
        for pair in query_string.split('&') {
            if let Some((k, v)) = pair.split_once('=') {
                query.set(
                    Value::string(percent_decode(k)),
                    Value::string(percent_decode(v)),
                );
            }
        }
    }
    let mut cookies = MapData::default();
    if let Some(cookie_header) = &cookie_header {
        for cookie in cookie_header.split(';') {
            if let Some((k, v)) = cookie.trim().split_once('=') {
                cookies.set(Value::string(k.trim()), Value::string(v.trim()));
            }
        }
    }
    let mut form = MapData::default();
    let is_form = content_type
        .to_ascii_lowercase()
        .contains("application/x-www-form-urlencoded");
    if is_form {
        for pair in body.split('&') {
            if let Some((k, v)) = pair.split_once('=') {
                form.set(
                    Value::string(percent_decode(k)),
                    Value::string(percent_decode(v)),
                );
            }
        }
    }
    for (key, value) in multipart_fields {
        form.set(Value::string(key), Value::string(value));
    }
    let trimmed_body = body.trim_start();
    let json = if !body.is_empty() && (trimmed_body.starts_with('{') || trimmed_body.starts_with('['))
    {
        serde_json::from_str::<serde_json::Value>(&body)
            .ok()
            .map(|v| json_to_value(&v))
    } else {
        None
    };
    Value::ary(
        "http.request",
        vec![
            ("method".to_string(), Value::string(method)),
            ("path".to_string(), Value::string(path)),
            (
                "params".to_string(),
                Value::map_data(MapData::default()),
            ),
            (
                "query".to_string(),
                Value::map_data(query),
            ),
            (
                "headers".to_string(),
                Value::map_data(header_map),
            ),
            ("body".to_string(), Value::string(body)),
            (
                "json".to_string(),
                match json {
                    Some(value) => Value::Some(Box::new(value)),
                    None => Value::None,
                },
            ),
            (
                "form".to_string(),
                Value::map_data(form),
            ),
            (
                "cookies".to_string(),
                Value::map_data(cookies),
            ),
            ("client_ip".to_string(), Value::string(client_ip)),
            ("files".to_string(), Value::vec(files)),
            ("request_id".to_string(), Value::string(request_id.to_string())),
            // 请求级上下文：中间件写入，后续处理函数读取；随请求结束丢弃。
            ("ctx".to_string(), Value::empty_map()),
        ],
    )
}

/// 从路由表生成 OpenAPI 3.0 JSON。
///
/// 只输出路径、方法、`operationId`（函数名）与路径参数；
/// 请求/响应模型由业务自己维护，不在这里猜测。
fn openapi_json(handler: &Value, title: &str, version: &str) -> String {
    let mut routes: Vec<(String, String, String)> = Vec::new();
    collect_openapi_routes(handler, "", &mut routes);

    let mut paths = serde_json::Map::new();
    for (method, path, operation) in routes {
        let key = method.to_ascii_lowercase();
        let entry = paths
            .entry(path.clone())
            .or_insert_with(|| serde_json::Value::Object(serde_json::Map::new()));
        let serde_json::Value::Object(methods) = entry else {
            continue;
        };
        let mut spec = serde_json::json!({
            "summary": if operation.is_empty() { path.clone() } else { operation.clone() },
            "responses": { "200": { "description": "OK" } }
        });
        if !operation.is_empty() {
            spec["operationId"] = serde_json::Value::from(operation);
        }
        let params: Vec<serde_json::Value> = path
            .split('/')
            .filter_map(|segment| {
                let name = segment.strip_prefix('{')?.strip_suffix('}')?;
                Some(serde_json::json!({
                    "name": name,
                    "in": "path",
                    "required": true,
                    "schema": { "type": "string" }
                }))
            })
            .collect();
        if !params.is_empty() {
            spec["parameters"] = serde_json::Value::Array(params);
        }
        methods.insert(key, spec);
    }

    let document = serde_json::json!({
        "openapi": "3.0.0",
        "info": { "title": title, "version": version },
        "paths": serde_json::Value::Object(paths)
    });
    serde_json::to_string_pretty(&document).unwrap_or_else(|_| "{}".to_string())
}

/// 递归收集路由：解开中间件链与路由分组，跳过静态资源等通配路由。
fn collect_openapi_routes(handler: &Value, prefix: &str, out: &mut Vec<(String, String, String)>) {
    let Value::Func(func) = handler else {
        return;
    };
    match &func.kind {
        FuncKind::Router(table) => {
            for route in table.routes() {
                // `*` 前缀路由是分组挂载或静态资源：挂载继续下钻（前缀由 Mounted 自己带），静态跳过。
                if route.method == "*" {
                    if let Value::Func(inner) = &route.handler {
                        if matches!(inner.kind, FuncKind::Mounted { .. } | FuncKind::Chain { .. }) {
                            collect_openapi_routes(&route.handler, prefix, out);
                        }
                    }
                    continue;
                }
                let path = openapi_path(&join_paths(prefix, &route.path));
                let operation = match &route.handler {
                    Value::Func(func) => func.name.trim_matches(['<', '>']).to_string(),
                    _ => String::new(),
                };
                out.push((route.method.clone(), path, operation));
            }
        }
        FuncKind::Mounted { prefix: mount, inner } => {
            collect_openapi_routes(inner, &join_paths(prefix, mount), out);
        }
        FuncKind::Chain { handler, .. } => collect_openapi_routes(handler, prefix, out),
        _ => {}
    }
}

/// 拼接两段路径，保证以 `/` 开头且不出现重复斜杠。
fn join_paths(base: &str, tail: &str) -> String {
    let base = base.trim_end_matches('/');
    let tail = tail.trim_start_matches('/');
    match (base.is_empty(), tail.is_empty()) {
        (true, true) => "/".to_string(),
        (true, false) => format!("/{}", tail),
        (false, true) => base.to_string(),
        (false, false) => format!("{}/{}", base, tail),
    }
}

/// 把 Rip 的 `:param` / `*` 转成 OpenAPI 的 `{param}` 形式。
fn openapi_path(path: &str) -> String {
    let mut out = String::with_capacity(path.len() + 8);
    for (index, segment) in path.split('/').enumerate() {
        if index > 0 {
            out.push('/');
        }
        if let Some(name) = segment.strip_prefix(':') {
            out.push('{');
            out.push_str(name);
            out.push('}');
        } else if segment == "*" {
            out.push_str("{path}");
        } else {
            out.push_str(segment);
        }
    }
    if out.is_empty() {
        "/".to_string()
    } else {
        out
    }
}

/// 把 `Value::Func` 转成可跨线程的处理函数描述。
fn handler_spec(handler: &Value) -> Option<crate::server::HandlerSpec> {
    use crate::server::{HandlerSpec, RouteSpec};
    let func = match handler {
        Value::Func(func) => func,
        _ => return None,
    };
    match &func.kind {
        FuncKind::Decl(_) => Some(HandlerSpec::Func(func.name.clone())),
        FuncKind::Static { dir, prefix } => Some(HandlerSpec::Static {
            dir: dir.clone(),
            prefix: prefix.clone(),
        }),
        FuncKind::Middleware(spec) => Some(HandlerSpec::Middleware(spec.clone())),
        FuncKind::Chain {
            middlewares,
            handler,
            ..
        } => {
            let mut specs = Vec::with_capacity(middlewares.len());
            for middleware in middlewares {
                specs.push(handler_spec(middleware)?);
            }
            Some(HandlerSpec::Chain {
                middlewares: specs,
                handler: Box::new(handler_spec(handler)?),
            })
        }
        FuncKind::Mounted { prefix, inner } => Some(HandlerSpec::Mounted {
            prefix: prefix.clone(),
            inner: Box::new(handler_spec(inner)?),
        }),
        // 组合后的中间件只应出现在链内部，单独作为路由处理器无法跨线程重建。
        FuncKind::MiddlewareApplied { .. } => None,
        // SSE 发送器由服务端按请求注入，不能作为处理器。
        FuncKind::SseSend(_) => None,
        FuncKind::Router(table) => {
            let mut out = Vec::with_capacity(table.routes().len());
            for route in table.routes() {
                out.push(RouteSpec {
                    method: route.method.clone(),
                    path: route.path.clone(),
                    handler: handler_spec(&route.handler)?,
                });
            }
            Some(HandlerSpec::Router(out))
        }
        // 闭包捕获了线程内的环境，不能跨线程重建。
        FuncKind::Closure { .. } => None,
        // AOT 顶层函数按名字在原生函数表里重建；匿名闭包同样不能跨线程。
        FuncKind::Native { .. } if func.name != "<closure>" => {
            Some(HandlerSpec::Func(func.name.clone()))
        }
        FuncKind::Native { .. } => None,
    }
}

/// 请求 ID：毫秒时间戳 + 进程内自增序号。
pub(crate) fn next_request_id() -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(1);
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0);
    format!("req-{:x}-{:x}", stamp, COUNTER.fetch_add(1, Ordering::Relaxed))
}

/// 指标响应：Prometheus 文本格式。
fn metrics_response(text: &str) -> Value {
    let mut headers = MapData::default();
    headers.set(
        Value::string("Content-Type"),
        Value::string("text/plain; version=0.0.4; charset=utf-8"),
    );
    Value::ary(
        "http.response",
        vec![
            ("status".to_string(), Value::Int(200)),
            (
                "headers".to_string(),
                Value::map_data(headers),
            ),
            ("body".to_string(), Value::string(text.to_string())),
            ("json".to_string(), Value::None),
            ("sse".to_string(), Value::None),
        ],
    )
}

/// 队列已满：不进入工作线程，直接回 503 并关闭连接。
fn write_rejection(mut stream: TcpStream) -> std::io::Result<()> {
    let _ = stream.set_write_timeout(Some(Duration::from_secs(2)));
    let body = "server is overloaded";
    // Content-Length 必须与 body 实际字节数一致，否则客户端会截断或挂起。
    let response = format!(
        "HTTP/1.1 503 Service Unavailable\r\nContent-Length: {}\r\nConnection: close\r\nRetry-After: 1\r\nContent-Type: text/plain; charset=utf-8\r\n\r\n{}",
        body.len(),
        body
    );
    stream.write_all(response.as_bytes())?;
    stream.flush()
}

/// 处理函数的返回值（可跨线程传回连接任务）。
pub(crate) struct ResponseParts {
    pub status: i64,
    pub headers: Vec<(String, String)>,
    /// 原始响应字节：静态文件等二进制内容不能被转成字符串。
    pub body: Vec<u8>,
    /// 静态文件：从磁盘流式发送，避免大文件整块进内存。
    pub file: Option<FilePart>,
    /// SSE 流式响应：响应长度未知（以关闭连接结束），body 为空、逐帧直接写 socket。
    pub streaming: bool,
}

pub(crate) struct FilePart {
    pub path: std::path::PathBuf,
    pub offset: u64,
    pub length: u64,
}

impl ResponseParts {
    /// 实际要发送的字节数（文件或内存体；流式响应没有长度）。
    pub fn content_length(&self) -> u64 {
        match &self.file {
            Some(file) => file.length,
            None => self.body.len() as u64,
        }
    }

    /// 是否为流式响应：此时不能写 `Content-Length`，也不能复用连接。
    pub fn is_stream(&self) -> bool {
        self.streaming
    }
}

/// 取出响应里的 SSE 生产函数（`sse: option<func(...)>`）。
pub(crate) fn take_sse(response: &Value) -> Option<Value> {
    let ary = match response {
        Value::Ary(ary) => ary,
        _ => return None,
    };
    let value = ary.borrow().get("sse")?;
    match value {
        Value::Some(inner) | Value::Ok(inner) if matches!(*inner, Value::Func(_)) => {
            Some((*inner).clone())
        }
        Value::Func(_) => Some(value),
        _ => None,
    }
}

/// SSE 响应：沿用处理函数给出的状态码与响应头，补上事件流所需的默认头。
pub(crate) fn into_sse_parts(mut parts: ResponseParts) -> ResponseParts {
    parts.body.clear();
    parts.file = None;
    if header_value(&parts.headers, "content-type").is_none() {
        parts.headers.push((
            "Content-Type".to_string(),
            "text/event-stream; charset=utf-8".to_string(),
        ));
    }
    if header_value(&parts.headers, "cache-control").is_none() {
        parts
            .headers
            .push(("Cache-Control".to_string(), "no-cache".to_string()));
    }
    // 反向代理（Nginx）默认缓冲响应，SSE 必须显式关闭。
    parts
        .headers
        .push(("X-Accel-Buffering".to_string(), "no".to_string()));
    parts.streaming = true;
    parts
}

/// 运行 SSE 生产函数：事件经 `sink` 直接写入连接。
///
/// 生产函数运行在当前解释器线程上（`Value` 不可跨线程），
/// 因此一条 SSE 连接会占用一个工作线程，直到生产函数返回或客户端断开。
pub(crate) fn run_sse_producer(
    runtime: &mut Runtime,
    producer: Value,
    sink: Rc<crate::value::SseSink>,
    request_id: &str,
) {
    // 长连接不受 request_timeout 约束：由生产函数返回或客户端断开决定结束。
    runtime.clear_deadline();
    let send = Value::func_rc(Rc::new(Function {
        name: "<sse>".to_string(),
        kind: FuncKind::SseSend(sink),
    }));
    let func = match producer {
        Value::Func(func) => func,
        _ => return,
    };
    if let Err(Flow::Error(error)) = runtime.call_value(&func, vec![send], 0) {
        // 客户端断开是正常结束，不写日志。
        if !error.message.contains("sse connection closed") {
            eprintln!("[http] {} sse producer error: {}", request_id, error.message);
        }
    }
    runtime.clear_deadline();
    runtime.current_request_id = None;
}

/// 把 `http.response` 解析成状态码、响应头与响应体。
pub(crate) fn response_parts(response: &Value) -> ResponseParts {
    let (status, headers, body, file) = match response {
        Value::Ary(ary) => {
            let ary = ary.borrow();
            let status = ary
                .get("status")
                .and_then(|v| v.as_int())
                .unwrap_or(200);
            let mut headers = MapData::default();
            if let Some(Value::Map(map)) = ary.get("headers") {
                headers = map.borrow().clone();
            }
            // 内部字段 `_file` / `_file_offset` / `_file_len`：静态文件流式发送。
            let file = ary.get("_file").and_then(|value| value.as_str()).map(|path| {
                let offset = ary
                    .get("_file_offset")
                    .and_then(|value| value.as_int())
                    .unwrap_or(0)
                    .max(0) as u64;
                let length = ary
                    .get("_file_len")
                    .and_then(|value| value.as_int())
                    .unwrap_or(0)
                    .max(0) as u64;
                FilePart {
                    path: std::path::PathBuf::from(path),
                    offset,
                    length,
                }
            });
            // `body_base64` 是内部字段：二进制内容经它原样传递。
            let body: Vec<u8> = if file.is_some() {
                Vec::new()
            } else {
                match ary.get("body_base64").and_then(|v| v.as_str()) {
                    Some(encoded) => base64::engine::general_purpose::STANDARD
                        .decode(encoded.as_bytes())
                        .unwrap_or_default(),
                    None => match ary.get("json") {
                        Some(Value::Some(json)) => {
                            if !headers.has(&Value::string("Content-Type")) {
                                headers.set(
                                    Value::string("Content-Type"),
                                    Value::string("application/json; charset=utf-8"),
                                );
                            }
                            // 快速路径：直接写进响应缓冲，不建 serde_json::Value 中间树。
                            let mut body = Vec::with_capacity(128);
                            crate::value::value_write_json(&json, &mut body);
                            body
                        }
                        _ => ary
                            .get("body")
                            .and_then(|v| v.as_str())
                            .unwrap_or_default()
                            .into_bytes(),
                    },
                }
            };
            (status, headers, body, file)
        }
        _ => (500, MapData::default(), b"invalid response".to_vec(), None),
    };
    let mut pairs = Vec::with_capacity(headers.entries.len());
    for (key, value) in &headers.entries {
        pairs.push((value_text(key), value_text(value)));
    }
    ResponseParts {
        status,
        headers: pairs,
        body,
        file,
        streaming: false,
    }
}

/// 按 `Accept-Encoding` 协商 gzip 压缩响应体。
///
/// 只处理内存响应（静态文件走流式发送，压缩收益低且会破坏 Range），
/// 且仅在体积达到阈值时压缩：小响应压缩后往往反而更大。
pub(crate) fn maybe_compress(
    parts: &mut ResponseParts,
    accept_encoding: Option<&str>,
    method: &str,
) {
    if parts.file.is_some()
        || parts.streaming
        || parts.body.len() < 1024
        || method.eq_ignore_ascii_case("HEAD")
        || !matches!(
            parts.status,
            200 | 201 | 202 | 400 | 403 | 404 | 409 | 422 | 500
        )
        || header_value(&parts.headers, "content-encoding").is_some()
        || !header_value(&parts.headers, "content-type")
            .is_some_and(is_compressible_type)
    {
        return;
    }
    // 表示随 Accept-Encoding 变化，缓存必须分键存储。
    parts.headers.push((
        "Vary".to_string(),
        "Accept-Encoding".to_string(),
    ));
    if !accept_encoding.is_some_and(accepts_gzip) {
        return;
    }
    let mut encoder = flate2::write::GzEncoder::new(
        Vec::with_capacity(parts.body.len() / 2),
        // level 1：以吞吐与延迟优先，文本仍能压到 1/3 左右。
        flate2::Compression::fast(),
    );
    if encoder.write_all(&parts.body).is_err() {
        return;
    }
    let compressed = match encoder.finish() {
        Ok(bytes) => bytes,
        Err(_) => return,
    };
    // 压缩没有收益时保留原文，避免客户端多付解压成本。
    if compressed.len() >= parts.body.len() {
        return;
    }
    parts.body = compressed;
    parts
        .headers
        .retain(|(key, _)| !key.eq_ignore_ascii_case("content-length"));
    parts
        .headers
        .push(("Content-Encoding".to_string(), "gzip".to_string()));
}

fn header_value<'a>(headers: &'a [(String, String)], name: &str) -> Option<&'a str> {
    headers
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case(name))
        .map(|(_, value)| value.as_str())
}

/// 文本类内容才值得压缩；图片/字体/压缩包本身已压缩。
fn is_compressible_type(content_type: &str) -> bool {
    let content_type = content_type
        .split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase();
    content_type.starts_with("text/")
        || matches!(
            content_type.as_str(),
            "application/json"
                | "application/javascript"
                | "application/xml"
                | "application/xhtml+xml"
                | "application/rss+xml"
                | "application/atom+xml"
                | "image/svg+xml"
                | "application/x-www-form-urlencoded"
        )
        || content_type.ends_with("+json")
        || content_type.ends_with("+xml")
}

/// 解析 `Accept-Encoding`：支持 `q=0` 拒绝与 `*` 通配。
fn accepts_gzip(value: &str) -> bool {
    for item in value.split(',') {
        let mut params = item.trim().split(';');
        let token = params.next().unwrap_or_default().trim();
        let refused = params.any(|param| {
            param
                .trim()
                .strip_prefix("q=")
                .and_then(|q| q.trim().parse::<f32>().ok())
                .is_some_and(|q| q <= 0.0)
        });
        if !refused && (token.eq_ignore_ascii_case("gzip") || token == "*") {
            return true;
        }
    }
    false
}

/// HTTP 状态码对应的原因短语。
pub(crate) fn status_reason(status: i64) -> &'static str {
    match status {
        200 => "OK",
        201 => "Created",
        202 => "Accepted",
        204 => "No Content",
        206 => "Partial Content",
        301 => "Moved Permanently",
        302 => "Found",
        304 => "Not Modified",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        408 => "Request Timeout",
        409 => "Conflict",
        413 => "Payload Too Large",
        415 => "Unsupported Media Type",
        416 => "Range Not Satisfiable",
        422 => "Unprocessable Entity",
        429 => "Too Many Requests",
        500 => "Internal Server Error",
        502 => "Bad Gateway",
        503 => "Service Unavailable",
        504 => "Gateway Timeout",
        _ => "OK",
    }
}

fn write_http_response(
    stream: &mut impl Write,
    response: &Value,
    keep_alive: bool,
    head_only: bool,
    request_id: &str,
) -> Result<(), String> {
    let mut parts = response_parts(response);
    write_parts(stream, &mut parts, keep_alive, head_only, request_id)
}

/// 写出已解析的响应；body 按字节写出，二进制内容不会被转换。
pub(crate) fn write_parts(
    stream: &mut impl Write,
    parts: &mut ResponseParts,
    keep_alive: bool,
    head_only: bool,
    request_id: &str,
) -> Result<(), String> {
    let reason = status_reason(parts.status);
    let mut head = if parts.is_stream() {
        // 长度未知：不写 Content-Length，用关闭连接表示结束。
        format!(
            "HTTP/1.1 {} {}\r\nConnection: close\r\nX-Request-Id: {}\r\n",
            parts.status, reason, request_id
        )
    } else {
        format!(
            "HTTP/1.1 {} {}\r\nContent-Length: {}\r\nConnection: {}\r\nX-Request-Id: {}\r\n",
            parts.status,
            reason,
            parts.content_length(),
            if keep_alive { "keep-alive" } else { "close" },
            request_id
        )
    };
    for (key, value) in &parts.headers {
        // 保留头由服务器统一写入；非法名/值直接丢弃，防止响应头注入与响应拆分。
        if is_reserved_header(key) || !is_header_name(key) || !is_header_value(value) {
            continue;
        }
        head.push_str(&format!("{}: {}\r\n", key, value));
    }
    head.push_str("\r\n");
    stream
        .write_all(head.as_bytes())
        .map_err(|error| error.to_string())?;
    if head_only {
        return stream.flush().map_err(|error| error.to_string());
    }
    // 流式响应（SSE）body 为空：这里只写响应头，帧由生产函数经 SseSink 直写 socket。
    match &parts.file {
        // 静态文件：按 offset/length 边读边写，内存占用与文件大小无关。
        Some(file) => {
            let mut handle = fs::File::open(&file.path).map_err(|error| error.to_string())?;
            if file.offset > 0 {
                use std::io::Seek;
                handle
                    .seek(std::io::SeekFrom::Start(file.offset))
                    .map_err(|error| error.to_string())?;
            }
            let mut limited = handle.take(file.length);
            std::io::copy(&mut limited, stream).map_err(|error| error.to_string())?;
        }
        None if !parts.body.is_empty() => {
            stream
                .write_all(&parts.body)
                .map_err(|error| error.to_string())?;
        }
        None => {}
    }
    stream.flush().map_err(|error| error.to_string())
}


/// 请求进行中计数：panic 展开时也能正确减一，避免指标泄漏。
struct InFlightGuard<'a>(&'a std::sync::atomic::AtomicUsize);

impl Drop for InFlightGuard<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::Relaxed);
    }
}

fn value_text(value: &Value) -> String {
    value.as_str().unwrap_or_else(|| format_value(value))
}

/// 把请求头 map 转成 (键, 值) 列表，忽略空键。
fn header_pairs(value: Option<&Value>) -> Vec<(String, String)> {
    match value {
        Some(Value::Map(map)) => {
            let map = map.borrow();
            let mut items = Vec::new();
            for (key, value) in &map.entries {
                let key = key.as_str().unwrap_or_default();
                if !key.is_empty() {
                    items.push((key.to_string(), value.as_str().unwrap_or_default().to_string()));
                }
            }
            items
        }
        _ => Vec::new(),
    }
}

/// 出站响应转成 `http.client_response`。
fn client_response_value(
    status: i64,
    received: Vec<(String, String)>,
    body: String,
) -> Value {
    let mut map = MapData::default();
    for (key, value) in received {
        map.set(
            Value::string(key.to_ascii_lowercase()),
            Value::string(value),
        );
    }
    Value::ary(
        "http.client_response",
        vec![
            ("status".to_string(), Value::Int(status)),
            (
                "headers".to_string(),
                Value::map_data(map),
            ),
            ("body".to_string(), Value::string(body)),
        ],
    )
}

fn content_type_for(path: &std::path::Path) -> String {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    match ext.as_str() {
        "html" | "htm" => "text/html; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "js" => "application/javascript; charset=utf-8",
        "json" => "application/json; charset=utf-8",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "svg" => "image/svg+xml",
        "txt" => "text/plain; charset=utf-8",
        _ => "application/octet-stream",
    }
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pbkdf2_matches_rfc_vectors() {
        // RFC 6070 同源向量（SHA-256 版）：password / salt，1 与 2 轮。
        let one = pbkdf2_sha256(b"password", b"salt", 1);
        assert_eq!(
            hex_lower(&one),
            "120fb6cffcf8b32c43e7225256c4f837a86548c92ccc35480805987cb70be17b"
        );
        let two = pbkdf2_sha256(b"password", b"salt", 2);
        assert_eq!(
            hex_lower(&two),
            "ae4d0c95af6b46d32d0adff928f06dd02a303f8ef3c251dfd6e2d85a95474c43"
        );
    }

    #[test]
    fn verifies_password_hash_format() {
        let salt = [7u8; 16];
        let hashed = format!(
            "ript$1000${}${}",
            hex_lower(&salt),
            hex_lower(&pbkdf2_sha256(b"secret", &salt, 1000))
        );
        assert!(password_verify("secret", &hashed));
        assert!(!password_verify("wrong", &hashed));
        // 旧格式不再兼容：只认 ript$。
        assert!(!password_verify(
            "secret",
            &format!("ripx$1000${}$00", hex_lower(&salt))
        ));
    }

    #[test]
    fn rejects_absurd_rounds_and_unknown_scheme() {
        let salt = hex_lower(&[1u8; 16]);
        assert!(!password_verify("secret", &format!("ript$0${}$00", salt)));
        assert!(!password_verify(
            "secret",
            &format!("ript${}${}$00", PASSWORD_MAX_ROUNDS + 1, salt)
        ));
        assert!(!password_verify("secret", &format!("argon$1000${}$00", salt)));
        assert!(!password_verify("secret", "not-a-hash"));
    }

    #[test]
    fn read_request_rejects_loose_headers_and_v1_chunked() {
        let config = crate::server::ServerConfig::default();
        // 冒号前带空格的头名：与严格代理定界不一致（走私面），必须 400。
        let loose = b"GET / HTTP/1.1\r\nContent-Length : 5\r\n\r\n";
        let mut reader = &loose[..];
        assert!(matches!(
            read_request(&mut reader, &config),
            ReadOutcome::BadRequest(_)
        ));
        // 无冒号头行（obs-fold 续行）：400。
        let fold = b"GET / HTTP/1.1\r\n continuation\r\n\r\n";
        let mut reader = &fold[..];
        assert!(matches!(
            read_request(&mut reader, &config),
            ReadOutcome::BadRequest(_)
        ));
        // HTTP/1.0 带 TE: chunked：400。
        let v10 = b"POST / HTTP/1.0\r\nTransfer-Encoding: chunked\r\n\r\n";
        let mut reader = &v10[..];
        assert!(matches!(
            read_request(&mut reader, &config),
            ReadOutcome::BadRequest(_)
        ));
        // 合法请求仍可解析。
        let ok = b"GET / HTTP/1.1\r\nHost: example.com\r\n\r\n";
        let mut reader = &ok[..];
        assert!(matches!(
            read_request(&mut reader, &config),
            ReadOutcome::Request(_)
        ));
    }

    #[test]
    fn aes_v2_payload_layout() {
        // V2 前缀 + 盐(16) + nonce(12)；旧格式（裸 nonce||ct）不带前缀。
        assert!(!derive_key("k").is_empty());
        let salt = [3u8; 16];
        let v2 = derive_key_v2("k", &salt);
        let legacy = derive_key("k");
        assert_ne!(v2, legacy);
        let fake_v2 = {
            let mut bytes = AES_V2_PREFIX.to_vec();
            bytes.extend_from_slice(&salt);
            bytes.extend_from_slice(&[0u8; 12]);
            bytes
        };
        assert!(fake_v2.starts_with(AES_V2_PREFIX));
        let fake_legacy = vec![0u8; 40];
        assert!(!fake_legacy.starts_with(AES_V2_PREFIX));
    }
}
