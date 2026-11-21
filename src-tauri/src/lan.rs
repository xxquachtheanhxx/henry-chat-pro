//! Gửi text/file giữa các máy cài Henry Chat Pro trong cùng mạng LAN.
//!
//! - UDP broadcast cổng 47821 để tìm thiết bị (mỗi 3 giây, quên sau 12 giây).
//! - TCP listener cổng ngẫu nhiên để nhận dữ liệu.
//! - Header JSON 1 dòng, sau đó là payload (text hoặc bytes của file).
//! - Chỉ chạy khi người dùng mở panel LAN (`lan_start`).
//! - Bên nhận chỉ chấp nhận khi header mang đúng mã ghép đôi; trả về 1 dòng `OK`/`ERR ...`.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream, UdpSocket};
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tauri::{AppHandle, Emitter, State};

pub const DISCOVERY_PORT: u16 = 47821;
const PROTO_VERSION: u32 = 2;
const MAX_HEADER_BYTES: u64 = 16 * 1024;
const MAX_TEXT_CHARS: usize = 8 * 1024;
pub const MAX_FILE_BYTES: u64 = 100 * 1024 * 1024;
const MAX_CONCURRENT_CONNS: usize = 4;
const MIN_PAIR_CODE_LEN: usize = 6;

#[derive(Clone, Serialize, Deserialize)]
pub struct LanPeer {
    pub id: String,
    pub name: String,
    pub ip: String,
    pub port: u16,
}

#[derive(Deserialize)]
struct Announce {
    bch: Option<u32>,
    id: String,
    name: String,
    port: u16,
}

pub struct LanState {
    pub device_id: String,
    pub device_name: Mutex<String>,
    pub peers: Mutex<HashMap<String, (LanPeer, Instant)>>,
    pub tcp_port: Mutex<u16>,
    pub started: Mutex<bool>,
    pub pair_code: Mutex<String>,
    active_conns: AtomicUsize,
}

impl LanState {
    pub fn new() -> Self {
        let device_id = format!(
            "dev-{}-{}",
            now_ms(),
            std::process::id()
        );
        let device_name = std::env::var("COMPUTERNAME")
            .or_else(|_| std::env::var("USERNAME"))
            .or_else(|_| std::env::var("USER"))
            .unwrap_or_else(|_| "Henry Chat Pro".to_string());
        Self {
            device_id,
            device_name: Mutex::new(device_name),
            peers: Mutex::new(HashMap::new()),
            tcp_port: Mutex::new(0),
            started: Mutex::new(false),
            pair_code: Mutex::new(String::new()),
            active_conns: AtomicUsize::new(0),
        }
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn peers_list(state: &LanState) -> Vec<LanPeer> {
    state
        .peers
        .lock()
        .map(|m| {
            m.values()
                .filter(|(_, t)| t.elapsed() < Duration::from_secs(15))
                .map(|(p, _)| p.clone())
                .collect()
        })
        .unwrap_or_default()
}

fn current_pair_code(state: &LanState) -> String {
    state.pair_code.lock().map(|c| c.clone()).unwrap_or_default()
}

fn codes_match(expected: &str, given: &str) -> bool {
    let (a, b) = (expected.as_bytes(), given.as_bytes());
    if a.is_empty() || a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

struct ConnGuard(Arc<LanState>);

impl Drop for ConnGuard {
    fn drop(&mut self) {
        self.0.active_conns.fetch_sub(1, Ordering::SeqCst);
    }
}

fn find_peer(state: &LanState, peer_id: &str) -> Result<LanPeer, String> {
    state
        .peers
        .lock()
        .ok()
        .and_then(|m| m.get(peer_id).map(|(p, _)| p.clone()))
        .ok_or_else(|| "Không tìm thấy thiết bị (có thể đã offline)".to_string())
}

pub fn start(app: AppHandle, state: Arc<LanState>) {
    {
        let mut started = match state.started.lock() {
            Ok(g) => g,
            Err(_) => return,
        };
        if *started {
            return;
        }
        *started = true;
    }

    let listener = match TcpListener::bind("0.0.0.0:0") {
        Ok(l) => l,
        Err(_) => return,
    };
    if let Ok(addr) = listener.local_addr() {
        if let Ok(mut p) = state.tcp_port.lock() {
            *p = addr.port();
        }
    }

    let accept_state = Arc::clone(&state);
    let accept_app = app.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            if let Ok(stream) = stream {
                let app = accept_app.clone();
                let state = Arc::clone(&accept_state);
                if state.active_conns.fetch_add(1, Ordering::SeqCst) >= MAX_CONCURRENT_CONNS {
                    state.active_conns.fetch_sub(1, Ordering::SeqCst);
                    continue;
                }
                std::thread::spawn(move || {
                    let _guard = ConnGuard(Arc::clone(&state));
                    handle_conn(app, stream, state);
                });
            }
        }
    });

