//! `std/net` 与 `std/ws`：TCP / UDP 套接字与 WebSocket。
//!
//! 连接、监听器与套接字在 `NetState` 里以整数句柄登记，Rip 侧看到的是
//! `net.conn` / `net.listener` / `net.socket` / `ws.conn` 不透明值。

use std::collections::HashMap;
use std::io::{self, Read, Write};
use std::net::{TcpListener, TcpStream, ToSocketAddrs, UdpSocket};
use std::time::{Duration, Instant};

use base64::Engine as _;
use sha1::Digest as _;

use crate::rt::{FlowResult, Runtime};
use crate::value::{Flow, Value};

/// 进程内打开的套接字与 WebSocket 连接。
#[derive(Default)]
pub(crate) struct NetState {
    next_id: i64,
    tcp: HashMap<i64, TcpStream>,
    listeners: HashMap<i64, TcpListener>,
    udp: HashMap<i64, UdpSocket>,
    ws: HashMap<i64, WsConn>,
}

impl NetState {
    fn next_id(&mut self) -> i64 {
        self.next_id += 1;
        self.next_id
    }

    /// 关闭本运行时打开的全部套接字与 WebSocket（任务复用前调用）。
    pub(crate) fn reset(&mut self) {
        self.tcp.clear();
        self.listeners.clear();
        self.udp.clear();
        self.ws.clear();
    }
}

