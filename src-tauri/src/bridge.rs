//! Bridge v2: kênh nhắn tin hai chiều giữa trang web (Zalo/Messenger/…) và app.
//!
//! Khác với bridge cũ (nhồi JSON vào `document.title`, giới hạn ~3.4KB và bị
//! site ghi đè), bridge v2 dùng `chrome.webview.postMessage` của WebView2:
//! không giới hạn kích thước thực tế, không phụ thuộc title, và trang web không
//! cần quyền Tauri. Đây là nền tảng cho kho mật khẩu (P3) và tiện ích (P4).
//!
//! Danh sách lệnh hỗ trợ (JSON body):
//!   {"id":"..","op":"ping"}
//!   {"id":"..","op":"storage:get","key":".."}
//!   {"id":"..","op":"storage:set","key":"..","value":".."}
//!   {"id":"..","op":"storage:remove","key":".."}
//!   {"id":"..","op":"addons:list"}

use serde_json::{json, Value};
use std::fs;
use std::path::PathBuf;
use tauri::{AppHandle, Emitter, Manager};

fn store_dir(app: &AppHandle, platform: &str, profile_id: &str) -> Option<PathBuf> {
    let base = app.path().app_local_data_dir().ok()?;
    let safe = |s: &str| -> String {
        s.chars()
            .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
            .collect()
    };
    Some(base.join("bridge_store").join(safe(platform)).join(safe(profile_id)))
}

fn load_store(app: &AppHandle, platform: &str, profile_id: &str) -> Value {
    let Some(dir) = store_dir(app, platform, profile_id) else {
        return json!({});
    };
    let path = dir.join("store.json");
    fs::read_to_string(&path)
        .ok()
        .and_then(|raw| serde_json::from_str::<Value>(&raw).ok())
        .filter(|v| v.is_object())
        .unwrap_or_else(|| json!({}))
}

fn save_store(app: &AppHandle, platform: &str, profile_id: &str, value: &Value) {
    let Some(dir) = store_dir(app, platform, profile_id) else {
        return;
    };
    if fs::create_dir_all(&dir).is_err() {
        return;
    }
    if let Ok(text) = serde_json::to_string(value) {
        let _ = fs::write(dir.join("store.json"), text);
    }
}

/// Xử lý một message từ trang web, trả về JSON phản hồi (hoặc None nếu không cần trả lời).
pub fn handle_message(app: &AppHandle, platform: &str, profile_id: &str, raw: &str) -> Option<String> {
    let parsed: Value = serde_json::from_str(raw).ok()?;
    let op = parsed.get("op").and_then(|v| v.as_str()).unwrap_or("");
    let id = parsed.get("id").and_then(|v| v.as_str()).unwrap_or("");
    if op.is_empty() {
        return None;
    }

    let reply = match op {
        "ping" => json!({
            "ok": true,
            "op": op,
            "data": {
                "platform": platform,
                "profile_id": profile_id,
                "bridge": 2,
                "app": "henry-chat-pro",
                "ts": crate::current_epoch_ms(),
            }
        }),
        "storage:get" => {
            let key = parsed.get("key").and_then(|v| v.as_str()).unwrap_or("");
            let store = load_store(app, platform, profile_id);
            let value = store.get(key).cloned().unwrap_or(Value::Null);
            json!({ "ok": true, "op": op, "data": value })
        }
        "storage:set" => {
            let key = parsed.get("key").and_then(|v| v.as_str()).unwrap_or("").to_string();
            let value = parsed.get("value").cloned().unwrap_or(Value::Null);
            if key.is_empty() || key.len() > 200 {
                json!({ "ok": false, "op": op, "error": "key không hợp lệ" })
            } else {
                let mut store = load_store(app, platform, profile_id);
                if let Some(map) = store.as_object_mut() {
                    map.insert(key, value);
                }
                save_store(app, platform, profile_id, &store);
                json!({ "ok": true, "op": op, "data": true })
            }
        }
        "storage:remove" => {
            let key = parsed.get("key").and_then(|v| v.as_str()).unwrap_or("").to_string();
            let mut store = load_store(app, platform, profile_id);
            if let Some(map) = store.as_object_mut() {
                map.remove(&key);
            }
            save_store(app, platform, profile_id, &store);
            json!({ "ok": true, "op": op, "data": true })
        }
        "addons:list" => json!({ "ok": true, "op": op, "data": [] }),
        other => json!({ "ok": false, "op": other, "error": "op không hỗ trợ" }),
    };

    // Cho phần React biết bridge đang hoạt động (hiển thị trạng thái trong Cài đặt).
    let _ = app.emit_to(
        "main",
        "bch-bridge-message",
        json!({
            "profile_id": profile_id,
            "platform": platform,
            "op": op,
            "ok": reply.get("ok").and_then(|v| v.as_bool()).unwrap_or(false),
        }),
    );

    let mut reply = reply;
    if let Some(obj) = reply.as_object_mut() {
        obj.insert("id".to_string(), Value::String(id.to_string()));
    }
    Some(reply.to_string())
}

