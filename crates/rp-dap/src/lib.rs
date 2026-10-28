//! Rip 调试适配器（DAP）。
//!
//! 通过 stdio（缺省）或 TCP（`rip dap --port N`）实现 Debug Adapter Protocol
//! 的可用子集：启动、断点、单步、暂停、调用栈、变量、表达式求值与断开。
//!
//! 适配器本身不加载 Rip 源码；`rip` 把「解析 → 类型检查 → 运行」作为
//! 启动闭包传进来，在独立线程里执行，调试状态由 [`rp_runtime::debug::Debugger`]
//! 与解释器共享。

use std::io::{self, BufRead, BufReader, Write};
use std::net::TcpListener;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};
use std::thread;

use rp_runtime::debug::{self, DebugEvent, Debugger, StepMode, StopInfo};
use serde_json::{json, Value};

/// 启动一次调试运行：`(debugger, program, args)` → 运行结果。
pub type Launcher =
    Arc<dyn Fn(Arc<Debugger>, String, Vec<String>) -> Result<(), String> + Send + Sync>;

/// 从标准输入输出运行适配器。程序输出会被改为 DAP `output` 事件。
pub fn run_stdio(launch: Launcher) -> i32 {
    let stdin = io::stdin();
    let reader = BufReader::new(stdin);
    let writer: Box<dyn Write + Send> = Box::new(io::stdout());
    serve(reader, writer, launch)
}

/// 在指定端口监听一次客户端连接（VSCode 的 `debugServer` 模式）。
pub fn run_tcp(port: u16, launch: Launcher) -> i32 {
    let listener = match TcpListener::bind(("127.0.0.1", port)) {
        Ok(listener) => listener,
        Err(error) => {
            eprintln!("error: cannot listen on 127.0.0.1:{}: {}", port, error);
            return 1;
        }
    };
    eprintln!("rip dap: listening on 127.0.0.1:{}", port);
    let (stream, address) = match listener.accept() {
        Ok(connection) => connection,
        Err(error) => {
            eprintln!("error: accept failed: {}", error);
            return 1;
        }
    };
    eprintln!("rip dap: client {} connected", address);
    let writer_stream = match stream.try_clone() {
        Ok(stream) => stream,
        Err(error) => {
            eprintln!("error: cannot clone socket: {}", error);
            return 1;
        }
    };
    serve(
        BufReader::new(stream),
        Box::new(writer_stream),
        launch,
    )
}

fn serve<R: BufRead>(
    mut reader: R,
    writer: Box<dyn Write + Send>,
    launch: Launcher,
) -> i32 {
    let seq = Arc::new(AtomicI64::new(1));
    // 写入是同步的：DAP 请求响应与程序事件共用一个加锁的 writer，
    // 退出前最后一条响应一定已经写到线路上。
    let writer = Arc::new(Mutex::new(writer));

    let (event_tx, event_rx) = mpsc::channel::<DebugEvent>();
    let event_seq = Arc::clone(&seq);
    let event_writer = Arc::clone(&writer);
    let forwarder = thread::spawn(move || {
        for incoming in event_rx {
            let message = match incoming {
                DebugEvent::Stopped(info) => stopped_event(&event_seq, &info),
                DebugEvent::Output(text) => {
                    let mut output = text;
                    if !output.ends_with('\n') {
                        output.push('\n');
                    }
                    event(&event_seq, "output", json!({ "category": "stdout", "output": output }))
                }
                DebugEvent::Exited(code) => {
                    event(&event_seq, "exited", json!({ "exitCode": code }))
                }
                DebugEvent::Terminated => {
                    write_locked(&event_writer, &event(&event_seq, "terminated", Value::Null));
                    break;
                }
            };
            write_locked(&event_writer, &message);
        }
    });

    let mut session = Session {
        seq,
        writer,
        event_tx,
        launch,
        program: None,
        args: Vec::new(),
        source: String::new(),
        debugger: None,
        started: false,
    };
    let code = session.run(&mut reader);
    debug::capture_output(None);
    drop(session);
    drop(forwarder);
    code
}

fn write_locked(writer: &Arc<Mutex<Box<dyn Write + Send>>>, message: &Value) {
    if let Ok(mut writer) = writer.lock() {
        let _ = write_message(&mut **writer, message);
    }
}