impl Runtime {
    pub(crate) fn net_module(
        &mut self,
        name: &str,
        args: Vec<Value>,
        line: u32,
    ) -> FlowResult<Value> {
        match name {
            "tcp_connect" => {
                let host = self.string_arg(&args, 0, name, line)?;
                let port = self.int_arg(&args, 1, name, line)?;
                let timeout = self.int_arg(&args, 2, name, line)?;
                if !(0..=65535).contains(&port) {
                    return self.error("tcp_connect port must be 0..65535", line);
                }
                let address = format!("{}:{}", host, port);
                let stream = match connect_tcp(&address, timeout) {
                    Ok(stream) => stream,
                    Err(message) => return Ok(Value::Err(Box::new(Value::string(message)))),
                };
                let remote = stream
                    .peer_addr()
                    .map(|addr| addr.to_string())
                    .unwrap_or(address);
                let id = self.net_state.next_id();
                self.net_state.tcp.insert(id, stream);
                Ok(Value::Ok(Box::new(handle_value("net.conn", id, &remote))))
            }
            "tcp_send" => {
                let id = self.handle_arg(&args, 0, "net.conn", name, line)?;
                let text = self.string_arg(&args, 1, name, line)?;
                match self.net_state.tcp.get_mut(&id) {
                    Some(stream) => match stream.write_all(text.as_bytes()) {
                        Ok(()) => {
                            let _ = stream.flush();
                            Ok(Value::Ok(Box::new(Value::Int(text.len() as i64))))
                        }
                        Err(error) => Ok(Value::Err(Box::new(Value::string(error.to_string())))),
                    },
                    None => Ok(Value::Err(Box::new(Value::string("connection is closed".to_string())))),
                }
            }
            "tcp_recv" => {
                let id = self.handle_arg(&args, 0, "net.conn", name, line)?;
                let max = self.int_arg(&args, 1, name, line)?;
                let timeout = self.int_arg(&args, 2, name, line)?;
                if !(1..=16 * 1024 * 1024).contains(&max) {
                    return self.error("tcp_recv max bytes must be 1..=16777216", line);
                }
                let stream = match self.net_state.tcp.get_mut(&id) {
                    Some(stream) => stream,
                    None => {
                        return Ok(Value::Err(Box::new(Value::string(
                            "connection is closed".to_string(),
                        ))))
                    }
                };
                apply_timeout(stream, timeout);
                let mut buffer = vec![0u8; max as usize];
                match stream.read(&mut buffer) {
                    Ok(0) => Ok(Value::Err(Box::new(Value::string("connection closed".to_string())))),
                    Ok(read) => Ok(Value::Ok(Box::new(Value::string(
                        String::from_utf8_lossy(&buffer[..read]).to_string(),
                    )))),
                    Err(error) => Ok(Value::Err(Box::new(Value::string(io_error(&error))))),
                }
            }
            "tcp_recv_line" => {
                let id = self.handle_arg(&args, 0, "net.conn", name, line)?;
                let max = self.int_arg(&args, 1, name, line)?;
                let timeout = self.int_arg(&args, 2, name, line)?;
                if !(1..=16 * 1024 * 1024).contains(&max) {
                    return self.error("tcp_recv_line max bytes must be 1..=16777216", line);
                }
                let stream = match self.net_state.tcp.get_mut(&id) {
                    Some(stream) => stream,
                    None => {
                        return Ok(Value::Err(Box::new(Value::string(
                            "connection is closed".to_string(),
                        ))))
                    }
                };
                apply_timeout(stream, timeout);
                match read_line(stream, max as usize) {
                    Ok(line) => Ok(Value::Ok(Box::new(Value::string(line)))),
                    Err(message) => Ok(Value::Err(Box::new(Value::string(message)))),
                }
            }
            "tcp_close" => {
                let id = self.handle_arg(&args, 0, "net.conn", name, line)?;
                let closed = self.net_state.tcp.remove(&id).is_some();
                Ok(Value::Ok(Box::new(Value::Bool(closed))))
            }
            "tcp_listen" => {
                let host = self.string_arg(&args, 0, name, line)?;
                let port = self.int_arg(&args, 1, name, line)?;
                if !(0..=65535).contains(&port) {
                    return self.error("tcp_listen port must be 0..65535", line);
                }
                match TcpListener::bind((host.as_str(), port as u16)) {
                    Ok(listener) => {
                        let local = listener
                            .local_addr()
                            .map(|addr| addr.to_string())
                            .unwrap_or_else(|_| format!("{}:{}", host, port));
                        let id = self.net_state.next_id();
                        self.net_state.listeners.insert(id, listener);
                        Ok(Value::Ok(Box::new(handle_value("net.listener", id, &local))))
                    }
                    Err(error) => Ok(Value::Err(Box::new(Value::string(error.to_string())))),
                }
            }
            "tcp_accept" => {
                let id = self.handle_arg(&args, 0, "net.listener", name, line)?;
                let timeout = self.int_arg(&args, 1, name, line)?;
                let listener = match self.net_state.listeners.get(&id) {
                    Some(listener) => listener,
                    None => {
                        return Ok(Value::Err(Box::new(Value::string(
                            "listener is closed".to_string(),
                        ))))
                    }
                };
                match accept_tcp(listener, timeout) {
                    Ok(stream) => {
                        let remote = stream
                            .peer_addr()
                            .map(|addr| addr.to_string())
                            .unwrap_or_default();
                        let id = self.net_state.next_id();
                        self.net_state.tcp.insert(id, stream);
                        Ok(Value::Ok(Box::new(handle_value("net.conn", id, &remote))))
                    }
                    Err(message) => Ok(Value::Err(Box::new(Value::string(message)))),
                }
            }
            "tcp_listener_close" => {
                let id = self.handle_arg(&args, 0, "net.listener", name, line)?;
                let closed = self.net_state.listeners.remove(&id).is_some();
                Ok(Value::Ok(Box::new(Value::Bool(closed))))
            }
            "udp_bind" => {
                let host = self.string_arg(&args, 0, name, line)?;
                let port = self.int_arg(&args, 1, name, line)?;
                if !(0..=65535).contains(&port) {
                    return self.error("udp_bind port must be 0..65535", line);
                }
                match UdpSocket::bind((host.as_str(), port as u16)) {
                    Ok(socket) => {
                        let local = socket
                            .local_addr()
                            .map(|addr| addr.to_string())
                            .unwrap_or_else(|_| format!("{}:{}", host, port));
                        let id = self.net_state.next_id();
                        self.net_state.udp.insert(id, socket);
                        Ok(Value::Ok(Box::new(handle_value("net.socket", id, &local))))
                    }
                    Err(error) => Ok(Value::Err(Box::new(Value::string(error.to_string())))),
                }
            }
            "udp_send" => {
                let id = self.handle_arg(&args, 0, "net.socket", name, line)?;
                let host = self.string_arg(&args, 1, name, line)?;
                let port = self.int_arg(&args, 2, name, line)?;
                let text = self.string_arg(&args, 3, name, line)?;
                if !(0..=65535).contains(&port) {
                    return self.error("udp_send port must be 0..65535", line);
                }
                let socket = match self.net_state.udp.get(&id) {
                    Some(socket) => socket,
                    None => {
                        return Ok(Value::Err(Box::new(Value::string(
                            "socket is closed".to_string(),
                        ))))
                    }
                };
                match socket.send_to(text.as_bytes(), (host.as_str(), port as u16)) {
                    Ok(bytes) => Ok(Value::Ok(Box::new(Value::Int(bytes as i64)))),
                    Err(error) => Ok(Value::Err(Box::new(Value::string(error.to_string())))),
                }
            }
            "udp_recv" => {
                let id = self.handle_arg(&args, 0, "net.socket", name, line)?;
                let max = self.int_arg(&args, 1, name, line)?;
                let timeout = self.int_arg(&args, 2, name, line)?;
                if !(1..=65535).contains(&max) {
                    return self.error("udp_recv max bytes must be 1..=65535", line);
                }
                let socket = match self.net_state.udp.get(&id) {
                    Some(socket) => socket,
                    None => {
                        return Ok(Value::Err(Box::new(Value::string(
                            "socket is closed".to_string(),
                        ))))
                    }
                };
                apply_socket_timeout(socket, timeout);
                let mut buffer = vec![0u8; max as usize];
                match socket.recv_from(&mut buffer) {
                    Ok((read, address)) => Ok(Value::Ok(Box::new(Value::ary(
                        "net.packet",
                        vec![
                            (
                                "data".to_string(),
                                Value::string(String::from_utf8_lossy(&buffer[..read]).to_string()),
                            ),
                            ("host".to_string(), Value::string(address.ip().to_string())),
                            ("port".to_string(), Value::Int(address.port() as i64)),
                        ],
                    )))),
                    Err(error) => Ok(Value::Err(Box::new(Value::string(io_error(&error))))),
                }
            }
            "udp_close" => {
                let id = self.handle_arg(&args, 0, "net.socket", name, line)?;
                let closed = self.net_state.udp.remove(&id).is_some();
                Ok(Value::Ok(Box::new(Value::Bool(closed))))
            }
            _ => self.error(format!("std/net has no function '{}'", name), line),
        }
    }

