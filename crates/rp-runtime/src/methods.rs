//! 内置方法分发。

use std::rc::Rc;

use rp_ast::IntSuffix;
use unicode_normalization::UnicodeNormalization;
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use crate::rt::{FlowResult, Runtime};
use crate::value::{deep_clone, format_value, value_eq, Ary, MapData, Value};

impl Runtime {
    pub(crate) fn invoke_router(
        &mut self,
        table: &crate::value::RouteTable,
        args: Vec<Value>,
        line: u32,
    ) -> FlowResult<Value> {
        let request = match args.first() {
            Some(value @ Value::Ary(_)) => value.clone(),
            _ => return self.error("router handler expects an http.request", line),
        };
        // 借用完成匹配：method/path 不再各克隆一个 String，
        // 且常见方法本来就是大写，避免每请求的 `to_uppercase` 分配。
        let matched = match &request {
            Value::Ary(ary) => {
                let ary = ary.borrow();
                let method = ary
                    .get_ref("method")
                    .and_then(Value::as_str_ref)
                    .unwrap_or("GET");
                let path = ary
                    .get_ref("path")
                    .and_then(Value::as_str_ref)
                    .unwrap_or("/");
                if method.bytes().all(|b| !b.is_ascii_lowercase()) {
                    table.find(method, path)
                } else {
                    table.find(&method.to_ascii_uppercase(), path)
                }
            }
            _ => unreachable!(),
        };
        if let Some((index, params)) = matched {
            let route = table.route(index);
            // 命中路由的模式（含 `:param`）作为指标与访问日志的标签，
            // 不用实际路径，避免指标基数随 URL 参数爆炸。
            self.current_route = Some(table.label(index).to_string());
            if !params.is_empty() {
                if let Value::Ary(ary) = &request {
                    let mut map = Vec::new();
                    for (k, v) in params {
                        map.push((Value::string(k), Value::string(v)));
                    }
                    ary.borrow_mut().set("params", Value::map(map));
                }
            }
            if let Value::Func(handler) = &route.handler {
                return self.call_value(handler, vec![request], line);
            }
            return self.error("route handler is not callable", line);
        }
        Ok(make_response(404, "not found"))
    }

    /// 路由分组：剥掉前缀后把请求交给内部处理器。
    ///
    /// 请求对象按字段浅拷贝（`headers` / `query` / `ctx` 仍是同一份引用），
    /// 只把 `path` 换成去掉前缀的剩余路径，因此分组内的路由不需要知道前缀。
    pub(crate) fn invoke_mounted(
        &mut self,
        prefix: &str,
        inner: &Value,
        args: Vec<Value>,
        line: u32,
    ) -> FlowResult<Value> {
        let request = match args.first() {
            Some(value @ Value::Ary(_)) => value.clone(),
            _ => return self.error("group handler expects an http.request", line),
        };
        let path = match &request {
            Value::Ary(ary) => ary
                .borrow()
                .get("path")
                .and_then(|value| value.as_str())
                .unwrap_or_else(|| "/".to_string()),
            _ => unreachable!(),
        };
        let rest = match strip_mount_prefix(&path, prefix) {
            Some(rest) => rest,
            None => return Ok(make_response(404, "not found")),
        };
        let copied = match &request {
            Value::Ary(ary) => {
                let ary = ary.borrow();
                let mut fields = ary.fields.clone();
                if let Some((_, slot)) = fields.iter_mut().find(|(name, _)| name == "path") {
                    *slot = Value::string(rest);
                }
                Value::ary_owned(Ary {
                    class: ary.class.clone(),
                    fields,
                })
            }
            _ => unreachable!(),
        };
        match inner {
            Value::Func(func) => self.call_value(func, vec![copied], line),
            _ => self.error("group handler is not callable", line),
        }
    }

    /// 内建中间件：CORS 处理预检请求，Headers 只追加响应头。
    pub(crate) fn invoke_middleware(
        &mut self,
        spec: &crate::value::MiddlewareSpec,
        next: &Value,
        args: Vec<Value>,
        line: u32,
    ) -> FlowResult<Value> {
        use crate::value::MiddlewareSpec;
        let request = match args.first() {
            Some(value @ Value::Ary(_)) => value.clone(),
            _ => return self.error("middleware expects an http.request", line),
        };
        match spec {
            MiddlewareSpec::Cors { origin } => {
                let (method, requested) = match &request {
                    Value::Ary(ary) => {
                        let ary = ary.borrow();
                        let method = ary
                            .get("method")
                            .and_then(|v| v.as_str())
                            .unwrap_or_else(|| "GET".to_string())
                            .to_uppercase();
                        let requested = match ary.get("headers") {
                            Some(Value::Map(map)) => map
                                .borrow()
                                .entries
                                .iter()
                                .find_map(|(key, value)| {
                                    key.as_str()
                                        .filter(|item| {
                                            item.eq_ignore_ascii_case(
                                                "access-control-request-headers",
                                            )
                                        })
                                        .and_then(|_| value.as_str())
                                })
                                .unwrap_or_default(),
                            _ => String::new(),
                        };
                        (method, requested)
                    }
                    _ => unreachable!(),
                };
                let mut headers = vec![
                    ("Access-Control-Allow-Origin".to_string(), origin.clone()),
                    ("Vary".to_string(), "Origin".to_string()),
                ];
                if method == "OPTIONS" {
                    headers.push((
                        "Access-Control-Allow-Methods".to_string(),
                        "GET, POST, PUT, PATCH, DELETE, OPTIONS".to_string(),
                    ));
                    headers.push((
                        "Access-Control-Allow-Headers".to_string(),
                        if requested.is_empty() {
                            "Content-Type, Authorization, X-CSRF-Token".to_string()
                        } else {
                            requested
                        },
                    ));
                    headers.push(("Access-Control-Max-Age".to_string(), "86400".to_string()));
                    let response = make_response(204, "");
                    merge_response_headers(&response, &headers);
                    return Ok(response);
                }
                let response = call_next(self, next, request, line)?;
                merge_response_headers(&response, &headers);
                Ok(response)
            }
            MiddlewareSpec::Headers { headers } => {
                let response = call_next(self, next, request, line)?;
                merge_response_headers(&response, headers);
                Ok(response)
            }
            // 路由级限流：按客户端 IP 取令牌，超出直接 429，不调用下一级。
            MiddlewareSpec::RateLimit { limiter } => {
                let client_ip = match &request {
                    Value::Ary(ary) => ary
                        .borrow()
                        .get("client_ip")
                        .and_then(|value| value.as_str())
                        .unwrap_or_default(),
                    _ => String::new(),
                };
                if !limiter.allow(&client_ip) {
                    if let Some(metrics) = &self.metrics {
                        metrics
                            .rate_limited
                            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    }
                    return Ok(rate_limited_response());
                }
                call_next(self, next, request, line)
            }
        }
    }