    std::thread::spawn(move || discovery_loop(app, state));
}

fn reply(mut stream: &TcpStream, msg: &str) {
    let _ = stream.write_all(msg.as_bytes());
    let _ = stream.write_all(b"\n");
    let _ = stream.flush();
}

fn handle_conn(app: AppHandle, stream: TcpStream, state: Arc<LanState>) {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(60)));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(10)));
    let clone = match stream.try_clone() {
        Ok(s) => s,
        Err(_) => return,
    };
    let mut reader = BufReader::new(clone);
    let mut header = String::new();
    match (&mut reader).take(MAX_HEADER_BYTES).read_line(&mut header) {
        Ok(_) if header.ends_with('\n') => {}
        _ => return reply(&stream, "ERR header"),
    }
    let v: serde_json::Value = match serde_json::from_str(header.trim()) {
        Ok(v) => v,
        Err(_) => return reply(&stream, "ERR header"),
    };
    let given_code = v.get("code").and_then(|k| k.as_str()).unwrap_or("");
    if !codes_match(&current_pair_code(&state), given_code) {
        return reply(&stream, "ERR code");
    }
    let kind = v.get("kind").and_then(|k| k.as_str()).unwrap_or("");
    let from: String = v
        .get("from")
        .and_then(|k| k.as_str())
        .unwrap_or("Thiết bị")
        .chars()
        .take(48)
        .collect();
    let at = now_ms();

    if kind == "text" {
        let text = v.get("text").and_then(|k| k.as_str()).unwrap_or("");
        if text.chars().count() > MAX_TEXT_CHARS {
            return reply(&stream, "ERR size");
        }
        reply(&stream, "OK");
        let _ = app.emit(
            "bch-lan-message",
            serde_json::json!({ "from": from, "text": text, "at": at }),
        );
    } else if kind == "file" {
        let name = sanitize_name(v.get("name").and_then(|k| k.as_str()).unwrap_or("file"));
        let size = v.get("size").and_then(|k| k.as_u64()).unwrap_or(0);
        if size == 0 || size > MAX_FILE_BYTES {
            return reply(&stream, "ERR size");
        }
        let dir = download_dir();
        let _ = std::fs::create_dir_all(&dir);
        let path = unique_path(&dir, &name);
        let part = path.with_file_name(format!("{name}.{at}.part"));
        let mut out = match std::fs::File::create(&part) {
            Ok(f) => f,
            Err(_) => return reply(&stream, "ERR io"),
        };
        let mut limited = reader.take(size);
        let copied = std::io::copy(&mut limited, &mut out);
        let complete = matches!(copied, Ok(n) if n == size) && out.flush().is_ok();
        drop(out);
        if !complete || std::fs::rename(&part, &path).is_err() {
            let _ = std::fs::remove_file(&part);
            return reply(&stream, "ERR io");
        }
        reply(&stream, "OK");
        let _ = app.emit(
            "bch-lan-file",
            serde_json::json!({
                "from": from,
                "name": path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or(name),
                "path": path.to_string_lossy(),
                "size": size,
                "at": at
            }),
        );
    } else {
        reply(&stream, "ERR kind");
    }
}