    pub(crate) fn ws_module(
        &mut self,
        name: &str,
        args: Vec<Value>,
        line: u32,
    ) -> FlowResult<Value> {
        match name {
            "serve" => {
                let port = self.int_arg(&args, 0, name, line)?;
                if !(0..=65535).contains(&port) {
                    return self.error("ws.serve port must be 0..65535", line);
                }
                let handler = match args.get(1) {
                    Some(handler @ Value::Func(_)) => handler.clone(),
                    _ => return self.error("ws.serve expects a handler function", line),
                };
                self.serve_ws(port as u16, handler, line)
            }
            "connect" => {
                let url = self.string_arg(&args, 0, name, line)?;
                let timeout = self.int_arg(&args, 1, name, line)?;
                match ws_connect(&url, timeout) {
                    Ok((stream, remote)) => {
                        let id = self.net_state.next_id();
                        self.net_state.ws.insert(id, WsConn::new(stream, true));
                        Ok(Value::Ok(Box::new(handle_value("ws.conn", id, &remote))))
                    }
                    Err(message) => Ok(Value::Err(Box::new(Value::string(message)))),
                }
            }
            "send" => {
                let id = self.handle_arg(&args, 0, "ws.conn", name, line)?;
                let text = self.string_arg(&args, 1, name, line)?;
                match self.net_state.ws.get_mut(&id) {
                    Some(conn) => match conn.send_text(&text) {
                        Ok(()) => Ok(Value::Ok(Box::new(Value::Int(text.len() as i64)))),
                        Err(message) => Ok(Value::Err(Box::new(Value::string(message)))),
                    },
                    None => Ok(Value::Err(Box::new(Value::string(
                        "connection is closed".to_string(),
                    )))),
                }
            }
            "recv" => {
                let id = self.handle_arg(&args, 0, "ws.conn", name, line)?;
                let timeout = self.int_arg(&args, 1, name, line)?;
                match self.net_state.ws.get_mut(&id) {
                    Some(conn) => match conn.recv_text(timeout) {
                        Ok(text) => Ok(Value::Ok(Box::new(Value::string(text)))),
                        Err(message) => Ok(Value::Err(Box::new(Value::string(message)))),
                    },
                    None => Ok(Value::Err(Box::new(Value::string(
                        "connection is closed".to_string(),
                    )))),
                }
            }
            "ping" => {
                let id = self.handle_arg(&args, 0, "ws.conn", name, line)?;
                match self.net_state.ws.get_mut(&id) {
                    Some(conn) => match conn.send_control(0x9, &[]) {
                        Ok(()) => Ok(Value::Ok(Box::new(Value::Bool(true)))),
                        Err(message) => Ok(Value::Err(Box::new(Value::string(message)))),
                    },
                    None => Ok(Value::Err(Box::new(Value::string(
                        "connection is closed".to_string(),
                    )))),
                }
            }
            "close" => {
                let id = self.handle_arg(&args, 0, "ws.conn", name, line)?;
                let closed = match self.net_state.ws.remove(&id) {
                    Some(mut conn) => {
                        let _ = conn.send_control(0x8, &[]);
                        true
                    }
                    None => false,
                };
                Ok(Value::Ok(Box::new(Value::Bool(closed))))
            }
            _ => self.error(format!("std/ws has no function '{}'", name), line),
        }
    }