    /// 中间件链：首次调用时组合并缓存，后续请求直接走组合后的处理函数。
    pub(crate) fn invoke_chain(
        &mut self,
        middlewares: &[Value],
        handler: &Value,
        composed: &std::cell::OnceCell<Value>,
        args: Vec<Value>,
        line: u32,
    ) -> FlowResult<Value> {
        let request = match args.into_iter().next() {
            Some(request) => request,
            None => return self.error("handler expects an http.request", line),
        };
        if let Some(composed) = composed.get() {
            return call_next(self, composed, request, line);
        }
        let mut current = handler.clone();
        for middleware in middlewares.iter().rev() {
            let next = match middleware {
                Value::Func(func) => self.call_value(func, vec![current], line)?,
                _ => return self.error("middleware must be a function", line),
            };
            current = next;
        }
        let _ = composed.set(current.clone());
        call_next(self, &current, request, line)
    }

    pub fn call_method(
        &mut self,
        receiver: Value,
        name: &str,
        args: Vec<Value>,
        line: u32,
    ) -> FlowResult<Value> {
        if name.starts_with("as_") {
            return self.any_method(receiver, name, args, line);
        }
        match receiver {
            Value::Int(v) => self.int_method(Value::Int(v), v as i128, name, args, line),
            Value::UInt(v) => self.int_method(Value::UInt(v), v as i128, name, args, line),
            Value::Float(v) => self.float_method(v, name, args, line),
            Value::Bool(b) => match name {
                "to_string" => Ok(Value::string(b.to_string())),
                _ => self.error(format!("bool has no method '{}'", name), line),
            },
            Value::Str(s) => {
                let s = (*s).clone();
                self.string_method(s, name, args, line)
            }
            Value::Char(c) => self.string_method(c, name, args, line),
            Value::Vec(items) => self.vec_method(items, name, args, line),
            Value::Map(map) => self.map_method(map, name, args, line),
            Value::Some(inner) => self.option_method(Some(*inner), name, args, line),
            Value::None => self.option_method(None, name, args, line),
            Value::Ok(inner) => self.result_method(Ok(*inner), name, args, line),
            Value::Err(inner) => self.result_method(Err(*inner), name, args, line),
            Value::Ary(ary) => {
                let class = ary.borrow().class.clone();
                // 用户方法：`func 类型.方法(...)`，按运行时类名分派（接口值同样是带类名的 ary）。
                if !class.is_empty() {
                    let method = format!("{}.{}", class, name);
                    if self.has_func(&method) {
                        let mut values = Vec::with_capacity(args.len() + 1);
                        values.push(Value::Ary(ary));
                        values.extend(args);
                        return self.call_named(&method, values, line);
                    }
                }
                match class.as_str() {
                    "time.datetime" => self.datetime_method(&ary, name, args, line),
                    "llm.client" => self.llm_client_method(&ary, name, args, line),
                    "db.conn" => self.db_conn_method(&ary, name, args, line),
                    "db.query" => self.db_query_method(&ary, name, args, line),
                    "redis.conn" => self.redis_conn_method(&ary, name, args, line),
                    "sync.mutex" => self.sync_mutex_method(&ary, name, args, line),
                    "sync.atomic" => self.sync_atomic_method(&ary, name, args, line),
                    "sync.channel" => self.sync_channel_method(&ary, name, args, line),
                    _ => match name {
                        "clone" => Ok(Value::Ary(ary)),
                        _ => self.error(
                            format!("{} has no method '{}'", class, name),
                            line,
                        ),
                    },
                }
            }
            Value::Null => self.error(format!("null has no method '{}'", name), line),
            Value::Func(_) => self.error(format!("function has no method '{}'", name), line),
            Value::Task(_) => self.error(format!("task has no method '{}'", name), line),
        }
    }

    fn int_method(
        &mut self,
        wrapped: Value,
        v: i128,
        name: &str,
        args: Vec<Value>,
        line: u32,
    ) -> FlowResult<Value> {
        // 结果保持接收者的有无符号表示；位宽越界由绑定处的类型转换负责。
        let wrap = |value: i128| match wrapped {
            Value::UInt(_) => Value::UInt(value as u64),
            _ => Value::Int(value as i64),
        };
        match name {
            // 整数转换：按 Rust `as` 规则截断 / 符号扩展；进制参数只对字符串有意义。
            "to_int" | "to_i64" => self.convert_num(v, IntSuffix::I64, &args, line),
            "to_i8" => self.convert_num(v, IntSuffix::I8, &args, line),
            "to_i16" => self.convert_num(v, IntSuffix::I16, &args, line),
            "to_i32" => self.convert_num(v, IntSuffix::I32, &args, line),
            "to_u8" => self.convert_num(v, IntSuffix::U8, &args, line),
            "to_u16" => self.convert_num(v, IntSuffix::U16, &args, line),
            "to_u32" => self.convert_num(v, IntSuffix::U32, &args, line),
            "to_u64" => self.convert_num(v, IntSuffix::U64, &args, line),
            "to_float" => Ok(Value::Float(v as f64)),
            "to_string" => Ok(Value::string(v.to_string())),
            "to_string_base" => {
                let base = self.int_arg(&args, 0, name, line)?;
                if !(2..=36).contains(&base) {
                    return self.error("base must be 2..36", line);
                }
                Ok(Value::string(match wrapped {
                    Value::UInt(_) => uint_to_base(v as u64, base as u32),
                    _ => int_to_base(v as i64, base as u32),
                }))
            }
            "abs" => {
                if v == i64::MIN as i128 {
                    self.error("integer overflow", line)
                } else {
                    Ok(wrap(v.abs()))
                }
            }
            "min" => {
                let other = self.int_wide_arg(&args, 0, name, line)?;
                Ok(wrap(v.min(other)))
            }
            "max" => {
                let other = self.int_wide_arg(&args, 0, name, line)?;
                Ok(wrap(v.max(other)))
            }
            "mod" => {
                let other = self.int_wide_arg(&args, 0, name, line)?;
                if other == 0 {
                    return self.error("modulo by zero", line);
                }
                Ok(wrap(v.rem_euclid(other)))
            }
            "pow" => {
                let exp = self.int_arg(&args, 0, name, line)?;
                if exp < 0 {
                    return self.error("exponent must be non-negative", line);
                }
                match v.checked_pow(exp as u32) {
                    Some(value) => Ok(wrap(value)),
                    None => self.error("integer overflow", line),
                }
            }
            "clamp" => {
                let low = self.int_wide_arg(&args, 0, name, line)?;
                let high = self.int_wide_arg(&args, 1, name, line)?;
                if low > high {
                    return self.error("clamp lower bound exceeds upper bound", line);
                }
                Ok(wrap(v.clamp(low, high)))
            }
            _ => self.error(format!("int has no method '{}'", name), line),
        }
    }