/// Gắn bridge vào một cửa sổ chat. Không làm gì nếu nền tảng không phải Windows
/// hoặc WebView2 chưa sẵn sàng (khi đó trang web vẫn chạy bình thường).
pub fn attach(win: &tauri::WebviewWindow, app: AppHandle, platform: String, profile_id: String) {
    #[cfg(target_os = "windows")]
    {
        use webview2_com::WebMessageReceivedEventHandler;
        use windows::core::PWSTR;

        let platform_c = platform.clone();
        let profile_c = profile_id.clone();
        let app_c = app.clone();

        let attached = win.with_webview(move |platform_webview| unsafe {
            let core = match platform_webview.controller().CoreWebView2() {
                Ok(core) => core,
                Err(err) => {
                    eprintln!("[bridge] CoreWebView2 chưa sẵn sàng: {err}");
                    return;
                }
            };
            let core_for_reply = core.clone();

            let handler = WebMessageReceivedEventHandler::create(Box::new(move |_sender, args| {
                let Some(args) = args else { return Ok(()) };
                let mut raw_ptr = PWSTR::null();
                if args.TryGetWebMessageAsString(&mut raw_ptr).is_err() {
                    return Ok(());
                }
                if raw_ptr.is_null() {
                    return Ok(());
                }
                let raw = raw_ptr.to_string().unwrap_or_default();
                if raw.is_empty() {
                    return Ok(());
                }
                if let Some(reply) = handle_message(&app_c, &platform_c, &profile_c, &raw) {
                    let wide: Vec<u16> = reply.encode_utf16().chain(std::iter::once(0)).collect();
                    let hstring = windows::core::HSTRING::from_wide(&wide[..wide.len() - 1]);
                    let _ = core_for_reply.PostWebMessageAsString(&hstring);
                }
                Ok(())
            }));

            let mut token = 0i64;
            if let Err(err) = core.add_WebMessageReceived(&handler, &mut token) {
                eprintln!("[bridge] không gắn được WebMessageReceived: {err}");
            }
        });

        if attached.is_err() {
            eprintln!("[bridge] with_webview lỗi — bridge không khả dụng cho pane này");
        }
    }

    #[cfg(not(target_os = "windows"))]
    {
        let _ = (win, app, platform, profile_id);
    }
}

/// Script chèn vào đầu mỗi trang: định nghĩa `window.__BCH_BRIDGE__`.
pub fn shim_script(platform: &str, profile_id: &str) -> String {
    let plat_js = serde_json::to_string(platform).unwrap_or_else(|_| "\"\"".into());
    let pid_js = serde_json::to_string(profile_id).unwrap_or_else(|_| "\"\"".into());
    format!(
        r#"(function () {{
    if (window.__BCH_BRIDGE__) return;
    var PLAT = {plat_js};
    var PID = {pid_js};
    var seq = 0;
    var pending = {{}};
    var webview = (window.chrome && window.chrome.webview) ? window.chrome.webview : null;

    if (webview && typeof webview.addEventListener === 'function') {{
        webview.addEventListener('message', function (ev) {{
            var data = ev && ev.data;
            if (typeof data === 'string') {{
                try {{ data = JSON.parse(data); }} catch (e) {{ return; }}
            }}
            if (!data || !data.id || !pending[data.id]) return;
            var cb = pending[data.id];
            delete pending[data.id];
            try {{ cb(data); }} catch (e) {{}}
        }});
    }}

    function call(op, payload) {{
        return new Promise(function (resolve) {{
            if (!webview || typeof webview.postMessage !== 'function') {{
                resolve({{ ok: false, error: 'bridge_unavailable' }});
                return;
            }}
            seq += 1;
            var id = PID + '_' + Date.now() + '_' + seq;
            var msg = payload || {{}};
            msg.id = id;
            msg.op = op;
            pending[id] = resolve;
            try {{
                // WebView2: gửi dạng chuỗi JSON để Rust đọc bằng TryGetWebMessageAsString
                webview.postMessage(JSON.stringify(msg));
            }} catch (e) {{
                delete pending[id];
                resolve({{ ok: false, error: String(e) }});
            }}
            setTimeout(function () {{
                if (pending[id]) {{
                    delete pending[id];
                    resolve({{ ok: false, error: 'timeout' }});
                }}
            }}, 4000);
        }});
    }}

    window.__BCH_BRIDGE__ = {{
        version: 2,
        platform: PLAT,
        profileId: PID,
        available: !!webview,
        call: call,
        ping: function () {{ return call('ping'); }},
        storageGet: function (key) {{ return call('storage:get', {{ key: key }}); }},
        storageSet: function (key, value) {{ return call('storage:set', {{ key: key, value: value }}); }},
        storageRemove: function (key) {{ return call('storage:remove', {{ key: key }}); }},
        pong: function () {{ return call('ping'); }}
    }};
}})();"#
    )
}