    fn serve_ws(&mut self, port: u16, handler: Value, line: u32) -> FlowResult<Value> {
        let host = self
            .http_host
            .clone()
            .unwrap_or_else(|| "127.0.0.1".to_string());
        let listener = match TcpListener::bind((host.as_str(), port)) {
            Ok(listener) => listener,
            Err(error) => {
                return self.error(format!("cannot bind {}:{}: {}", host, port, error), line);
            }
        };
        let shown = if host.contains(':') && !host.starts_with('[') {
            format!("[{}]", host)
        } else {
            host
        };
        self.say(
            format!("Rip WebSocket server listening on ws://{}:{}", shown, port),
            format!("Rip WebSocket 服务器已启动：ws://{}:{}", shown, port),
        );
        for stream in listener.incoming() {
            let mut stream = match stream {
                Ok(stream) => stream,
                Err(error) => {
                    eprintln!(
                        "{}",
                        if self.language.is_chinese() {
                            format!("[ws] 接受连接失败：{}", error)
                        } else {
                            format!("[ws] accept error: {}", error)
                        }
                    );
                    continue;
                }
            };
            let remote = stream
                .peer_addr()
                .map(|addr| addr.to_string())
                .unwrap_or_default();
            if let Err(message) = ws_handshake_server(&mut stream) {
                eprintln!(
                    "{}",
                    if self.language.is_chinese() {
                        format!("[ws] 与 {} 握手失败：{}", remote, message)
                    } else {
                        format!("[ws] handshake failed with {}: {}", remote, message)
                    }
                );
                continue;
            }
            let id = self.net_state.next_id();
            self.net_state.ws.insert(id, WsConn::new(WsStream::Plain(stream), false));
            let connection = handle_value("ws.conn", id, &remote);
            let outcome = match &handler {
                Value::Func(func) => self.call_value(func, vec![connection], line),
                _ => self.error("ws.serve handler is not callable", line),
            };
            self.net_state.ws.remove(&id);
            if let Err(Flow::Error(error)) = outcome {
                eprintln!(
                    "{}",
                    if self.language.is_chinese() {
                        format!("[ws] 处理器错误：{}", error.message)
                    } else {
                        format!("[ws] handler error: {}", error.message)
                    }
                );
            }
        }
        Ok(Value::Null)
    }

    fn handle_arg(
        &mut self,
        args: &[Value],
        index: usize,
        class: &str,
        function: &str,
        line: u32,
    ) -> FlowResult<i64> {
        let id = match args.get(index) {
            Some(Value::Ary(ary)) => {
                let ary = ary.borrow();
                if ary.class == class {
                    ary.get("id").and_then(|value| value.as_int())
                } else {
                    None
                }
            }
            _ => None,
        };
        match id {
            Some(id) => Ok(id),
            None => self.error(format!("{} expects {}", function, class), line),
        }
    }
}

fn handle_value(class: &str, id: i64, detail: &str) -> Value {
    Value::ary(
        class,
        vec![
            ("id".to_string(), Value::Int(id)),
            ("detail".to_string(), Value::string(detail)),
        ],
    )
}

fn connect_tcp(address: &str, timeout_ms: i64) -> Result<TcpStream, String> {
    if timeout_ms <= 0 {
        return TcpStream::connect(address).map_err(|error| error.to_string());
    }
    let duration = Duration::from_millis(timeout_ms as u64);
    let addresses = match address.to_socket_addrs() {
        Ok(addresses) => addresses,
        Err(error) => return Err(error.to_string()),
    };
    let mut last = "no address resolved".to_string();
    for address in addresses {
        match TcpStream::connect_timeout(&address, duration) {
            Ok(stream) => return Ok(stream),
            Err(error) => last = error.to_string(),
        }
    }
    Err(last)
}