    /// 数值接收者的位宽转换：带进制参数时把数值的十进制写法按该进制重新解释
    /// （`55.to_i8(16)` → 0x55 = 85），缺省 10 时就是普通的位宽转换。
    fn convert_num(
        &mut self,
        value: i128,
        suffix: IntSuffix,
        args: &[Value],
        line: u32,
    ) -> FlowResult<Value> {
        let base = match args.first() {
            None => return Ok(convert_int(value, suffix)),
            Some(arg) => match arg.as_int() {
                Some(base) if (2..=36).contains(&base) => base as u32,
                _ => return self.error("base must be 2..36", line),
            },
        };
        if base == 10 {
            return Ok(convert_int(value, suffix));
        }
        let negative = value < 0;
        let digits = value.unsigned_abs().to_string();
        match i128::from_str_radix(&digits, base) {
            Ok(parsed) => Ok(convert_int(if negative { -parsed } else { parsed }, suffix)),
            Err(_) => self.error(
                format!("{} is not a valid base-{} integer", value, base),
                line,
            ),
        }
    }

    fn float_method(
        &mut self,
        v: f64,
        name: &str,
        args: Vec<Value>,
        line: u32,
    ) -> FlowResult<Value> {
        match name {
            "to_int" => Ok(Value::Int(v as i64)),
            "to_float" => Ok(Value::Float(v)),
            "to_string" => Ok(Value::string(format!("{}", v))),
            "abs" => Ok(Value::Float(v.abs())),
            "min" => {
                let other = self.float_arg(&args, 0, name, line)?;
                Ok(Value::Float(v.min(other)))
            }
            "max" => {
                let other = self.float_arg(&args, 0, name, line)?;
                Ok(Value::Float(v.max(other)))
            }
            "pow" => {
                let other = self.float_arg(&args, 0, name, line)?;
                Ok(Value::Float(v.powf(other)))
            }
            "clamp" => {
                let low = self.float_arg(&args, 0, name, line)?;
                let high = self.float_arg(&args, 1, name, line)?;
                Ok(Value::Float(v.clamp(low, high)))
            }
            _ => self.error(format!("float has no method '{}'", name), line),
        }
    }

