//! Cấu hình mạng theo từng profile: proxy HTTP/SOCKS5 (kèm xác thực),
//! chống lộ IP thật qua WebRTC/QUIC, và bộ kiểm tra proxy/IP công khai.
//!
//! Lý do phải có relay nội bộ: Chromium (WebView2) không gửi `Proxy-Authorization`
//! từ URL `http://user:pass@host:port` nữa, và không có API nào để truyền
//! user/pass của proxy. Vì vậy app mở một listener 127.0.0.1 tạm thời, chèn
//! header xác thực rồi chuyển tiếp tới proxy thật. Nhờ đó user/pass không bao giờ
//! xuất hiện trong command line của tiến trình WebView2.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream, ToSocketAddrs};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ProxyConfig {
    #[serde(default)]
    pub enabled: bool,
    /// "http" (mặc định) hoặc "socks5"
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub host: String,
    #[serde(default)]
    pub port: u16,
    #[serde(default)]
    pub username: String,
    #[serde(default)]
    pub password: String,
    /// Danh sách bỏ qua proxy, ví dụ "*.local,localhost". Rỗng -> "<-loopback>".
    #[serde(default)]
    pub bypass: String,
}

impl ProxyConfig {
    pub fn kind_norm(&self) -> &'static str {
        if self.kind.eq_ignore_ascii_case("socks5") {
            "socks5"
        } else {
            "http"
        }
    }

    pub fn is_usable(&self) -> bool {
        self.enabled && !self.host.trim().is_empty() && self.port > 0
    }

    pub fn needs_auth(&self) -> bool {
        !self.username.is_empty() || !self.password.is_empty()
    }

    fn basic_auth(&self) -> String {
        format!(
            "Basic {}",
            base64_encode(format!("{}:{}", self.username, self.password).as_bytes())
        )
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ProxyTestResult {
    pub ok: bool,
    pub ip: String,
    pub latency_ms: u64,
    pub via: String,
    pub message: String,
}

/* ------------------------------- base64 ------------------------------- */

fn base64_encode(input: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(((input.len() + 2) / 3) * 4);
    for chunk in input.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = *chunk.get(1).unwrap_or(&0) as u32;
        let b2 = *chunk.get(2).unwrap_or(&0) as u32;
        let triple = (b0 << 16) | (b1 << 8) | b2;
        out.push(TABLE[((triple >> 18) & 0x3f) as usize] as char);
        out.push(TABLE[((triple >> 12) & 0x3f) as usize] as char);
        out.push(if chunk.len() > 1 {
            TABLE[((triple >> 6) & 0x3f) as usize] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            TABLE[(triple & 0x3f) as usize] as char
        } else {
            '='
        });
    }
    out
}

/* ------------------------------ TCP helpers ---------------------------- */

fn connect_timeout(host: &str, port: u16, timeout: Duration) -> Result<TcpStream, String> {
    let addrs: Vec<_> = (host, port)
        .to_socket_addrs()
        .map_err(|e| format!("Không phân giải được {}:{} ({})", host, port, e))?
        .collect();
    let mut last_err = String::from("Không có địa chỉ hợp lệ");
    for addr in addrs {
        match TcpStream::connect_timeout(&addr, timeout) {
            Ok(stream) => {
                let _ = stream.set_nodelay(true);
                return Ok(stream);
            }
            Err(e) => last_err = e.to_string(),
        }
    }
    Err(format!("Không kết nối được {}:{} ({})", host, port, last_err))
}