fn accept_tcp(listener: &TcpListener, timeout_ms: i64) -> Result<TcpStream, String> {
    if timeout_ms <= 0 {
        return listener.accept().map(|(stream, _)| stream).map_err(|error| error.to_string());
    }
    if let Err(error) = listener.set_nonblocking(true) {
        return Err(error.to_string());
    }
    let deadline = Instant::now() + Duration::from_millis(timeout_ms as u64);
    loop {
        match listener.accept() {
            Ok((stream, _)) => {
                let _ = stream.set_nonblocking(false);
                return Ok(stream);
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                if Instant::now() >= deadline {
                    return Err("timeout".to_string());
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(error) => return Err(error.to_string()),
        }
    }
}

fn read_line(stream: &mut TcpStream, max: usize) -> Result<String, String> {
    let mut line = Vec::new();
    let mut byte = [0u8; 1];
    loop {
        match stream.read(&mut byte) {
            Ok(0) => {
                return Err(if line.is_empty() {
                    "connection closed".to_string()
                } else {
                    String::from_utf8_lossy(&line).to_string()
                })
            }
            Ok(_) => {
                if byte[0] == b'\n' {
                    if line.last() == Some(&b'\r') {
                        line.pop();
                    }
                    return Ok(String::from_utf8_lossy(&line).to_string());
                }
                line.push(byte[0]);
                if line.len() >= max {
                    return Ok(String::from_utf8_lossy(&line).to_string());
                }
            }
            Err(error) => return Err(io_error(&error)),
        }
    }
}

fn apply_timeout(stream: &mut TcpStream, timeout_ms: i64) {
    let timeout = if timeout_ms > 0 {
        Some(Duration::from_millis(timeout_ms as u64))
    } else {
        None
    };
    let _ = stream.set_read_timeout(timeout);
}

fn apply_socket_timeout(socket: &UdpSocket, timeout_ms: i64) {
    let timeout = if timeout_ms > 0 {
        Some(Duration::from_millis(timeout_ms as u64))
    } else {
        None
    };
    let _ = socket.set_read_timeout(timeout);
}

fn io_error(error: &io::Error) -> String {
    match error.kind() {
        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut => "timeout".to_string(),
        io::ErrorKind::UnexpectedEof => "connection closed".to_string(),
        _ => error.to_string(),
    }
}

/// WebSocket 底层字节流：明文 TCP 或 rustls 包装的 TLS TCP（wss://）。
pub(crate) enum WsStream {
    Plain(TcpStream),
    Tls(Box<rustls::StreamOwned<rustls::ClientConnection, TcpStream>>),
}

impl Read for WsStream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match self {
            WsStream::Plain(stream) => stream.read(buf),
            WsStream::Tls(stream) => stream.read(buf),
        }
    }
}

impl Write for WsStream {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        match self {
            WsStream::Plain(stream) => stream.write(buf),
            WsStream::Tls(stream) => stream.write(buf),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        match self {
            WsStream::Plain(stream) => stream.flush(),
            WsStream::Tls(stream) => stream.flush(),
        }
    }
}

impl WsStream {
    fn set_read_timeout(&mut self, timeout_ms: i64) {
        let timeout = if timeout_ms > 0 {
            Some(Duration::from_millis(timeout_ms as u64))
        } else {
            None
        };
        match self {
            WsStream::Plain(stream) => {
                let _ = stream.set_read_timeout(timeout);
            }
            WsStream::Tls(stream) => {
                let _ = stream.sock.set_read_timeout(timeout);
            }
        }
    }
}

/// WebSocket 帧连接。客户端发送的帧必须掩码，服务端发送的帧不能掩码。
pub(crate) struct WsConn {
    stream: WsStream,
    mask_frames: bool,
    closed: bool,
}

impl WsConn {
    fn new(stream: WsStream, mask_frames: bool) -> Self {
        WsConn {
            stream,
            mask_frames,
            closed: false,
        }
    }

    fn send_text(&mut self, text: &str) -> Result<(), String> {
        self.send_frame(0x1, text.as_bytes())
    }

    fn send_control(&mut self, opcode: u8, payload: &[u8]) -> Result<(), String> {
        self.send_frame(opcode, payload)
    }

    fn send_frame(&mut self, opcode: u8, payload: &[u8]) -> Result<(), String> {
        if self.closed && opcode != 0x8 {
            return Err("connection is closed".to_string());
        }
        let mut frame = Vec::with_capacity(payload.len() + 14);
        frame.push(0x80 | opcode);
        let mask_bit = if self.mask_frames { 0x80 } else { 0 };
        if payload.len() < 126 {
            frame.push(mask_bit | payload.len() as u8);
        } else if payload.len() <= u16::MAX as usize {
            frame.push(mask_bit | 126);
            frame.extend_from_slice(&(payload.len() as u16).to_be_bytes());
        } else {
            frame.push(mask_bit | 127);
            frame.extend_from_slice(&(payload.len() as u64).to_be_bytes());
        }
        if self.mask_frames {
            let mut mask = [0u8; 4];
            getrandom::getrandom(&mut mask)
                .map_err(|error| format!("cannot read random source: {}", error))?;
            frame.extend_from_slice(&mask);
            for (index, byte) in payload.iter().enumerate() {
                frame.push(byte ^ mask[index % 4]);
            }
        } else {
            frame.extend_from_slice(payload);
        }
        self.stream
            .write_all(&frame)
            .and_then(|_| self.stream.flush())
            .map_err(|error| error.to_string())?;
        if opcode == 0x8 {
            self.closed = true;
        }
        Ok(())
    }

