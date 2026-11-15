//! Gửi text/file giữa các máy cài Henry Chat Pro trong cùng mạng LAN.
//!
//! - UDP broadcast cổng 47821 để tìm thiết bị (mỗi 3 giây, quên sau 12 giây).
//! - TCP listener cổng ngẫu nhiên để nhận dữ liệu.
//! - Header JSON 1 dòng, sau đó là payload (text hoặc bytes của file).

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream, UdpSocket};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tauri::{AppHandle, Emitter, State};

pub const DISCOVERY_PORT: u16 = 47821;
const PROTO_VERSION: u32 = 1;

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
                std::thread::spawn(move || handle_conn(app, stream, state));
            }
        }
    });

    std::thread::spawn(move || discovery_loop(app, state));
}

fn handle_conn(app: AppHandle, stream: TcpStream, _state: Arc<LanState>) {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(60)));
    let clone = match stream.try_clone() {
        Ok(s) => s,
        Err(_) => return,
    };
    let mut reader = BufReader::new(clone);
    let mut header = String::new();
    if reader.read_line(&mut header).is_err() {
        return;
    }
    let v: serde_json::Value = match serde_json::from_str(header.trim()) {
        Ok(v) => v,
        Err(_) => return,
    };
    let kind = v.get("kind").and_then(|k| k.as_str()).unwrap_or("");
    let from = v
        .get("from")
        .and_then(|k| k.as_str())
        .unwrap_or("Thiết bị")
        .to_string();
    let at = now_ms();

    if kind == "text" {
        let text = v
            .get("text")
            .and_then(|k| k.as_str())
            .unwrap_or("")
            .to_string();
        let _ = app.emit(
            "bch-lan-message",
            serde_json::json!({ "from": from, "text": text, "at": at }),
        );
    } else if kind == "file" {
        let name = sanitize_name(v.get("name").and_then(|k| k.as_str()).unwrap_or("file"));
        let size = v.get("size").and_then(|k| k.as_u64()).unwrap_or(0);
        let dir = download_dir();
        let _ = std::fs::create_dir_all(&dir);
        let path = unique_path(&dir, &name);
        let mut out = match std::fs::File::create(&path) {
            Ok(f) => f,
            Err(_) => return,
        };
        let mut limited = reader.take(size);
        if std::io::copy(&mut limited, &mut out).is_err() {
            return;
        }
        let _ = out.flush();
        let _ = app.emit(
            "bch-lan-file",
            serde_json::json!({
                "from": from,
                "name": name,
                "path": path.to_string_lossy(),
                "size": size,
                "at": at
            }),
        );
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
    let mut stream = TcpStream::connect((peer.ip.as_str(), peer.port))
        .map_err(|e| format!("Không kết nối được tới {}: {e}", peer.name))?;
    let line = serde_json::json!({ "kind": "text", "from": from, "text": text }).to_string();
    stream
        .write_all(line.as_bytes())
        .and_then(|_| stream.write_all(b"\n"))
        .map_err(|e| format!("Gửi thất bại: {e}"))?;
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
    let peer = find_peer(&state, &peer_id)?;
    let from = state
        .device_name
        .lock()
        .map(|n| n.clone())
        .unwrap_or_default();
    let mut stream = TcpStream::connect((peer.ip.as_str(), peer.port))
        .map_err(|e| format!("Không kết nối được tới {}: {e}", peer.name))?;
    let header = serde_json::json!({
        "kind": "file",
        "from": from,
        "name": sanitize_name(&name),
        "size": bytes.len()
    })
    .to_string();
    stream
        .write_all(header.as_bytes())
        .and_then(|_| stream.write_all(b"\n"))
        .and_then(|_| stream.write_all(&bytes))
        .map_err(|e| format!("Gửi file thất bại: {e}"))?;
    Ok(true)
}