    fn string_method(
        &mut self,
        s: String,
        name: &str,
        args: Vec<Value>,
        line: u32,
    ) -> FlowResult<Value> {
        let chars: Vec<char> = s.chars().collect();
        match name {
            "len" => Ok(Value::Int(chars.len() as i64)),
            "at" => {
                let index = self.int_arg(&args, 0, name, line)?;
                if index < 0 || index as usize >= chars.len() {
                    return Ok(Value::Err(Box::new(Value::string(
                        "index out of bounds",
                    ))));
                }
                Ok(Value::Ok(Box::new(Value::string(
                    chars[index as usize].to_string(),
                ))))
            }
            "slice" => {
                let start = self.int_arg(&args, 0, name, line)?;
                let end = self.int_arg(&args, 1, name, line)?;
                if start < 0 || end < start || end as usize > chars.len() {
                    return Ok(Value::Err(Box::new(Value::string("slice out of bounds"))));
                }
                let value: String = chars[start as usize..end as usize].iter().collect();
                Ok(Value::Ok(Box::new(Value::string(value))))
            }
            "first" => Ok(Value::string(
                chars.first().map(|c| c.to_string()).unwrap_or_default(),
            )),
            "last" => Ok(Value::string(
                chars.last().map(|c| c.to_string()).unwrap_or_default(),
            )),
            "contains" => {
                let needle = self.string_arg(&args, 0, name, line)?;
                Ok(Value::Bool(s.contains(&needle)))
            }
            "starts_with" => {
                let needle = self.string_arg(&args, 0, name, line)?;
                Ok(Value::Bool(s.starts_with(&needle)))
            }
            "ends_with" => {
                let needle = self.string_arg(&args, 0, name, line)?;
                Ok(Value::Bool(s.ends_with(&needle)))
            }
            "index_of" => {
                let needle = self.string_arg(&args, 0, name, line)?;
                match s.find(&needle) {
                    Some(byte_index) => {
                        let char_index = s[..byte_index].chars().count();
                        Ok(Value::Some(Box::new(Value::Int(char_index as i64))))
                    }
                    None => Ok(Value::None),
                }
            }
            "last_index_of" => {
                let needle = self.string_arg(&args, 0, name, line)?;
                match s.rfind(&needle) {
                    Some(byte_index) => {
                        let char_index = s[..byte_index].chars().count();
                        Ok(Value::Some(Box::new(Value::Int(char_index as i64))))
                    }
                    None => Ok(Value::None),
                }
            }
            "to_upper" => Ok(Value::string(s.to_uppercase())),
            "to_lower" => Ok(Value::string(s.to_lowercase())),
            // 模板插值统一走 `.to_string()`，字符串自身原样返回。
            "to_string" => Ok(Value::string(s.to_string())),
            "trim" => Ok(Value::string(s.trim())),
            "trim_start" => Ok(Value::string(s.trim_start())),
            "trim_end" => Ok(Value::string(s.trim_end())),
            "replace" => {
                let from = self.string_arg(&args, 0, name, line)?;
                let to = self.string_arg(&args, 1, name, line)?;
                Ok(Value::string(s.replacen(&from, &to, 1)))
            }
            "replace_all" => {
                let from = self.string_arg(&args, 0, name, line)?;
                let to = self.string_arg(&args, 1, name, line)?;
                Ok(Value::string(s.replace(&from, &to)))
            }
            "split" => {
                let sep = self.string_arg(&args, 0, name, line)?;
                let parts: Vec<Value> = s.split(&sep).map(Value::string).collect();
                Ok(Value::vec(parts))
            }
            "split_lines" => {
                let parts: Vec<Value> = s.lines().map(Value::string).collect();
                Ok(Value::vec(parts))
            }
            "chars" => Ok(Value::vec(
                chars.iter().map(|c| Value::string(c.to_string())).collect(),
            )),
            "bytes" => Ok(Value::vec(
                s.as_bytes()
                    .iter()
                    .map(|b| Value::UInt(*b as u64))
                    .collect(),
            )),
            "reverse" => Ok(Value::string(chars.iter().rev().collect::<String>())),
            "filter" => {
                let f = self.func_arg(&args, 0, name, line)?;
                let mut out = String::new();
                for c in chars {
                    let keep = self.call_value(
                        &f,
                        vec![Value::string(c.to_string())],
                        line,
                    )?;
                    if let Value::Bool(true) = keep {
                        out.push(c);
                    }
                }
                Ok(Value::string(out))
            }
            "remove_whitespace" => Ok(Value::string(
                chars
                    .iter()
                    .filter(|c| !c.is_whitespace())
                    .collect::<String>(),
            )),
            "keep_chars" => {
                let set = self.string_arg(&args, 0, name, line)?;
                Ok(Value::string(
                    chars.iter().filter(|c| set.contains(**c)).collect::<String>(),
                ))
            }
            "remove_chars" => {
                let set = self.string_arg(&args, 0, name, line)?;
                Ok(Value::string(
                    chars
                        .iter()
                        .filter(|c| !set.contains(**c))
                        .collect::<String>(),
                ))
            }
            "to_int" | "to_i64" | "to_i8" | "to_i16" | "to_i32" | "to_u8" | "to_u16"
            | "to_u32" | "to_u64" => {
                let suffix = if name == "to_int" {
                    IntSuffix::I64
                } else {
                    IntSuffix::from_name(&name[3..]).unwrap_or(IntSuffix::I64)
                };
                // 进制参数缺省 10；给字符串按指定进制解析。
                let base = match args.first() {
                    None => 10u32,
                    Some(value) => match value.as_int() {
                        Some(base) if (2..=36).contains(&base) => base as u32,
                        _ => return self.error("base must be 2..36", line),
                    },
                };
                match i128::from_str_radix(s.trim(), base) {
                    Ok(value) => {
                        let (min, max) = suffix.range();
                        if value < min || value > max {
                            return Ok(Value::Err(Box::new(Value::string(format!(
                                "integer out of range for {}",
                                suffix.name()
                            )))));
                        }
                        Ok(Value::Ok(Box::new(convert_int(value, suffix))))
                    }
                    Err(_) => Ok(Value::Err(Box::new(Value::string("invalid integer")))),
                }
            }
            "to_float" => match s.trim().parse::<f64>() {
                Ok(value) => Ok(Value::Ok(Box::new(Value::Float(value)))),
                Err(_) => Ok(Value::Err(Box::new(Value::string("invalid float")))),
            },
            "to_bool" => match s.trim() {
                "true" => Ok(Value::Ok(Box::new(Value::Bool(true)))),
                "false" => Ok(Value::Ok(Box::new(Value::Bool(false)))),
                _ => Ok(Value::Err(Box::new(Value::string("invalid bool")))),
            },
            "pad_start" | "pad_end" => {
                let width = self.int_arg(&args, 0, name, line)?;
                let pad = self.string_arg(&args, 1, name, line)?;
                if width < 0 {
                    return self.error("width must be non-negative", line);
                }
                let current = chars.len() as i64;
                if current >= width || pad.is_empty() {
                    return Ok(Value::string(s));
                }
                let mut padding = String::new();
                while (current + padding.chars().count() as i64) < width {
                    padding.push_str(&pad);
                }
                let padding: String = padding
                    .chars()
                    .take((width - current) as usize)
                    .collect();
                if name == "pad_start" {
                    Ok(Value::string(format!("{}{}", padding, s)))
                } else {
                    Ok(Value::string(format!("{}{}", s, padding)))
                }
            }
            "repeat" => {
                let count = self.int_arg(&args, 0, name, line)?;
                if count < 0 {
                    return self.error("repeat count must be non-negative", line);
                }
                Ok(Value::string(s.repeat(count as usize)))
            }
            "join" => {
                let items = match args.first() {
                    Some(Value::Vec(items)) => items.borrow().clone(),
                    _ => return self.error("join expects vec<string>", line),
                };
                let mut parts = Vec::new();
                for item in items {
                    match item.as_str() {
                        Some(part) => parts.push(part),
                        None => return self.error("join expects vec<string>", line),
                    }
                }
                Ok(Value::string(parts.join(&s)))
            }
            "clone" => Ok(Value::string(s)),
            // Unicode 归一化：`s.normalize("nfc")`。
            "normalize" => {
                let form = self
                    .string_arg(&args, 0, name, line)?
                    .trim()
                    .to_ascii_lowercase();
                let normalized = match form.as_str() {
                    "nfc" => s.nfc().collect::<String>(),
                    "nfd" => s.nfd().collect::<String>(),
                    "nfkc" => s.nfkc().collect::<String>(),
                    "nfkd" => s.nfkd().collect::<String>(),
                    _ => {
                        return Ok(Value::Err(Box::new(Value::string(format!(
                            "unknown normalization form '{}' (use nfc/nfd/nfkc/nfkd)",
                            form
                        )))))
                    }
                };
                Ok(Value::Ok(Box::new(Value::string(normalized))))
            }
            "graphemes" => Ok(Value::vec(
                s.graphemes(true).map(Value::string).collect(),
            )),
            "grapheme_len" => Ok(Value::Int(s.graphemes(true).count() as i64)),
            "width" => Ok(Value::Int(UnicodeWidthStr::width(s.as_str()) as i64)),
            _ => self.error(format!("string has no method '{}'", name), line),
        }
    }