fn discovery_loop(app: AppHandle, state: Arc<LanState>) {
    let socket = match UdpSocket::bind(("0.0.0.0", DISCOVERY_PORT)) {
        Ok(s) => s,
        Err(_) => return,
    };
    let _ = socket.set_broadcast(true);
    let _ = socket.set_read_timeout(Some(Duration::from_millis(800)));

    let send_socket = match socket.try_clone() {
        Ok(s) => s,
        Err(_) => return,
    };
    let send_state = Arc::clone(&state);
    std::thread::spawn(move || loop {
        let id = send_state.device_id.clone();
        let name = send_state
            .device_name
            .lock()
            .map(|n| n.clone())
            .unwrap_or_default();
        let port = *send_state.tcp_port.lock().unwrap_or_else(|e| e.into_inner());
        if port == 0 {
            std::thread::sleep(Duration::from_secs(1));
            continue;
        }
        let msg = serde_json::json!({
            "bch": PROTO_VERSION,
            "id": id,
            "name": name,
            "port": port
        })
        .to_string();
        let _ = send_socket.send_to(msg.as_bytes(), ("255.255.255.255", DISCOVERY_PORT));
        std::thread::sleep(Duration::from_secs(3));
    });

    let mut buf = [0u8; 4096];
    loop {
        match socket.recv_from(&mut buf) {
            Ok((n, src)) => {
                let ann: Announce = match serde_json::from_slice(&buf[..n]) {
                    Ok(a) => a,
                    Err(_) => continue,
                };
                if ann.bch.unwrap_or(0) != PROTO_VERSION || ann.id == state.device_id {
                    continue;
                }
                let peer = LanPeer {
                    id: ann.id.clone(),
                    name: ann.name,
                    ip: src.ip().to_string(),
                    port: ann.port,
                };
                if let Ok(mut map) = state.peers.lock() {
                    map.insert(ann.id, (peer, Instant::now()));
                }
                let list = peers_list(&state);
                let _ = app.emit("bch-lan-peers", list);
            }
            Err(_) => {
                if let Ok(mut map) = state.peers.lock() {
                    map.retain(|_, (_, t)| t.elapsed() < Duration::from_secs(15));
                }
                let list = peers_list(&state);
                let _ = app.emit("bch-lan-peers", list);
            }
        }
    }
}

fn sanitize_name(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| match c {
            '\\' | '/' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
            _ => c,
        })
        .collect();
    let trimmed = cleaned.trim().trim_matches('.').to_string();
    if trimmed.is_empty() {
        "file".to_string()
    } else {
        trimmed
    }
}

fn unique_path(dir: &PathBuf, name: &str) -> PathBuf {
    let candidate = dir.join(name);
    if !candidate.exists() {
        return candidate;
    }
    let (stem, ext) = match name.rsplit_once('.') {
        Some((s, e)) => (s.to_string(), format!(".{e}")),
        None => (name.to_string(), String::new()),
    };
    for i in 1..10000 {
        let p = dir.join(format!("{stem} ({i}){ext}"));
        if !p.exists() {
            return p;
        }
    }
    candidate
}

fn download_dir() -> PathBuf {
    if let Ok(profile) = std::env::var("USERPROFILE") {
        return PathBuf::from(profile).join("Downloads");
    }
    if let Ok(home) = std::env::var("HOME") {
        return PathBuf::from(home).join("Downloads");
    }
    PathBuf::from(".")
}

fn b64_decode(input: &str) -> Result<Vec<u8>, String> {
    let table = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut lookup = [255u8; 256];
    for (i, &c) in table.iter().enumerate() {
        lookup[c as usize] = i as u8;
    }
    let mut out = Vec::with_capacity(input.len() * 3 / 4 + 3);
    let mut buf: u32 = 0;
    let mut bits = 0u8;
    for c in input.bytes() {
        if c == b'=' || c == b'\n' || c == b'\r' || c == b' ' {
            continue;
        }
        let v = lookup[c as usize];
        if v == 255 {
            return Err("Dữ liệu base64 không hợp lệ".to_string());
        }
        buf = (buf << 6) | v as u32;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((buf >> bits) as u8);
        }
    }
    Ok(out)
}

#[tauri::command]
pub async fn lan_list_peers(state: State<'_, Arc<LanState>>) -> Result<Vec<LanPeer>, String> {
    Ok(peers_list(&state))
}

#[tauri::command]
pub async fn lan_get_device_name(state: State<'_, Arc<LanState>>) -> Result<String, String> {
    Ok(state
        .device_name
        .lock()
        .map(|n| n.clone())
        .unwrap_or_else(|_| "Henry Chat Pro".to_string()))
}

#[tauri::command]
pub async fn lan_set_device_name(
    state: State<'_, Arc<LanState>>,
    name: String,
) -> Result<(), String> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Err("Tên thiết bị không được để trống".to_string());
    }
    if let Ok(mut n) = state.device_name.lock() {
        *n = trimmed.chars().take(48).collect();
    }
    Ok(())
}