struct Session {
    seq: Arc<AtomicI64>,
    writer: Arc<Mutex<Box<dyn Write + Send>>>,
    event_tx: Sender<DebugEvent>,
    launch: Launcher,
    program: Option<String>,
    args: Vec<String>,
    source: String,
    debugger: Option<Arc<Debugger>>,
    started: bool,
}

impl Session {
    fn run<R: BufRead>(&mut self, reader: &mut R) -> i32 {
        loop {
            let message = match read_message(reader) {
                Ok(Some(message)) => message,
                Ok(None) => return 0,
                Err(_) => return 1,
            };
            if message.get("type").and_then(|value| value.as_str()) != Some("request") {
                continue;
            }
            let command = message
                .get("command")
                .and_then(|value| value.as_str())
                .unwrap_or("")
                .to_string();
            let request_seq = message.get("seq").and_then(|value| value.as_i64()).unwrap_or(0);
            let arguments = message.get("arguments").cloned().unwrap_or(Value::Null);

            let mut disconnect = false;
            let body = match command.as_str() {
                "initialize" => {
                    let response = self.ok(request_seq, &command, json!({
                        "supportsConfigurationDoneRequest": true,
                        "supportsSetBreakpoints": true,
                        "supportsEvaluateForHovers": true,
                        "supportsTerminateRequest": true,
                        "supportsConditionalBreakpoints": false,
                        "supportsFunctionBreakpoints": false,
                        "supportsStepBack": false
                    }));
                    self.send(response);
                    self.send(event(&self.seq, "initialized", Value::Null));
                    continue;
                }
                "launch" => {
                    self.program = arguments
                        .get("program")
                        .and_then(|value| value.as_str())
                        .map(|value| value.to_string());
                    self.source = self.program.clone().unwrap_or_default();
                    self.args = arguments
                        .get("args")
                        .and_then(|value| value.as_array())
                        .map(|items| {
                            items
                                .iter()
                                .filter_map(|item| item.as_str().map(|text| text.to_string()))
                                .collect()
                        })
                        .unwrap_or_default();
                    let debugger = self.ensure_debugger();
                    debugger.set_stop_on_entry(
                        arguments
                            .get("stopOnEntry")
                            .and_then(|value| value.as_bool())
                            .unwrap_or(false),
                    );
                    self.ok(request_seq, &command, Value::Null)
                }
                "setBreakpoints" => {
                    let lines: Vec<u32> = arguments
                        .get("breakpoints")
                        .and_then(|value| value.as_array())
                        .map(|items| {
                            items
                                .iter()
                                .filter_map(|item| item.get("line")?.as_u64())
                                .filter(|line| *line > 0 && *line <= u32::MAX as u64)
                                .map(|line| line as u32)
                                .collect()
                        })
                        .unwrap_or_default();
                    let debugger = self.ensure_debugger();
                    debugger.set_breakpoints(lines.clone());
                    let verified: Vec<Value> = lines
                        .iter()
                        .map(|line| json!({ "verified": true, "line": line }))
                        .collect();
                    self.ok(request_seq, &command, json!({ "breakpoints": verified }))
                }
                "setExceptionBreakpoints" => {
                    self.ok(request_seq, &command, json!({ "breakpoints": [] }))
                }
                "configurationDone" => {
                    self.start_program();
                    self.ok(request_seq, &command, Value::Null)
                }
                "threads" => self.ok(
                    request_seq,
                    &command,
                    json!({ "threads": [{ "id": 1, "name": "main" }] }),
                ),
                "stackTrace" => self.stack_trace(request_seq, &command),
                "scopes" => self.scopes(request_seq, &command),
                "variables" => self.variables(request_seq, &command, &arguments),
                "evaluate" => {
                    let expression = arguments
                        .get("expression")
                        .and_then(|value| value.as_str())
                        .unwrap_or("");
                    match self
                        .debugger
                        .as_ref()
                        .and_then(|debugger| debugger.evaluate(expression))
                    {
                        Some(value) => self.ok(
                            request_seq,
                            &command,
                            json!({ "result": value, "variablesReference": 0 }),
                        ),
                        None => self.fail(
                            request_seq,
                            &command,
                            format!("cannot evaluate '{}' (only variable names are supported)", expression),
                        ),
                    }
                }
                "continue" => {
                    if let Some(debugger) = &self.debugger {
                        debugger.resume_continue();
                    }
                    self.ok(request_seq, &command, json!({ "allThreadsContinued": true }))
                }
                "next" => {
                    if let Some(debugger) = &self.debugger {
                        debugger.resume_step(StepMode::Over);
                    }
                    self.ok(request_seq, &command, Value::Null)
                }
                "stepIn" => {
                    if let Some(debugger) = &self.debugger {
                        debugger.resume_step(StepMode::In);
                    }
                    self.ok(request_seq, &command, Value::Null)
                }
                "stepOut" => {
                    if let Some(debugger) = &self.debugger {
                        debugger.resume_step(StepMode::Out);
                    }
                    self.ok(request_seq, &command, Value::Null)
                }
                "pause" => {
                    if let Some(debugger) = &self.debugger {
                        debugger.request_pause();
                    }
                    self.ok(request_seq, &command, Value::Null)
                }
                "disconnect" | "terminate" => {
                    disconnect = true;
                    self.ok(request_seq, &command, Value::Null)
                }
                _ => self.ok(request_seq, &command, Value::Null),
            };
            self.send(body);
            if disconnect {
                return 0;
            }
        }
    }