    fn vec_method(
        &mut self,
        items: Rc<std::cell::RefCell<Vec<Value>>>,
        name: &str,
        args: Vec<Value>,
        line: u32,
    ) -> FlowResult<Value> {
        match name {
            "len" => Ok(Value::Int(items.borrow().len() as i64)),
            "is_empty" => Ok(Value::Bool(items.borrow().is_empty())),
            "push" => {
                let value = self.arg(&args, 0, name, line)?;
                items.borrow_mut().push(value);
                Ok(Value::Null)
            }
            "pop" => {
                let value = items.borrow_mut().pop();
                Ok(match value {
                    Some(value) => Value::Some(Box::new(value)),
                    None => Value::None,
                })
            }
            "insert" => {
                let index = self.int_arg(&args, 0, name, line)?;
                let value = self.arg(&args, 1, name, line)?;
                let mut items = items.borrow_mut();
                if index < 0 || index as usize > items.len() {
                    return self.error("insert index out of bounds", line);
                }
                items.insert(index as usize, value);
                Ok(Value::Null)
            }
            "remove" => {
                let index = self.int_arg(&args, 0, name, line)?;
                let mut items = items.borrow_mut();
                if index < 0 || index as usize >= items.len() {
                    return self.error("remove index out of bounds", line);
                }
                Ok(items.remove(index as usize))
            }
            "clear" => {
                items.borrow_mut().clear();
                Ok(Value::Null)
            }
            "reverse" => {
                items.borrow_mut().reverse();
                Ok(Value::Null)
            }
            // 返回新 vec（不改原 vec），元素类型与接收者一致。
            "take" | "skip" => {
                let count = self.int_arg(&args, 0, name, line)?.max(0) as usize;
                let source = items.borrow();
                let sliced: Vec<Value> = if name == "take" {
                    source.iter().take(count).cloned().collect()
                } else {
                    source.iter().skip(count).cloned().collect()
                };
                Ok(Value::vec(sliced))
            }
            "sort" => {
                let f = self.func_arg(&args, 0, name, line)?;
                let snapshot = items.borrow().clone();
                let mut sorted: Vec<Value> = Vec::new();
                for item in snapshot {
                    let mut pos = sorted.len();
                    while pos > 0 {
                        let cmp = self.call_value(
                            &f,
                            vec![sorted[pos - 1].clone(), item.clone()],
                            line,
                        )?;
                        let ordering = cmp.as_int().unwrap_or(0);
                        if ordering > 0 {
                            pos -= 1;
                        } else {
                            break;
                        }
                    }
                    sorted.insert(pos, item);
                }
                *items.borrow_mut() = sorted;
                Ok(Value::Null)
            }
            "map" => {
                let f = self.func_arg(&args, 0, name, line)?;
                let snapshot = items.borrow().clone();
                let mut out = Vec::new();
                for item in snapshot {
                    out.push(self.call_value(&f, vec![item], line)?);
                }
                Ok(Value::vec(out))
            }
            "filter" => {
                let f = self.func_arg(&args, 0, name, line)?;
                let snapshot = items.borrow().clone();
                let mut out = Vec::new();
                for item in snapshot {
                    let keep = self.call_value(&f, vec![item.clone()], line)?;
                    if let Value::Bool(true) = keep {
                        out.push(item);
                    }
                }
                Ok(Value::vec(out))
            }
            "reduce" => {
                let f = self.func_arg(&args, 0, name, line)?;
                let mut acc = self.arg(&args, 1, name, line)?;
                let snapshot = items.borrow().clone();
                for item in snapshot {
                    acc = self.call_value(&f, vec![acc, item], line)?;
                }
                Ok(acc)
            }
            "find" => {
                let f = self.func_arg(&args, 0, name, line)?;
                let snapshot = items.borrow().clone();
                for item in snapshot {
                    let keep = self.call_value(&f, vec![item.clone()], line)?;
                    if let Value::Bool(true) = keep {
                        return Ok(Value::Some(Box::new(item)));
                    }
                }
                Ok(Value::None)
            }
            "any" | "all" | "count" => {
                let f = self.func_arg(&args, 0, name, line)?;
                let snapshot = items.borrow().clone();
                let mut count = 0i64;
                for item in snapshot {
                    let keep = self.call_value(&f, vec![item], line)?;
                    if let Value::Bool(true) = keep {
                        count += 1;
                        if name == "any" {
                            return Ok(Value::Bool(true));
                        }
                    } else if name == "all" {
                        return Ok(Value::Bool(false));
                    }
                }
                Ok(match name {
                    "any" => Value::Bool(false),
                    "all" => Value::Bool(true),
                    _ => Value::Int(count),
                })
            }
            "slice" => {
                let start = self.int_arg(&args, 0, name, line)?;
                let end = self.int_arg(&args, 1, name, line)?;
                let snapshot = items.borrow().clone();
                if start < 0 || end < start || end as usize > snapshot.len() {
                    return self.error("slice out of bounds", line);
                }
                Ok(Value::vec(
                    snapshot[start as usize..end as usize].to_vec(),
                ))
            }
            "concat" => {
                let other = match args.first() {
                    Some(Value::Vec(other)) => other.borrow().clone(),
                    _ => return self.error("concat expects vec", line),
                };
                let mut values = items.borrow().clone();
                values.extend(other);
                Ok(Value::vec(values))
            }
            "unique" => {
                let snapshot = items.borrow().clone();
                let mut out: Vec<Value> = Vec::new();
                for item in snapshot {
                    if !out.iter().any(|existing| value_eq(existing, &item)) {
                        out.push(item);
                    }
                }
                Ok(Value::vec(out))
            }
            "flatten" => {
                let snapshot = items.borrow().clone();
                let mut out = Vec::new();
                for item in snapshot {
                    match item {
                        Value::Vec(inner) => out.extend(inner.borrow().clone()),
                        _ => return self.error("flatten expects vec<vec<T>>", line),
                    }
                }
                Ok(Value::vec(out))
            }
            "join" => {
                let sep = self.string_arg(&args, 0, name, line)?;
                let snapshot = items.borrow().clone();
                let mut parts = Vec::new();
                for item in snapshot {
                    match item.as_str() {
                        Some(part) => parts.push(part),
                        None => return self.error("join expects vec<string>", line),
                    }
                }
                Ok(Value::string(parts.join(&sep)))
            }
            "chunk" => {
                let size = self.int_arg(&args, 0, name, line)?;
                if size <= 0 {
                    return self.error("chunk size must be positive", line);
                }
                let snapshot = items.borrow().clone();
                let chunks: Vec<Value> = snapshot
                    .chunks(size as usize)
                    .map(|chunk| Value::vec(chunk.to_vec()))
                    .collect();
                Ok(Value::vec(chunks))
            }
            "zip" => {
                let other = match args.first() {
                    Some(Value::Vec(other)) => other.borrow().clone(),
                    _ => return self.error("zip expects vec", line),
                };
                let snapshot = items.borrow().clone();
                let mut out = Vec::new();
                for (a, b) in snapshot.into_iter().zip(other.into_iter()) {
                    out.push(Value::vec(vec![a, b]));
                }
                Ok(Value::vec(out))
            }
            "at" => {
                let index = self.int_arg(&args, 0, name, line)?;
                // 只取单个元素，避免每次索引都克隆整条 Vec。
                let taken = {
                    let items = items.borrow();
                    if index < 0 || index as usize >= items.len() {
                        None
                    } else {
                        Some(items[index as usize].clone())
                    }
                };
                match taken {
                    Some(value) => Ok(Value::Ok(Box::new(value))),
                    None => Ok(Value::Err(Box::new(Value::string("index out of bounds")))),
                }
            }
            "clone" => {
                let snapshot = items.borrow().clone();
                Ok(Value::vec(snapshot.iter().map(deep_clone).collect()))
            }
            _ => self.error(format!("vec has no method '{}'", name), line),
        }
    }