#[tauri::command]
pub async fn lan_send_text(
    state: State<'_, Arc<LanState>>,
    peer_id: String,
    text: String,
) -> Result<bool, String> {
    if text.trim().is_empty() {
        return Err("Nội dung trống".to_string());
    }
    let peer = find_peer(&state, &peer_id)?;
    let from = state
        .device_name
        .lock()
        .map(|n| n.clone())
        .unwrap_or_default();
    if text.chars().count() > MAX_TEXT_CHARS {
        return Err(format!("Tin nhắn dài quá {MAX_TEXT_CHARS} ký tự"));
    }
    let code = require_pair_code(&state)?;
    let mut stream = connect_peer(&peer)?;
    let line = serde_json::json!({ "kind": "text", "from": from, "text": text, "code": code })
        .to_string();
    stream
        .write_all(line.as_bytes())
        .and_then(|_| stream.write_all(b"\n"))
        .map_err(|e| format!("Gửi thất bại: {e}"))?;
    read_reply(&stream, &peer)?;
    Ok(true)
}

#[tauri::command]
pub async fn lan_send_file(
    state: State<'_, Arc<LanState>>,
    peer_id: String,
    name: String,
    data_base64: String,
) -> Result<bool, String> {
    let bytes = b64_decode(&data_base64)?;
    if bytes.is_empty() {
        return Err("File rỗng".to_string());
    }
    if bytes.len() as u64 > MAX_FILE_BYTES {
        return Err("File lớn hơn 100 MB".to_string());
    }
    let code = require_pair_code(&state)?;
    let peer = find_peer(&state, &peer_id)?;
    let from = state
        .device_name
        .lock()
        .map(|n| n.clone())
        .unwrap_or_default();
    let mut stream = connect_peer(&peer)?;
    let header = serde_json::json!({
        "kind": "file",
        "from": from,
        "name": sanitize_name(&name),
        "size": bytes.len(),
        "code": code
    })
    .to_string();
    stream
        .write_all(header.as_bytes())
        .and_then(|_| stream.write_all(b"\n"))
        .and_then(|_| stream.write_all(&bytes))
        .map_err(|e| format!("Gửi file thất bại: {e}"))?;
    read_reply(&stream, &peer)?;
    Ok(true)
}

fn require_pair_code(state: &LanState) -> Result<String, String> {
    let code = current_pair_code(state);
    if code.is_empty() {
        return Err("Chưa đặt mã ghép đôi LAN".to_string());
    }
    Ok(code)
}

fn connect_peer(peer: &LanPeer) -> Result<TcpStream, String> {
    let addr: std::net::SocketAddr = format!("{}:{}", peer.ip, peer.port)
        .parse()
        .map_err(|_| format!("Địa chỉ không hợp lệ: {}", peer.ip))?;
    let stream = TcpStream::connect_timeout(&addr, Duration::from_secs(5))
        .map_err(|e| format!("Không kết nối được tới {}: {e}", peer.name))?;
    let _ = stream.set_write_timeout(Some(Duration::from_secs(60)));
    Ok(stream)
}

fn read_reply(stream: &TcpStream, peer: &LanPeer) -> Result<(), String> {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(60)));
    let mut line = String::new();
    BufReader::new(stream)
        .take(256)
        .read_line(&mut line)
        .map_err(|e| format!("{} không phản hồi: {e}", peer.name))?;
    match line.trim() {
        "OK" => Ok(()),
        "ERR code" => Err(format!("Mã ghép đôi không khớp với {}", peer.name)),
        "ERR size" => Err(format!("{} từ chối: dữ liệu quá lớn", peer.name)),
        "" => Err(format!("{} không phản hồi (có thể đang dùng bản cũ)", peer.name)),
        other => Err(format!("{} từ chối: {other}", peer.name)),
    }
}

#[tauri::command]
pub async fn lan_start(app: AppHandle, state: State<'_, Arc<LanState>>) -> Result<bool, String> {
    start(app, Arc::clone(&state));
    Ok(true)
}

#[tauri::command]
pub async fn lan_set_pair_code(
    state: State<'_, Arc<LanState>>,
    code: String,
) -> Result<(), String> {
    let trimmed = code.trim();
    if !trimmed.is_empty() && trimmed.chars().count() < MIN_PAIR_CODE_LEN {
        return Err(format!("Mã ghép đôi cần ít nhất {MIN_PAIR_CODE_LEN} ký tự"));
    }
    if let Ok(mut c) = state.pair_code.lock() {
        *c = trimmed.chars().take(64).collect();
    }
    Ok(())
}