    /// 读取一条完整消息；ping 自动回 pong，close 回 close 并返回错误。
    fn recv_text(&mut self, timeout_ms: i64) -> Result<String, String> {
        self.stream.set_read_timeout(timeout_ms);
        let mut message: Vec<u8> = Vec::new();
        loop {
            let (fin, opcode, payload) = self.read_frame()?;
            // 控制帧必须是单帧且 <=125 字节，否则按协议错误关闭（防 ping 放大）。
            if opcode >= 0x8 && (!fin || payload.len() > 125) {
                let _ = self.send_frame(0x8, &1002u16.to_be_bytes());
                return Err("malformed WebSocket control frame".to_string());
            }
            match opcode {
                0x0 => {
                    message.extend_from_slice(&payload);
                }
                0x1 | 0x2 => {
                    message = payload;
                }
                0x8 => {
                    let _ = self.send_frame(0x8, &payload[..payload.len().min(125)]);
                    return Err("connection closed".to_string());
                }
                0x9 => {
                    self.send_frame(0xA, &payload)?;
                    continue;
                }
                0xA => continue,
                _ => {
                    return Err(format!("unsupported WebSocket opcode {}", opcode));
                }
            }
            // 分片消息同样受 16 MiB 上限约束，超限发 1009 后关闭。
            if message.len() > 16 * 1024 * 1024 {
                let _ = self.send_frame(0x8, &1009u16.to_be_bytes());
                return Err("WebSocket message is too large (max 16 MiB)".to_string());
            }
            if fin {
                return Ok(String::from_utf8_lossy(&message).to_string());
            }
        }
    }

    fn read_frame(&mut self) -> Result<(bool, u8, Vec<u8>), String> {
        let mut header = [0u8; 2];
        read_exact(&mut self.stream, &mut header)?;
        let fin = header[0] & 0x80 != 0;
        let opcode = header[0] & 0x0f;
        let masked = header[1] & 0x80 != 0;
        // RFC 6455 5.1：客户端→服务端帧必须掩码，服务端→客户端帧必须不掩码。
        if masked == self.mask_frames {
            let _ = self.send_frame(0x8, &1002u16.to_be_bytes());
            return Err(if self.mask_frames {
                "WebSocket server sent a masked frame".to_string()
            } else {
                "WebSocket client frame is not masked".to_string()
            });
        }
        let mut length = (header[1] & 0x7f) as u64;
        if length == 126 {
            let mut extended = [0u8; 2];
            read_exact(&mut self.stream, &mut extended)?;
            length = u16::from_be_bytes(extended) as u64;
        } else if length == 127 {
            let mut extended = [0u8; 8];
            read_exact(&mut self.stream, &mut extended)?;
            length = u64::from_be_bytes(extended);
        }
        if length > 16 * 1024 * 1024 {
            return Err("WebSocket frame is too large (max 16 MiB)".to_string());
        }
        let mut mask = [0u8; 4];
        if masked {
            read_exact(&mut self.stream, &mut mask)?;
        }
        let mut payload = vec![0u8; length as usize];
        if !payload.is_empty() {
            read_exact(&mut self.stream, &mut payload)?;
        }
        if masked {
            for (index, byte) in payload.iter_mut().enumerate() {
                *byte ^= mask[index % 4];
            }
        }
        Ok((fin, opcode, payload))
    }
}

fn read_exact(stream: &mut impl Read, buffer: &mut [u8]) -> Result<(), String> {
    stream
        .read_exact(buffer)
        .map_err(|error| io_error(&error))
}

const WS_GUID: &str = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11";

fn ws_accept_key(key: &str) -> String {
    let mut hasher = sha1::Sha1::new();
    hasher.update(key.as_bytes());
    hasher.update(WS_GUID.as_bytes());
    base64::engine::general_purpose::STANDARD.encode(hasher.finalize())
}

fn ws_handshake_server(stream: &mut TcpStream) -> Result<(), String> {
    // 握手阶段读写超时：防止慢速客户端无限占用连接；成功后恢复为阻塞等待帧。
    let handshake_timeout = std::time::Duration::from_secs(10);
    let _ = stream.set_read_timeout(Some(handshake_timeout));
    let _ = stream.set_write_timeout(Some(handshake_timeout));
    let result = ws_handshake_inner(stream);
    if result.is_ok() {
        let _ = stream.set_read_timeout(None);
        let _ = stream.set_write_timeout(None);
    }
    result
}