    fn map_method(
        &mut self,
        map: Rc<std::cell::RefCell<MapData>>,
        name: &str,
        args: Vec<Value>,
        line: u32,
    ) -> FlowResult<Value> {
        match name {
            "set" => {
                let key = self.arg(&args, 0, name, line)?;
                let value = self.arg(&args, 1, name, line)?;
                map.borrow_mut().set(key, value);
                Ok(Value::Null)
            }
            "get" => {
                let key = self.arg(&args, 0, name, line)?;
                let value = map.borrow().get(&key);
                Ok(match value {
                    Some(value) => Value::Some(Box::new(value)),
                    None => Value::None,
                })
            }
            "has" => {
                let key = self.arg(&args, 0, name, line)?;
                Ok(Value::Bool(map.borrow().has(&key)))
            }
            "delete" => {
                let key = self.arg(&args, 0, name, line)?;
                let value = map.borrow_mut().delete(&key);
                Ok(match value {
                    Some(value) => Value::Some(Box::new(value)),
                    None => Value::None,
                })
            }
            "len" => Ok(Value::Int(map.borrow().entries.len() as i64)),
            "keys" => Ok(Value::vec(
                map.borrow().entries.iter().map(|(k, _)| k.clone()).collect(),
            )),
            "values" => Ok(Value::vec(
                map.borrow()
                    .entries
                    .iter()
                    .map(|(_, v)| v.clone())
                    .collect(),
            )),
            "items" => {
                let entries = map.borrow().entries.clone();
                let items: Vec<Value> = entries
                    .into_iter()
                    .map(|(k, v)| {
                        Value::ary(
                            "",
                            vec![
                                ("key".to_string(), k),
                                ("value".to_string(), v),
                            ],
                        )
                    })
                    .collect();
                Ok(Value::vec(items))
            }
            "merge" => {
                let other = match args.first() {
                    Some(Value::Map(other)) => other.borrow().entries.clone(),
                    _ => return self.error("merge expects map", line),
                };
                let mut map = map.borrow_mut();
                for (k, v) in other {
                    map.set(k, v);
                }
                Ok(Value::Null)
            }
            "filter" => {
                let f = self.func_arg(&args, 0, name, line)?;
                let entries = map.borrow().entries.clone();
                let mut out = MapData::default();
                for (k, v) in entries {
                    let keep = self.call_value(
                        &f,
                        vec![k.clone(), v.clone()],
                        line,
                    )?;
                    if let Value::Bool(true) = keep {
                        out.set(k, v);
                    }
                }
                Ok(Value::map_data(out))
            }
            "clone" => {
                let map = map.borrow();
                Ok(Value::map(
                    map.entries
                        .iter()
                        .map(|(k, v)| (deep_clone(k), deep_clone(v)))
                        .collect(),
                ))
            }
            _ => self.error(format!("map has no method '{}'", name), line),
        }
    }

    fn option_method(
        &mut self,
        value: Option<Value>,
        name: &str,
        args: Vec<Value>,
        line: u32,
    ) -> FlowResult<Value> {
        match name {
            "unwrap_or" => {
                let fallback = self.arg(&args, 0, name, line)?;
                Ok(value.unwrap_or(fallback))
            }
            "is_some" => Ok(Value::Bool(value.is_some())),
            "is_none" => Ok(Value::Bool(value.is_none())),
            "unwrap" => match value {
                Some(value) => Ok(value),
                None => self.error("called unwrap on none", line),
            },
            "clone" => Ok(match value {
                Some(value) => Value::Some(Box::new(deep_clone(&value))),
                None => Value::None,
            }),
            _ => self.error(format!("option has no method '{}'", name), line),
        }
    }

    fn result_method(
        &mut self,
        value: Result<Value, Value>,
        name: &str,
        args: Vec<Value>,
        line: u32,
    ) -> FlowResult<Value> {
        match name {
            "unwrap_or" => {
                let fallback = self.arg(&args, 0, name, line)?;
                Ok(match value {
                    Ok(value) => value,
                    Err(_) => fallback,
                })
            }
            "is_ok" => Ok(Value::Bool(value.is_ok())),
            "is_err" => Ok(Value::Bool(value.is_err())),
            "unwrap" => match value {
                Ok(value) => Ok(value),
                Err(error) => self.error(
                    format!("called unwrap on err: {}", format_value(&error)),
                    line,
                ),
            },
            "unwrap_err" => match value {
                Err(error) => Ok(error),
                Ok(value) => self.error(
                    format!("called unwrap_err on ok: {}", format_value(&value)),
                    line,
                ),
            },
            "clone" => Ok(match value {
                Ok(value) => Value::Ok(Box::new(deep_clone(&value))),
                Err(error) => Value::Err(Box::new(deep_clone(&error))),
            }),
            _ => self.error(format!("result has no method '{}'", name), line),
        }
    }