fn read_until_headers(stream: &mut TcpStream, limit: usize) -> Result<Vec<u8>, String> {
    let mut buf: Vec<u8> = Vec::with_capacity(1024);
    let mut chunk = [0u8; 2048];
    loop {
        match stream.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => {
                buf.extend_from_slice(&chunk[..n]);
                if buf.windows(4).any(|w| w == b"\r\n\r\n") || buf.len() >= limit {
                    break;
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
            Err(e) if e.kind() == std::io::ErrorKind::TimedOut => break,
            Err(e) => return Err(e.to_string()),
        }
    }
    if buf.is_empty() {
        return Err("Kết nối đóng trước khi nhận phản hồi".to_string());
    }
    Ok(buf)
}

fn split_host_port(target: &str) -> (String, u16) {
    match target.rsplit_once(':') {
        Some((h, p)) => (h.trim_start_matches('[').trim_end_matches(']').to_string(), p.parse().unwrap_or(443)),
        None => (target.to_string(), 443),
    }
}

fn proxy_pipe(mut client: TcpStream, upstream: TcpStream) {
    let (mut up_read, mut up_write) = match (upstream.try_clone(), upstream.try_clone()) {
        (Ok(a), Ok(b)) => (a, b),
        _ => return,
    };
    let client_write = client.try_clone();
    let mut client_write = match client_write {
        Ok(c) => c,
        Err(_) => return,
    };
    let up_thread = std::thread::spawn(move || {
        let _ = std::io::copy(&mut up_read, &mut client_write);
        let _ = client_write.shutdown(Shutdown::Write);
    });
    let _ = std::io::copy(&mut client, &mut up_write);
    let _ = up_write.shutdown(Shutdown::Write);
    let _ = up_thread.join();
}

/* ------------------------------- SOCKS5 -------------------------------- */

fn socks5_connect(
    stream: &mut TcpStream,
    target: &str,
    username: &str,
    password: &str,
) -> Result<(), String> {
    let use_auth = !username.is_empty() || !password.is_empty();
    let greeting: &[u8] = if use_auth { &[0x05, 0x01, 0x02] } else { &[0x05, 0x01, 0x00] };
    stream.write_all(greeting).map_err(|e| e.to_string())?;
    let mut resp = [0u8; 2];
    stream.read_exact(&mut resp).map_err(|e| e.to_string())?;
    if resp[0] != 0x05 {
        return Err("Proxy SOCKS5 trả về phiên bản không hợp lệ".to_string());
    }
    if resp[1] == 0x02 {
        let u = username.as_bytes();
        let p = password.as_bytes();
        let mut msg = vec![0x01u8, u.len().min(255) as u8];
        msg.extend_from_slice(&u[..u.len().min(255)]);
        msg.push(p.len().min(255) as u8);
        msg.extend_from_slice(&p[..p.len().min(255)]);
        stream.write_all(&msg).map_err(|e| e.to_string())?;
        let mut auth_resp = [0u8; 2];
        stream.read_exact(&mut auth_resp).map_err(|e| e.to_string())?;
        if auth_resp[1] != 0x00 {
            return Err("SOCKS5 từ chối user/password".to_string());
        }
    } else if resp[1] != 0x00 {
        return Err("SOCKS5 không chấp nhận phương thức xác thực".to_string());
    }

    let (host, port) = split_host_port(target);
    let host_bytes = host.as_bytes();
    let mut req = vec![0x05u8, 0x01, 0x00, 0x03, host_bytes.len().min(255) as u8];
    req.extend_from_slice(&host_bytes[..host_bytes.len().min(255)]);
    req.extend_from_slice(&port.to_be_bytes());
    stream.write_all(&req).map_err(|e| e.to_string())?;

    let mut head = [0u8; 4];
    stream.read_exact(&mut head).map_err(|e| e.to_string())?;
    if head[1] != 0x00 {
        return Err(format!("SOCKS5 không mở được kênh (mã {})", head[1]));
    }
    match head[3] {
        0x01 => {
            let mut skip = [0u8; 6];
            let _ = stream.read_exact(&mut skip);
        }
        0x04 => {
            let mut skip = [0u8; 18];
            let _ = stream.read_exact(&mut skip);
        }
        0x03 => {
            let mut len = [0u8; 1];
            stream.read_exact(&mut len).map_err(|e| e.to_string())?;
            let mut skip = vec![0u8; len[0] as usize + 2];
            let _ = stream.read_exact(&mut skip);
        }
        _ => {}
    }
    Ok(())
}

/* -------------------------------- relay -------------------------------- */

fn relay_registry() -> &'static Mutex<HashMap<String, u16>> {
    static REGISTRY: OnceLock<Mutex<HashMap<String, u16>>> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Trả về cổng relay đã chạy cho cấu hình này (tạo mới nếu chưa có).
/// Mỗi relay chỉ chuyển tiếp tới đúng một proxy, nên khoá là (kind, host, port, hash user/pass).
fn ensure_relay(cfg: &ProxyConfig) -> Result<u16, String> {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    use std::hash::{Hash, Hasher};
    cfg.username.hash(&mut hasher);
    cfg.password.hash(&mut hasher);
    let auth_hash = hasher.finish();
    let key = format!("{}|{}|{}|{}", cfg.kind_norm(), cfg.host.trim(), cfg.port, auth_hash);

    if let Ok(map) = relay_registry().lock() {
        if let Some(port) = map.get(&key) {
            return Ok(*port);
        }
    }

    let listener = TcpListener::bind(("127.0.0.1", 0)).map_err(|e| e.to_string())?;
    let port = listener.local_addr().map_err(|e| e.to_string())?.port();
    let cfg_clone = cfg.clone();

    std::thread::spawn(move || {
        for incoming in listener.incoming() {
            let Ok(client) = incoming else { continue };
            let cfg = cfg_clone.clone();
            std::thread::spawn(move || {
                let _ = handle_relay_client(client, &cfg);
            });
        }
    });

    if let Ok(mut map) = relay_registry().lock() {
        map.insert(key, port);
    }
    Ok(port)
}

fn handle_relay_client(mut client: TcpStream, cfg: &ProxyConfig) -> Result<(), String> {
    let _ = client.set_read_timeout(Some(Duration::from_secs(30)));
    let head = read_until_headers(&mut client, 64 * 1024)?;
    let text = String::from_utf8_lossy(&head).to_string();
    let first_line = text.lines().next().unwrap_or("").trim().to_string();
    let mut parts = first_line.split_whitespace();
    let method = parts.next().unwrap_or("").to_uppercase();
    let target = parts.next().unwrap_or("").to_string();

    if method == "CONNECT" {
        let mut upstream = connect_timeout(&cfg.host.trim(), cfg.port, Duration::from_secs(15))?;
        if cfg.kind_norm() == "socks5" {
            socks5_connect(&mut upstream, &target, &cfg.username, &cfg.password)?;
        } else {
            let mut req = format!("CONNECT {target} HTTP/1.1\r\nHost: {target}\r\n");
            if cfg.needs_auth() {
                req.push_str(&format!("Proxy-Authorization: {}\r\n", cfg.basic_auth()));
            }
            req.push_str("\r\n");
            upstream.write_all(req.as_bytes()).map_err(|e| e.to_string())?;
            let _ = upstream.set_read_timeout(Some(Duration::from_secs(20)));
            let resp = read_until_headers(&mut upstream, 32 * 1024)?;
            let status_ok = resp
                .split(|b| *b == b' ')
                .nth(1)
                .map(|code| code == b"200")
                .unwrap_or(false);
            if !status_ok {
                let _ = client.write_all(&resp);
                return Ok(());
            }
            let _ = upstream.set_read_timeout(None);
        }
        let _ = client.set_read_timeout(None);
        client
            .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
            .map_err(|e| e.to_string())?;
        proxy_pipe(client, upstream);
        return Ok(());
    }

    // HTTP thuần (không TLS): chuyển tiếp nguyên request, chèn header xác thực.
    let mut upstream = connect_timeout(&cfg.host.trim(), cfg.port, Duration::from_secs(15))?;
    if cfg.kind_norm() == "socks5" {
        let host_port = absolute_uri_host_port(&target);
        socks5_connect(&mut upstream, &host_port, &cfg.username, &cfg.password)?;
    }
    let rewritten = rewrite_plain_request(&text, cfg);
    upstream
        .write_all(rewritten.as_bytes())
        .map_err(|e| e.to_string())?;
    proxy_pipe(client, upstream);
    Ok(())
}

fn absolute_uri_host_port(uri: &str) -> String {
    let without_scheme = uri.split("://").nth(1).unwrap_or(uri);
    let host_part = without_scheme.split('/').next().unwrap_or(without_scheme);
    if host_part.contains(':') {
        host_part.to_string()
    } else if uri.starts_with("https://") {
        format!("{host_part}:443")
    } else {
        format!("{host_part}:80")
    }
}

/// Bỏ header xác thực cũ (nếu có) và chèn `Proxy-Authorization` mới ngay sau dòng request.
fn rewrite_plain_request(text: &str, cfg: &ProxyConfig) -> String {
    let mut out = String::with_capacity(text.len() + 96);
    for (idx, line) in text.split("\r\n").enumerate() {
        if idx == 0 {
            out.push_str(line);
            out.push_str("\r\n");
            if cfg.needs_auth() && cfg.kind_norm() != "socks5" {
                out.push_str(&format!("Proxy-Authorization: {}\r\n", cfg.basic_auth()));
            }
            continue;
        }
        if line.to_ascii_lowercase().starts_with("proxy-authorization:") {
            continue;
        }
        out.push_str(line);
        out.push_str("\r\n");
    }
    out
}

/* ------------------------------ public API ----------------------------- */

/// Tham số dòng lệnh WebView2 cho profile. `None` = không dùng proxy.
pub fn build_proxy_args(cfg: &ProxyConfig) -> Result<Option<String>, String> {
    if !cfg.is_usable() {
        return Ok(None);
    }
    let server = if cfg.needs_auth() {
        let port = ensure_relay(cfg)?;
        format!("http://127.0.0.1:{port}")
    } else if cfg.kind_norm() == "socks5" {
        format!("socks5://{}:{}", cfg.host.trim(), cfg.port)
    } else {
        format!("http://{}:{}", cfg.host.trim(), cfg.port)
    };

    let bypass = if cfg.bypass.trim().is_empty() {
        "<-loopback>".to_string()
    } else {
        format!("<-loopback>,{}", cfg.bypass.trim())
    };

    Ok(Some(format!(
        "--proxy-server={server} --proxy-bypass-list={bypass} \
         --force-webrtc-ip-handling-policy=disable_non_proxied_udp --disable-quic"
    )))
}

fn read_http_body(stream: &mut TcpStream) -> Result<String, String> {
    let mut buf: Vec<u8> = Vec::with_capacity(4096);
    let mut chunk = [0u8; 4096];
    loop {
        match stream.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => {
                buf.extend_from_slice(&chunk[..n]);
                if buf.len() > 64 * 1024 {
                    break;
                }
            }
            Err(_) => break,
        }
    }
    let text = String::from_utf8_lossy(&buf).to_string();
    let (head, body) = match text.split_once("\r\n\r\n") {
        Some((h, b)) => (h.to_string(), b.to_string()),
        None => (text.clone(), String::new()),
    };
    let status_ok = head
        .lines()
        .next()
        .and_then(|l| l.split_whitespace().nth(1))
        .map(|code| code.starts_with('2'))
        .unwrap_or(false);
    if !status_ok {
        let first = head.lines().next().unwrap_or("không có phản hồi").to_string();
        return Err(format!("Proxy trả về: {first}"));
    }
    Ok(body)
}

