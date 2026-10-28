//! HTTPS/TLS：证书加载与加密连接封装。

use std::fs::File;
use std::io::{self, BufReader, Read, Write};
use std::net::TcpStream;
use std::sync::{Arc, Once};

/// HTTP 连接：明文 TCP 或 rustls 加密流，二者用同一套读写接口。
pub(crate) enum HttpStream {
    Plain(TcpStream),
    Tls(Box<rustls::StreamOwned<rustls::ServerConnection, TcpStream>>),
}

impl HttpStream {
    pub(crate) fn plain(stream: TcpStream) -> Self {
        HttpStream::Plain(stream)
    }

    /// 建立 TLS 服务端连接；握手在首次读写时完成。
    pub(crate) fn tls(
        config: Arc<rustls::ServerConfig>,
        stream: TcpStream,
    ) -> Result<Self, String> {
        install_crypto_provider();
        let connection =
            rustls::ServerConnection::new(config).map_err(|error| error.to_string())?;
        Ok(HttpStream::Tls(Box::new(rustls::StreamOwned::new(
            connection, stream,
        ))))
    }

    /// 仅供解释器侧的同步 HTTP 服务调整连接超时（AOT 宿主不用这套路径）。
    #[allow(dead_code)]
    pub(crate) fn set_read_timeout(
        &self,
        timeout: Option<std::time::Duration>,
    ) -> io::Result<()> {
        match self {
            HttpStream::Plain(stream) => stream.set_read_timeout(timeout),
            HttpStream::Tls(stream) => stream.sock.set_read_timeout(timeout),
        }
    }

    #[allow(dead_code)]
    pub(crate) fn set_write_timeout(
        &self,
        timeout: Option<std::time::Duration>,
    ) -> io::Result<()> {
        match self {
            HttpStream::Plain(stream) => stream.set_write_timeout(timeout),
            HttpStream::Tls(stream) => stream.sock.set_write_timeout(timeout),
        }
    }
}

impl Read for HttpStream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match self {
            HttpStream::Plain(stream) => stream.read(buf),
            HttpStream::Tls(stream) => stream.read(buf),
        }
    }
}

impl Write for HttpStream {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        match self {
            HttpStream::Plain(stream) => stream.write(buf),
            HttpStream::Tls(stream) => stream.write(buf),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        match self {
            HttpStream::Plain(stream) => stream.flush(),
            HttpStream::Tls(stream) => stream.flush(),
        }
    }
}

pub(crate) fn install_crypto_provider() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let _ = rustls::crypto::ring::default_provider().install_default();
    });
}

/// 读取 PEM 证书链与私钥，构造 rustls 服务端配置。
pub(crate) fn server_config(
    cert_path: &str,
    key_path: &str,
) -> Result<Arc<rustls::ServerConfig>, String> {
    install_crypto_provider();
    let cert_file = File::open(cert_path)
        .map_err(|error| format!("cannot open certificate {}: {}", cert_path, error))?;
    let mut cert_reader = BufReader::new(cert_file);
    let certs = rustls_pemfile::certs(&mut cert_reader)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("cannot read certificate {}: {}", cert_path, error))?;
    if certs.is_empty() {
        return Err(format!("certificate {} has no PEM certificate", cert_path));
    }
    let key_file = File::open(key_path)
        .map_err(|error| format!("cannot open private key {}: {}", key_path, error))?;
    let mut key_reader = BufReader::new(key_file);
    let key = rustls_pemfile::private_key(&mut key_reader)
        .map_err(|error| format!("cannot read private key {}: {}", key_path, error))?
        .ok_or_else(|| format!("private key {} has no PEM key", key_path))?;
    let config = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(certs, key)
        .map_err(|error| format!("invalid TLS certificate or key: {}", error))?;
    Ok(Arc::new(config))
}