    fn any_method(
        &mut self,
        value: Value,
        name: &str,
        args: Vec<Value>,
        line: u32,
    ) -> FlowResult<Value> {
        if !args.is_empty() {
            return self.error(format!("method '{}' takes no arguments", name), line);
        }
        let result = match name {
            "as_string" => match &value {
                Value::Str(s) => Some(Value::string((**s).clone())),
                Value::Char(c) => Some(Value::string(c.clone())),
                _ => None,
            },
            "as_int" => value.as_int().map(Value::Int),
            "as_float" => match &value {
                Value::Float(v) => Some(Value::Float(*v)),
                other => other.as_int().map(|v| Value::Float(v as f64)),
            },
            "as_bool" => value.as_bool().map(Value::Bool),
            "as_vec" => match &value {
                Value::Vec(_) => Some(value.clone()),
                _ => None,
            },
            "as_map" => match &value {
                Value::Map(_) => Some(value.clone()),
                _ => None,
            },
            _ => return self.error(format!("any has no method '{}'", name), line),
        };
        Ok(match result {
            Some(value) => Value::Some(Box::new(value)),
            None => Value::None,
        })
    }

    fn datetime_method(
        &mut self,
        ary: &Rc<std::cell::RefCell<Ary>>,
        name: &str,
        args: Vec<Value>,
        line: u32,
    ) -> FlowResult<Value> {
        let timestamp = ary
            .borrow()
            .get("timestamp_ms")
            .and_then(|v| v.as_int())
            .unwrap_or_else(now_ms);
        match name {
            "timestamp_ms" => Ok(Value::Int(timestamp)),
            "format" => {
                let pattern = self.string_arg(&args, 0, name, line)?;
                Ok(Value::string(format_datetime(timestamp, &pattern)))
            }
            "to_string" => Ok(Value::string(format_datetime(
                timestamp,
                "YYYY-MM-DD HH:MM:SS",
            ))),
            _ => self.error(format!("time.datetime has no method '{}'", name), line),
        }
    }

    fn llm_client_method(
        &mut self,
        ary: &Rc<std::cell::RefCell<Ary>>,
        name: &str,
        args: Vec<Value>,
        line: u32,
    ) -> FlowResult<Value> {
        let config = ary.borrow();
        let base_url = config
            .get("base_url")
            .and_then(|v| v.as_str())
            .unwrap_or_default();
        let provider = config
            .get("provider")
            .and_then(|v| v.as_str())
            .unwrap_or_default();
        drop(config);

        let messages = match args.first() {
            Some(Value::Vec(items)) => items.borrow().clone(),
            _ => return self.error("chat expects messages vec", line),
        };
        let options = args.get(1).cloned().unwrap_or(Value::Null);
        let model = options
            .clone()
            .as_ary_field("model")
            .and_then(|v| v.as_str())
            .unwrap_or_else(|| "ripmock-1".to_string());
        let last = messages
            .last()
            .and_then(|m| m.as_ary_field("content"))
            .and_then(|v| v.as_str())
            .unwrap_or_default();
        let text = format!(
            "（Rip 演示模型 {} / {}）已收到：{}",
            provider, base_url, last
        );
        let prompt_tokens = (last.chars().count() as i64 / 2).max(1);
        let completion_tokens = (text.chars().count() as i64 / 2).max(1);
        let response = Value::ary(
            "llm.response",
            vec![
                ("content".to_string(), Value::string(text.clone())),
                ("model".to_string(), Value::string(model.clone())),
                ("prompt_tokens".to_string(), Value::Int(prompt_tokens)),
                (
                    "completion_tokens".to_string(),
                    Value::Int(completion_tokens),
                ),
                (
                    "finish_reason".to_string(),
                    Value::string("stop"),
                ),
            ],
        );

        match name {
            "chat" => Ok(Value::Ok(Box::new(response))),
            "chat_stream" => {
                let progress = self.func_arg(&args, 2, name, line)?;
                let chunks: Vec<String> = text
                    .chars()
                    .collect::<Vec<char>>()
                    .chunks(6)
                    .map(|c| c.iter().collect())
                    .collect();
                for chunk in chunks {
                    self.call_value(&progress, vec![Value::string(chunk)], line)?;
                }
                Ok(Value::Ok(Box::new(response)))
            }
            "chat_json" => {
                let mut object = MapData::default();
                object.set(Value::string("reply"), Value::string(text));
                Ok(Value::Ok(Box::new(Value::map_data(object))))
            }
            _ => self.error(format!("llm.client has no method '{}'", name), line),
        }
    }

    fn arg(&self, args: &[Value], index: usize, name: &str, line: u32) -> FlowResult<Value> {
        match args.get(index) {
            Some(value) => Ok(value.clone()),
            None => self.error(
                format!("method '{}' missing argument {}", name, index + 1),
                line,
            ),
        }
    }

    pub(crate) fn int_arg(
        &self,
        args: &[Value],
        index: usize,
        name: &str,
        line: u32,
    ) -> FlowResult<i64> {
        match args.get(index).and_then(|v| v.as_int()) {
            Some(value) => Ok(value),
            None => self.error(
                format!("argument {} of '{}' must be int", index + 1, name),
                line,
            ),
        }
    }

    /// 读取整数参数并保留完整 64 位范围（`u64` 上界超出 `i64`），用于 `min` / `max` / `clamp`。
    pub(crate) fn int_wide_arg(
        &self,
        args: &[Value],
        index: usize,
        name: &str,
        line: u32,
    ) -> FlowResult<i128> {
        match args.get(index) {
            Some(Value::Int(value)) => Ok(*value as i128),
            Some(Value::UInt(value)) => Ok(*value as i128),
            _ => self.error(
                format!("argument {} of '{}' must be int", index + 1, name),
                line,
            ),
        }
    }