/// Kiểm tra proxy: gọi `http://api.ipify.org` qua proxy (hoặc trực tiếp nếu tắt)
/// để lấy IP công khai thật sự đang dùng.
pub fn test_proxy(cfg: &ProxyConfig) -> ProxyTestResult {
    const TARGET: &str = "api.ipify.org";
    let started = Instant::now();
    let via = if cfg.is_usable() {
        format!("{}://{}:{}", cfg.kind_norm(), cfg.host.trim(), cfg.port)
    } else {
        "trực tiếp (không proxy)".to_string()
    };

    let outcome = (|| -> Result<String, String> {
        if !cfg.is_usable() {
            let mut stream = connect_timeout(TARGET, 80, Duration::from_secs(10))?;
            stream
                .set_read_timeout(Some(Duration::from_secs(15)))
                .map_err(|e| e.to_string())?;
            stream
                .write_all(
                    b"GET / HTTP/1.1\r\nHost: api.ipify.org\r\nUser-Agent: HenryChatPro/3\r\nConnection: close\r\n\r\n",
                )
                .map_err(|e| e.to_string())?;
            return read_http_body(&mut stream);
        }

        if cfg.needs_auth() {
            let port = ensure_relay(cfg)?;
            let mut stream = connect_timeout("127.0.0.1", port, Duration::from_secs(10))?;
            stream
                .set_read_timeout(Some(Duration::from_secs(20)))
                .map_err(|e| e.to_string())?;
            stream
                .write_all(
                    b"CONNECT api.ipify.org:80 HTTP/1.1\r\nHost: api.ipify.org:80\r\n\r\n",
                )
                .map_err(|e| e.to_string())?;
            let head = read_until_headers(&mut stream, 16 * 1024)?;
            let head_text = String::from_utf8_lossy(&head).to_string();
            if !head_text.contains(" 200") {
                return Err(format!(
                    "Proxy từ chối CONNECT: {}",
                    head_text.lines().next().unwrap_or("")
                ));
            }
            stream
                .write_all(
                    b"GET / HTTP/1.1\r\nHost: api.ipify.org\r\nUser-Agent: HenryChatPro/3\r\nConnection: close\r\n\r\n",
                )
                .map_err(|e| e.to_string())?;
            return read_http_body(&mut stream);
        }

        if cfg.kind_norm() == "socks5" {
            let mut stream = connect_timeout(&cfg.host.trim(), cfg.port, Duration::from_secs(10))?;
            stream
                .set_read_timeout(Some(Duration::from_secs(15)))
                .map_err(|e| e.to_string())?;
            socks5_connect(&mut stream, "api.ipify.org:80", "", "")?;
            stream
                .write_all(
                    b"GET / HTTP/1.1\r\nHost: api.ipify.org\r\nUser-Agent: HenryChatPro/3\r\nConnection: close\r\n\r\n",
                )
                .map_err(|e| e.to_string())?;
            return read_http_body(&mut stream);
        }

        let mut stream = connect_timeout(&cfg.host.trim(), cfg.port, Duration::from_secs(10))?;
        stream
            .set_read_timeout(Some(Duration::from_secs(15)))
            .map_err(|e| e.to_string())?;
        let mut req = String::from(
            "GET http://api.ipify.org/ HTTP/1.1\r\nHost: api.ipify.org\r\nUser-Agent: HenryChatPro/3\r\nConnection: close\r\n",
        );
        if cfg.needs_auth() {
            req.push_str(&format!("Proxy-Authorization: {}\r\n", cfg.basic_auth()));
        }
        req.push_str("\r\n");
        stream.write_all(req.as_bytes()).map_err(|e| e.to_string())?;
        read_http_body(&mut stream)
    })();

    let latency_ms = started.elapsed().as_millis() as u64;
    match outcome {
        Ok(body) => {
            let ip = body.trim().lines().next().unwrap_or("").trim().to_string();
            if ip.is_empty() || ip.len() > 64 {
                ProxyTestResult {
                    ok: false,
                    ip: String::new(),
                    latency_ms,
                    via,
                    message: "Proxy hoạt động nhưng không đọc được IP công khai".to_string(),
                }
            } else {
                ProxyTestResult {
                    ok: true,
                    ip: ip.clone(),
                    latency_ms,
                    via,
                    message: format!("Kết nối thành công qua proxy — IP công khai: {ip}"),
                }
            }
        }
        Err(err) => ProxyTestResult {
            ok: false,
            ip: String::new(),
            latency_ms,
            via,
            message: err,
        },
    }
}