    fn send(&self, message: Value) {
        write_locked(&self.writer, &message);
    }

    fn ok(&self, request_seq: i64, command: &str, body: Value) -> Value {
        response(&self.seq, request_seq, command, true, body)
    }

    fn fail(&self, request_seq: i64, command: &str, message: String) -> Value {
        let mut response = response(&self.seq, request_seq, command, false, Value::Null);
        response["message"] = Value::String(message);
        response
    }

    fn ensure_debugger(&mut self) -> Arc<Debugger> {
        if let Some(debugger) = &self.debugger {
            return Arc::clone(debugger);
        }
        let debugger = Arc::new(Debugger::new());
        let _ = debugger.attach_events(self.event_tx.clone());
        debug::capture_output(Some(self.event_tx.clone()));
        self.debugger = Some(Arc::clone(&debugger));
        debugger
    }

    fn start_program(&mut self) {
        if self.started {
            return;
        }
        self.started = true;
        let program = match &self.program {
            Some(program) => program.clone(),
            None => {
                self.send(event(&self.seq, "output", json!({
                    "category": "stderr",
                    "output": "rip dap: launch request did not provide \"program\"\n"
                })));
                self.send(event(&self.seq, "terminated", Value::Null));
                return;
            }
        };
        let debugger = self.ensure_debugger();
        self.send(event(&self.seq, "process", json!({
            "name": "rip",
            "isLocalProcess": true,
            "startMethod": "launch"
        })));
        self.send(event(&self.seq, "thread", json!({ "reason": "started", "threadId": 1 })));

        let launch = Arc::clone(&self.launch);
        let args = self.args.clone();
        let running = Arc::clone(&debugger);
        thread::spawn(move || {
            let code = match launch(Arc::clone(&running), program, args) {
                Ok(()) => 0,
                Err(message) => {
                    if !message.is_empty() {
                        debug::emit_line(message);
                    }
                    1
                }
            };
            running.finish(code);
        });
    }

    fn stack_trace(&mut self, request_seq: i64, command: &str) -> Value {
        let stop = match self.current_stop() {
            Some(stop) => stop,
            None => {
                return self.ok(
                    request_seq,
                    &command,
                    json!({ "stackFrames": [], "totalFrames": 0 }),
                )
            }
        };
        let frames: Vec<Value> = stop
            .frames
            .iter()
            .rev()
            .enumerate()
            .map(|(index, (name, line))| {
                json!({
                    "id": index,
                    "name": name,
                    "source": {
                        "name": source_name(&self.source),
                        "path": self.source.clone()
                    },
                    "line": line,
                    "column": 1
                })
            })
            .collect();
        self.ok(
            request_seq,
            &command,
            json!({ "stackFrames": frames, "totalFrames": frames.len() }),
        )
    }

    fn scopes(&mut self, request_seq: i64, command: &str) -> Value {
        let (locals, globals) = match self.current_stop() {
            Some(stop) => (stop.locals.len(), stop.globals.len()),
            None => (0, 0),
        };
        self.ok(
            request_seq,
            &command,
            json!({
                "scopes": [
                    {
                        "name": "Locals",
                        "variablesReference": 1,
                        "expensive": false,
                        "namedVariables": locals
                    },
                    {
                        "name": "Globals",
                        "variablesReference": 2,
                        "expensive": false,
                        "namedVariables": globals
                    }
                ]
            }),
        )
    }