    fn float_arg(
        &self,
        args: &[Value],
        index: usize,
        name: &str,
        line: u32,
    ) -> FlowResult<f64> {
        match args.get(index) {
            Some(Value::Float(value)) => Ok(*value),
            _ => self.error(
                format!("argument {} of '{}' must be float", index + 1, name),
                line,
            ),
        }
    }

    pub(crate) fn string_arg(
        &self,
        args: &[Value],
        index: usize,
        name: &str,
        line: u32,
    ) -> FlowResult<String> {
        match args.get(index).and_then(|v| v.as_str()) {
            Some(value) => Ok(value),
            None => self.error(
                format!("argument {} of '{}' must be string", index + 1, name),
                line,
            ),
        }
    }

    fn func_arg(
        &self,
        args: &[Value],
        index: usize,
        name: &str,
        line: u32,
    ) -> FlowResult<Rc<crate::value::Function>> {
        match args.get(index) {
            Some(Value::Func(func)) => Ok(func.clone()),
            _ => self.error(
                format!("argument {} of '{}' must be function", index + 1, name),
                line,
            ),
        }
    }
}

impl Value {
    fn as_ary_field(&self, name: &str) -> Option<Value> {
        match self {
            Value::Ary(ary) => ary.borrow().get(name),
            _ => None,
        }
    }
}

fn int_to_base(value: i64, base: u32) -> String {
    if value == 0 {
        return "0".to_string();
    }
    let negative = value < 0;
    let mut value = value.unsigned_abs();
    let mut digits = Vec::new();
    while value > 0 {
        let digit = (value % base as u64) as u32;
        digits.push(char::from_digit(digit, base).unwrap_or('0'));
        value /= base as u64;
    }
    if negative {
        digits.push('-');
    }
    digits.iter().rev().collect()
}

fn uint_to_base(value: u64, base: u32) -> String {
    if value == 0 {
        return "0".to_string();
    }
    let mut value = value;
    let mut digits = Vec::new();
    while value > 0 {
        let digit = (value % base as u64) as u32;
        digits.push(char::from_digit(digit, base).unwrap_or('0'));
        value /= base as u64;
    }
    digits.iter().rev().collect()
}

/// 按 Rust `as` 语义把整数截断 / 符号扩展到目标位宽，并打到对应的运行时表示上。
pub(crate) fn convert_int(value: i128, suffix: IntSuffix) -> Value {
    let bits = suffix.bits();
    let mask = (1i128 << bits) - 1;
    let mut truncated = value & mask;
    if suffix.is_signed() && truncated >= (1i128 << (bits - 1)) {
        truncated -= 1i128 << bits;
    }
    if suffix.is_signed() {
        Value::Int(truncated as i64)
    } else {
        Value::UInt(truncated as u64)
    }
}

pub(crate) fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

pub(crate) fn format_datetime(timestamp_ms: i64, pattern: &str) -> String {
    // 使用 UTC+8（Asia/Shanghai）显示，避免引入额外时区依赖。
    let total_seconds = timestamp_ms.div_euclid(1000) + 8 * 3600;
    let days = total_seconds.div_euclid(86_400);
    let seconds_of_day = total_seconds.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    let hour = seconds_of_day / 3600;
    let minute = (seconds_of_day % 3600) / 60;
    let second = seconds_of_day % 60;

    let mut out = String::new();
    let mut index = 0;
    while index < pattern.len() {
        let rest = &pattern[index..];
        let token = [
            ("YYYY", format!("{:04}", year)),
            ("YY", format!("{:02}", year % 100)),
            ("MM", format!("{:02}", month)),
            ("DD", format!("{:02}", day)),
            ("HH", format!("{:02}", hour)),
            ("mm", format!("{:02}", minute)),
            ("SS", format!("{:02}", second)),
        ]
        .into_iter()
        .find(|(token, _)| rest.starts_with(token));
        match token {
            Some((token, value)) => {
                out.push_str(&value);
                index += token.len();
            }
            None => {
                let ch = rest.chars().next().unwrap();
                out.push(ch);
                index += ch.len_utf8();
            }
        }
    }
    out
}

fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if m <= 2 { y + 1 } else { y };
    (year, m as u32, d as u32)
}

pub(crate) fn make_response(status: i64, body: &str) -> Value {
    let mut headers = MapData::default();
    headers.set(
        Value::string("Content-Type"),
        Value::string("text/plain; charset=utf-8"),
    );
    Value::ary(
        "http.response",
        vec![
            ("status".to_string(), Value::Int(status)),
            (
                "headers".to_string(),
                Value::map_data(headers),
            ),
            ("body".to_string(), Value::string(body.to_string())),
            ("json".to_string(), Value::None),
            ("sse".to_string(), Value::None),
        ],
    )
}

/// 429：路由级限流命中时的响应，带 `Retry-After`。
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

/// 去掉分组前缀，返回剩余路径；不匹配返回 None。
fn strip_mount_prefix(path: &str, prefix: &str) -> Option<String> {
    let trimmed = prefix.trim_end_matches('/');
    if trimmed.is_empty() {
        return Some(path.to_string());
    }
    if path == trimmed {
        return Some("/".to_string());
    }
    match path.strip_prefix(trimmed) {
        Some(rest) if rest.starts_with('/') => Some(rest.to_string()),
        _ => None,
    }
}

fn call_next(
    runtime: &mut Runtime,
    next: &Value,
    request: Value,
    line: u32,
) -> FlowResult<Value> {
    match next {
        Value::Func(func) => runtime.call_value(func, vec![request], line),
        _ => runtime.error("middleware next is not callable", line),
    }
}

/// 把响应头合并进 `http.response.headers`；已有同名头被覆盖。
fn merge_response_headers(response: &Value, extra: &[(String, String)]) {
    let Value::Ary(ary) = response else {
        return;
    };
    let mut ary = ary.borrow_mut();
    let existing = match ary.get("headers") {
        Some(Value::Map(map)) => Some(map),
        _ => None,
    };
    let mut merged = match &existing {
        Some(map) => map.borrow().clone(),
        None => MapData::default(),
    };
    for (key, value) in extra {
        merged.set(Value::string(key.clone()), Value::string(value.clone()));
    }
    match existing {
        Some(map) => *map.borrow_mut() = merged,
        None => ary.fields.push(("headers".to_string(), Value::map_data(merged))),
    }
}