fn ws_host_of(value: &str) -> String {
    let rest = value
        .split_once("://")
        .map(|(_, rest)| rest)
        .unwrap_or(value);
    rest.split('/').next().unwrap_or(rest).to_lowercase()
}

fn ws_handshake_inner(stream: &mut TcpStream) -> Result<(), String> {
    let head = read_http_head(stream)?;
    let mut lines = head.lines();
    let request_line = lines.next().unwrap_or_default();
    if !request_line.contains("GET ") {
        let _ = stream.write_all(
            b"HTTP/1.1 400 Bad Request\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
        );
        return Err("handshake must be a GET request".to_string());
    }
    let mut key = None;
    let mut origin = None;
    let mut host = None;
    for line in lines {
        if let Some((name, value)) = line.split_once(':') {
            let name = name.trim();
            if name.eq_ignore_ascii_case("sec-websocket-key") {
                key = Some(value.trim().to_string());
            } else if name.eq_ignore_ascii_case("origin") {
                origin = Some(value.trim().to_string());
            } else if name.eq_ignore_ascii_case("host") {
                host = Some(value.trim().to_string());
            }
        }
    }
    let key = match key {
        Some(key) if !key.is_empty() => key,
        _ => {
            let _ = stream.write_all(
                b"HTTP/1.1 400 Bad Request\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            );
            return Err("missing Sec-WebSocket-Key".to_string());
        }
    };
    // 跨站 WebSocket 劫持防护：浏览器必带 Origin，与 Host 不同源即拒绝；
    // 非浏览器客户端不带 Origin，不受影响。
    if let Some(origin) = &origin {
        let same_host = host
            .as_deref()
            .is_some_and(|host| ws_host_of(origin) == ws_host_of(host));
        if !same_host {
            let _ = stream.write_all(
                b"HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            );
            return Err(format!("cross-origin handshake rejected: {}", origin));
        }
    }
    let accept = ws_accept_key(&key);
    let response = format!(
        "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: {}\r\n\r\n",
        accept
    );
    stream
        .write_all(response.as_bytes())
        .and_then(|_| stream.flush())
        .map_err(|error| error.to_string())
}

fn ws_connect(url: &str, timeout_ms: i64) -> Result<(WsStream, String), String> {
    let (host, port, path, tls) = parse_ws_url(url)?;
    let address = format!("{}:{}", host, port);
    let tcp = connect_tcp(&address, timeout_ms)?;
    let remote = tcp
        .peer_addr()
        .map(|addr| addr.to_string())
        .unwrap_or(address);
    let mut stream = if tls {
        crate::tls::install_crypto_provider();
        let mut roots = rustls::RootCertStore::empty();
        roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
        let config = rustls::ClientConfig::builder()
            .with_root_certificates(roots)
            .with_no_client_auth();
        let server_name = rustls::pki_types::ServerName::try_from(host.clone())
            .map_err(|_| format!("invalid WebSocket host name '{}'", host))?;
        let connection = rustls::ClientConnection::new(std::sync::Arc::new(config), server_name)
            .map_err(|error| error.to_string())?;
        WsStream::Tls(Box::new(rustls::StreamOwned::new(connection, tcp)))
    } else {
        WsStream::Plain(tcp)
    };
    let mut nonce = [0u8; 16];
    getrandom::getrandom(&mut nonce)
        .map_err(|error| format!("cannot read random source: {}", error))?;
    let key = base64::engine::general_purpose::STANDARD.encode(nonce);
    let host_header = if port == 80 || (tls && port == 443) {
        host.clone()
    } else {
        format!("{}:{}", host, port)
    };
    let request = format!(
        "GET {} HTTP/1.1\r\nHost: {}\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: {}\r\nSec-WebSocket-Version: 13\r\n\r\n",
        path, host_header, key
    );
    stream
        .write_all(request.as_bytes())
        .and_then(|_| stream.flush())
        .map_err(|error| error.to_string())?;
    let head = read_http_head(&mut stream)?;
    let mut lines = head.lines();
    let status = lines.next().unwrap_or_default();
    if !status.contains(" 101 ") {
        return Err(format!("WebSocket handshake failed: {}", status.trim()));
    }
    let expected = ws_accept_key(&key);
    for line in lines {
        if let Some((name, value)) = line.split_once(':') {
            if name.trim().eq_ignore_ascii_case("sec-websocket-accept")
                && value.trim() != expected
            {
                return Err("WebSocket handshake failed: invalid Sec-WebSocket-Accept".to_string());
            }
        }
    }
    Ok((stream, remote))
}

fn parse_ws_url(url: &str) -> Result<(String, u16, String, bool), String> {
    let (tls, rest) = if let Some(rest) = url.strip_prefix("wss://") {
        (true, rest)
    } else if let Some(rest) = url.strip_prefix("ws://") {
        (false, rest)
    } else {
        return Err("WebSocket URL must start with ws:// or wss://".to_string());
    };
    let default_port = if tls { 443 } else { 80 };
    let (authority, path) = match rest.find('/') {
        Some(index) => (&rest[..index], &rest[index..]),
        None => (rest, "/"),
    };
    if authority.is_empty() {
        return Err("WebSocket URL has no host".to_string());
    }
    let (host, port) = if let Some(rest) = authority.strip_prefix('[') {
        match rest.split_once(']') {
            Some((host, tail)) => {
                let port = tail.strip_prefix(':').and_then(|value| value.parse().ok());
                (host.to_string(), port.unwrap_or(default_port))
            }
            None => return Err("invalid IPv6 WebSocket URL".to_string()),
        }
    } else {
        match authority.rsplit_once(':') {
            Some((host, port)) => match port.parse::<u16>() {
                Ok(port) => (host.to_string(), port),
                Err(_) => return Err(format!("invalid port '{}'", port)),
            },
            None => (authority.to_string(), default_port),
        }
    };
    let path = if path.is_empty() {
        "/".to_string()
    } else {
        path.to_string()
    };
    Ok((host, port, path, tls))
}

/// 逐字节读取 HTTP 头部，避免读缓冲吞掉其后的 WebSocket 帧。
fn read_http_head(stream: &mut impl Read) -> Result<String, String> {
    let mut buffer = Vec::new();
    let mut byte = [0u8; 1];
    loop {
        match stream.read(&mut byte) {
            Ok(0) => return Err("connection closed during handshake".to_string()),
            Ok(_) => {
                buffer.push(byte[0]);
                if buffer.len() >= 4 && &buffer[buffer.len() - 4..] == b"\r\n\r\n" {
                    return String::from_utf8(buffer)
                        .map_err(|_| "handshake contains invalid UTF-8".to_string());
                }
                if buffer.len() > 16 * 1024 {
                    return Err("handshake is too large".to_string());
                }
            }
            Err(error) => return Err(io_error(&error)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::parse_ws_url;

    #[test]
    fn parse_ws_url_accepts_ws_and_wss() {
        assert_eq!(
            parse_ws_url("ws://example.com/chat").unwrap(),
            ("example.com".to_string(), 80, "/chat".to_string(), false)
        );
        assert_eq!(
            parse_ws_url("wss://example.com").unwrap(),
            ("example.com".to_string(), 443, "/".to_string(), true)
        );
        assert_eq!(
            parse_ws_url("wss://example.com:8443/ws").unwrap(),
            ("example.com".to_string(), 8443, "/ws".to_string(), true)
        );
        assert_eq!(
            parse_ws_url("ws://127.0.0.1:9001/").unwrap(),
            ("127.0.0.1".to_string(), 9001, "/".to_string(), false)
        );
        assert_eq!(
            parse_ws_url("wss://[::1]:9443/api").unwrap(),
            ("::1".to_string(), 9443, "/api".to_string(), true)
        );
        assert!(parse_ws_url("http://example.com").is_err());
    }

    #[test]
    fn ws_conn_enforces_mask_by_role() {
        use super::{WsConn, WsStream};
        use std::io::Write;
        use std::net::{TcpListener, TcpStream};

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let client = TcpStream::connect(addr).unwrap();
        let (server_side, _) = listener.accept().unwrap();
        // 服务端角色：必须拒绝对端发来的未掩码帧。
        let mut server_conn = WsConn::new(WsStream::Plain(server_side), false);
        let mut raw_peer = client.try_clone().unwrap();
        raw_peer.write_all(&[0x81, 0x02, b'h', b'i']).unwrap();
        let error = server_conn.recv_text(2000).unwrap_err();
        assert!(error.contains("not masked"), "{error}");

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let client2 = TcpStream::connect(addr).unwrap();
        let (server2, _) = listener.accept().unwrap();
        // 客户端角色：必须拒绝对端发来的已掩码帧。
        let mut client_conn = WsConn::new(WsStream::Plain(client2), true);
        let mut raw_peer = server2;
        raw_peer.write_all(&[0x81, 0x82, 1, 2, 3, 4, b'h' ^ 1, b'i' ^ 2]).unwrap();
        let error = client_conn.recv_text(2000).unwrap_err();
        assert!(error.contains("masked frame"), "{error}");
    }
}