    fn variables(&mut self, request_seq: i64, command: &str, arguments: &Value) -> Value {
        let reference = arguments
            .get("variablesReference")
            .and_then(|value| value.as_i64())
            .unwrap_or(0);
        let stop = match self.current_stop() {
            Some(stop) => stop,
            None => return self.ok(request_seq, &command, json!({ "variables": [] })),
        };
        let items = match reference {
            1 => &stop.locals,
            2 => &stop.globals,
            _ => return self.ok(request_seq, &command, json!({ "variables": [] })),
        };
        let variables: Vec<Value> = items
            .iter()
            .map(|(name, value)| {
                json!({
                    "name": name,
                    "value": value,
                    "type": value_type(value),
                    "variablesReference": 0
                })
            })
            .collect();
        self.ok(request_seq, &command, json!({ "variables": variables }))
    }

    fn current_stop(&self) -> Option<StopInfo> {
        self.debugger.as_ref().and_then(|debugger| debugger.last_stop())
    }
}

fn source_name(path: &str) -> String {
    std::path::Path::new(path)
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_else(|| path.to_string())
}

/// 按展示文本猜一个 DAP 类型名，纯粹为了让编辑器显示得更友好。
fn value_type(value: &str) -> &'static str {
    let trimmed = value.trim();
    if trimmed == "true" || trimmed == "false" {
        "bool"
    } else if trimmed.parse::<i64>().is_ok() {
        "int"
    } else if trimmed.parse::<f64>().is_ok() {
        "float"
    } else if trimmed.starts_with('[') {
        "vec"
    } else if trimmed.starts_with('{') {
        "map"
    } else {
        "string"
    }
}

fn stopped_event(seq: &AtomicI64, info: &StopInfo) -> Value {
    event(
        seq,
        "stopped",
        json!({
            "reason": info.reason,
            "description": format!("paused at line {}", info.line),
            "threadId": 1,
            "allThreadsStopped": true
        }),
    )
}

fn response(
    seq: &AtomicI64,
    request_seq: i64,
    command: &str,
    success: bool,
    body: Value,
) -> Value {
    json!({
        "seq": seq.fetch_add(1, Ordering::SeqCst),
        "type": "response",
        "request_seq": request_seq,
        "success": success,
        "command": command,
        "body": body
    })
}

fn event(seq: &AtomicI64, name: &str, body: Value) -> Value {
    json!({
        "seq": seq.fetch_add(1, Ordering::SeqCst),
        "type": "event",
        "event": name,
        "body": body
    })
}

/// 单条消息体的大小上限（64 MiB），防止恶意 Content-Length 造成无界分配。
const MAX_MESSAGE_LEN: usize = 64 * 1024 * 1024;

fn read_message<R: BufRead>(reader: &mut R) -> io::Result<Option<Value>> {
    let mut content_length: Option<usize> = None;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line)? == 0 {
            return Ok(None);
        }
        let trimmed = line
            .trim_end_matches(['\r', '\n'])
            .trim_start_matches('\u{feff}');
        if trimmed.is_empty() {
            break;
        }
        if let Some(rest) = trimmed.strip_prefix("Content-Length:") {
            content_length = rest.trim().parse().ok();
        }
    }

    let length = match content_length {
        Some(length) => length,
        None => return Ok(None),
    };
    if length > MAX_MESSAGE_LEN {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("Content-Length {} exceeds the 64 MiB limit", length),
        ));
    }
    let mut body = vec![0u8; length];
    reader.read_exact(&mut body)?;
    let value = serde_json::from_slice(&body)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    Ok(Some(value))
}

fn write_message<W: Write + ?Sized>(writer: &mut W, value: &Value) -> io::Result<()> {
    let body = serde_json::to_vec(value)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    write!(writer, "Content-Length: {}\r\n\r\n", body.len())?;
    writer.write_all(&body)?;
    writer.flush()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_message_rejects_oversized_content_length() {
        // 4 GiB 的 Content-Length 必须报错断开，而不是分配等长缓冲。
        let raw = b"Content-Length: 4294967296\r\n\r\n{}";
        let mut reader = &raw[..];
        let result = read_message(&mut reader);
        assert!(result.is_err());
    }

    #[test]
    fn read_message_accepts_normal_content_length() {
        let body = b"{\"type\":\"request\",\"seq\":1}";
        let raw = format!("Content-Length: {}\r\n\r\n", body.len()).into_bytes();
        let mut cursor = std::io::Cursor::new([raw.as_slice(), &body[..]].concat());
        let message = read_message(&mut cursor).expect("read").expect("message");
        assert_eq!(message["seq"], 1);
    }
}
