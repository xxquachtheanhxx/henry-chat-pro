//! Henry Chat Pro — Backend workspace đa tài khoản web.
//!
//! Phạm vi duy nhất của lớp Rust:
//! 1. Mỗi tài khoản (profile) chạy trong một WebView2 riêng với thư mục dữ liệu riêng
//!    -> giữ phiên đăng nhập độc lập cho từng tài khoản của cùng một website.
//! 2. Nhúng các cửa sổ đó khít vào phần bên phải cửa sổ chính (Win32 SetWindowPos).
//! 3. Tiêm script giám sát thông báo / cuộc gọi (chỉ đọc DOM, không lưu nội dung tin nhắn).
//!
//! Toàn bộ phần kho dữ liệu Zalo (quét LevelDB, backup, Google Drive, bơm lịch sử vào web chat)
//! đã được loại bỏ: đó là hướng đi không khả thi và không còn thuộc phạm vi sản phẩm.

mod bridge;
mod lan;
mod net;

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;
use tauri::{
    AppHandle, Emitter, LogicalSize, Manager, Size, State, WebviewUrl, WebviewWindowBuilder,
    WindowEvent,
};
// Chỉ dùng cho nhánh dự phòng ngoài Windows (sản phẩm chính chạy Win32).
#[cfg(not(target_os = "windows"))]
use tauri::{PhysicalPosition, PhysicalSize, Position};

const DEFAULT_SIDEBAR_LOGICAL_WIDTH: f64 = 310.0;

/// Sự kiện thông báo / cuộc gọi do script giám sát trong WebView của từng tài khoản gửi về.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProfileAlertEvent {
    #[serde(default)]
    pub profile_id: String,
    #[serde(default)]
    pub platform: String,
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub unread_count: u32,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub body: String,
    #[serde(default)]
    pub caller: String,
    #[serde(default)]
    pub call_kind: String,
    /// Khoá hội thoại/nhóm (Telegram peer id hoặc tên chuẩn hoá).
    #[serde(default)]
    pub conversation: String,
    #[serde(default)]
    pub timestamp: u64,
}

/// Cấu hình web riêng của từng profile (hiện có proxy; vân tay sẽ thêm ở P2).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PrivacyConfig {
    #[serde(default)]
    pub block_read_receipts: bool,
    #[serde(default)]
    pub block_typing: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ProfileWebConfig {
    #[serde(default)]
    pub proxy: net::ProxyConfig,
    #[serde(default)]
    pub privacy: PrivacyConfig,
}

pub struct WorkspaceState {
    pub app_handle: Mutex<Option<AppHandle>>,
    pub active_chat_label: Mutex<Option<String>>,
    pub modal_overlay_open: Mutex<bool>,
    pub allow_exit: Mutex<bool>,
    pub window_op_lock: Mutex<()>,
    pub created_hwnds: Mutex<HashMap<String, isize>>,
    pub sidebar_logical_width: Mutex<f64>,
    /// Phần chiều ngang chừa ở bên phải khung chat (khi rail/khung tài khoản nằm bên phải).
    pub right_inset_logical: Mutex<f64>,
    pub last_bounds: Mutex<Option<(String, i32, i32, u32, u32)>>,
    pub profile_notify_enabled: Mutex<HashMap<String, bool>>,
    /// label cửa sổ chat -> thời điểm dùng gần nhất (epoch ms)
    pub last_used: Mutex<HashMap<String, u64>>,
    /// Số WebView được giữ chạy đồng thời (0 = không giới hạn)
    pub max_live_webviews: Mutex<usize>,
    pub minimize_to_tray: Mutex<bool>,
    pub tray_profiles: Mutex<Vec<(String, String, String)>>, // (id, name, platform_name)
    pub privacy_mode: Mutex<(bool, bool)>, // (block_read_receipts, block_typing)
    /// Cho mọi tài khoản bị ẩn "ngủ" (TrySuspend). Tắt thì chỉ tài khoản tắt thông báo mới ngủ.
    pub sleep_hidden_profiles: Mutex<bool>,
    /// label cửa sổ chat -> profile_id
    pub label_profile: Mutex<HashMap<String, String>>,
}

impl WorkspaceState {
    pub fn new() -> Self {
        Self {
            app_handle: Mutex::new(None),
            active_chat_label: Mutex::new(None),
            modal_overlay_open: Mutex::new(false),
            allow_exit: Mutex::new(false),
            window_op_lock: Mutex::new(()),
            created_hwnds: Mutex::new(HashMap::new()),
            sidebar_logical_width: Mutex::new(DEFAULT_SIDEBAR_LOGICAL_WIDTH),
            right_inset_logical: Mutex::new(0.0),
            last_bounds: Mutex::new(None),
            profile_notify_enabled: Mutex::new(HashMap::new()),
            last_used: Mutex::new(HashMap::new()),
            max_live_webviews: Mutex::new(3),
            minimize_to_tray: Mutex::new(true),
            tray_profiles: Mutex::new(Vec::new()),
            privacy_mode: Mutex::new((false, false)),
            sleep_hidden_profiles: Mutex::new(false),
            label_profile: Mutex::new(HashMap::new()),
        }
    }

    pub fn is_profile_notify_enabled(&self, profile_id: &str) -> bool {
        self.profile_notify_enabled
            .lock()
            .ok()
            .and_then(|m| m.get(profile_id).copied())
            .unwrap_or(true)
    }

    /// Tài khoản bị ẩn có được "ngủ" không: luôn ngủ nếu tắt thông báo, còn lại theo cài đặt.
    pub fn should_sleep_hidden(&self, label: &str) -> bool {
        if self.sleep_hidden_profiles.lock().map(|g| *g).unwrap_or(false) {
            return true;
        }
        self.label_profile
            .lock()
            .ok()
            .and_then(|m| m.get(label).cloned())
            .map(|profile_id| !self.is_profile_notify_enabled(&profile_id))
            .unwrap_or(false)
    }

    pub fn emit_profile_event(&self, event: ProfileAlertEvent) {
        if event.profile_id.is_empty() {
            return;
        }
        if let Some(app) = self.app_handle.lock().ok().and_then(|g| g.clone()) {
            let _ = app.emit_to("main", "bch-profile-event", &event);
            self.alert_outside_app(&app, &event);
        }
    }

    /// Cửa sổ chính đang nằm dưới khay hoặc không được chọn thì giao diện trong app không ai thấy:
    /// báo ở cấp hệ điều hành (balloon ở khay / nhấp nháy nút trên taskbar).
    fn alert_outside_app(&self, app: &AppHandle, event: &ProfileAlertEvent) {
        let is_call = event.kind == "call_start";
        if !(is_call || event.kind == "message")
            || !self.is_profile_notify_enabled(&event.profile_id)
        {
            return;
        }
        let Some(main) = app.get_webview_window("main") else {
            return;
        };
        if main.is_visible().unwrap_or(true) && !main.is_minimized().unwrap_or(false) {
            if !main.is_focused().unwrap_or(true) {
                let _ = main.request_user_attention(Some(if is_call {
                    tauri::UserAttentionType::Critical
                } else {
                    tauri::UserAttentionType::Informational
                }));
            }
            return;
        }
        if main.is_minimized().unwrap_or(false) {
            let _ = main.request_user_attention(Some(if is_call {
                tauri::UserAttentionType::Critical
            } else {
                tauri::UserAttentionType::Informational
            }));
            return;
        }
        // Tin nhắn dồn dập: tối đa 1 balloon mỗi 5 giây. Cuộc gọi thì luôn báo.
        static LAST_BALLOON_MS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let now = current_epoch_ms();
        let last = LAST_BALLOON_MS.load(std::sync::atomic::Ordering::Relaxed);
        if !is_call && now.saturating_sub(last) < 5000 {
            return;
        }
        LAST_BALLOON_MS.store(now, std::sync::atomic::Ordering::Relaxed);
        #[cfg(target_os = "windows")]
        {
            let (title, body) = if is_call {
                (
                    "📞 Cuộc gọi đến".to_string(),
                    if event.caller.is_empty() { "Mở Henry Chat Pro để nghe máy".to_string() } else { event.caller.clone() },
                )
            } else {
                (
                    if event.title.is_empty() { "Tin nhắn mới".to_string() } else { event.title.clone() },
                    event.body.clone(),
                )
            };
            show_tray_balloon(app, &title, &body);
        }
    }

    /// Kênh IPC qua `document.title`: trang web từ xa không cần quyền Tauri, không bị CSP chặn.
    /// Chỉ nhận sự kiện thông báo / yêu cầu chuyển tài khoản — không nhận nội dung tin nhắn.
    pub fn handle_webview_title_ipc(&self, title: &str) {
        if let Some(rest) = title.strip_prefix("__BCH_EVENT__:") {
            if let Some((_seq, json_str)) = rest.split_once(':') {
                if let Ok(evt) = serde_json::from_str::<ProfileAlertEvent>(json_str) {
                    self.emit_profile_event(evt);
                }
            }
        } else if let Some(rest) = title.strip_prefix("__BCH_SWITCH_PROFILE__:") {
            let pid = rest.trim().to_string();
            if !pid.is_empty() {
                self.emit_profile_event(ProfileAlertEvent {
                    profile_id: pid,
                    platform: String::new(),
                    kind: "switch_profile".to_string(),
                    unread_count: 0,
                    title: String::new(),
                    body: String::new(),
                    caller: String::new(),
                    call_kind: String::new(),
                    conversation: String::new(),
                    timestamp: 0,
                });
            }
        }
    }
}

#[cfg(target_os = "windows")]
fn win32_hide_hwnd(raw_hwnd: isize) {
    #[link(name = "user32")]
    extern "system" {
        fn ShowWindow(hWnd: *mut std::ffi::c_void, nCmdShow: i32) -> i32;
        fn SetWindowPos(
            hWnd: *mut std::ffi::c_void,
            hWndInsertAfter: *mut std::ffi::c_void,
            X: i32,
            Y: i32,
            cx: i32,
            cy: i32,
            uFlags: u32,
        ) -> i32;
    }
    if raw_hwnd == 0 {
        return;
    }
    unsafe {
        let hwnd_ptr = raw_hwnd as *mut std::ffi::c_void;
        // SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE | SWP_HIDEWINDOW
        let _ = SetWindowPos(
            hwnd_ptr,
            std::ptr::null_mut(),
            -32000,
            -32000,
            0,
            0,
            0x0001 | 0x0004 | 0x0010 | 0x0080,
        );
        // SW_HIDE = 0
        let _ = ShowWindow(hwnd_ptr, 0);
    }
}

#[cfg(not(target_os = "windows"))]
fn win32_hide_hwnd(_raw_hwnd: isize) {}

#[cfg(target_os = "windows")]
fn win32_show_hwnd(raw_hwnd: isize, x: i32, y: i32, w: u32, h: u32) {
    #[link(name = "user32")]
    extern "system" {
        fn ShowWindow(hWnd: *mut std::ffi::c_void, nCmdShow: i32) -> i32;
        fn SetWindowPos(
            hWnd: *mut std::ffi::c_void,
            hWndInsertAfter: *mut std::ffi::c_void,
            X: i32,
            Y: i32,
            cx: i32,
            cy: i32,
            uFlags: u32,
        ) -> i32;
    }
    if raw_hwnd == 0 {
        return;
    }
    unsafe {
        let hwnd_ptr = raw_hwnd as *mut std::ffi::c_void;
        // SWP_NOZORDER | SWP_NOACTIVATE | SWP_SHOWWINDOW
        // Tránh SWP_FRAMECHANGED để không ép WebView2 tính lại layout khi kéo cửa sổ
        let _ = SetWindowPos(
            hwnd_ptr,
            std::ptr::null_mut(),
            x,
            y,
            w as i32,
            h as i32,
            0x0004 | 0x0010 | 0x0040,
        );
        // SW_SHOWNOACTIVATE = 4
        let _ = ShowWindow(hwnd_ptr, 4);
    }
}

#[cfg(not(target_os = "windows"))]
fn win32_show_hwnd(_raw_hwnd: isize, _x: i32, _y: i32, _w: u32, _h: u32) {}

/// Đưa focus bàn phím vào cửa sổ chat con (dùng khi bắt máy cuộc gọi đến).
#[cfg(target_os = "windows")]
fn win32_focus_hwnd(raw_hwnd: isize) {
    #[link(name = "user32")]
    extern "system" {
        fn SetFocus(hWnd: *mut std::ffi::c_void) -> *mut std::ffi::c_void;
    }
    if raw_hwnd == 0 {
        return;
    }
    unsafe {
        let _ = SetFocus(raw_hwnd as *mut std::ffi::c_void);
    }
}

/// Cho phép kéo thả file từ Windows Explorer vào WebView2 (kể cả khi chạy quyền Admin / khác mức UIPI)
#[cfg(target_os = "windows")]
fn win32_allow_drag_drop_messages(raw_hwnd: isize) {
    #[link(name = "user32")]
    extern "system" {
        fn ChangeWindowMessageFilterEx(
            hWnd: *mut std::ffi::c_void,
            message: u32,
            action: u32,
            pChangeFilterStruct: *mut std::ffi::c_void,
        ) -> i32;
    }
    #[link(name = "shell32")]
    extern "system" {
        fn DragAcceptFiles(hWnd: *mut std::ffi::c_void, fAccept: i32);
    }
    if raw_hwnd == 0 {
        return;
    }
    unsafe {
        let hwnd_ptr = raw_hwnd as *mut std::ffi::c_void;
        // MSGFLT_ALLOW = 1
        // WM_DROPFILES = 0x0233, WM_COPYDATA = 0x004A, WM_COPYGLOBALDATA = 0x0049
        let _ = ChangeWindowMessageFilterEx(hwnd_ptr, 0x0233, 1, std::ptr::null_mut());
        let _ = ChangeWindowMessageFilterEx(hwnd_ptr, 0x004A, 1, std::ptr::null_mut());
        let _ = ChangeWindowMessageFilterEx(hwnd_ptr, 0x0049, 1, std::ptr::null_mut());
        DragAcceptFiles(hwnd_ptr, 1);
    }
}

#[cfg(not(target_os = "windows"))]
fn win32_allow_drag_drop_messages(_raw_hwnd: isize) {}

/// Biến cửa sổ chat thành cửa sổ CON thật sự (WS_CHILD) của cửa sổ chính.
/// Tauri `parent()` chỉ đặt owner (WS_POPUP) nên cửa sổ chat có thể nổi lên trên ứng dụng khác
/// và không bị cắt theo cửa sổ chính. WS_CHILD khắc phục triệt để cả hai vấn đề.
#[cfg(target_os = "windows")]
fn win32_attach_as_child(raw_hwnd: isize, raw_parent: isize) {
    #[link(name = "user32")]
    extern "system" {
        fn SetParent(
            hWnd: *mut std::ffi::c_void,
            hWndNewParent: *mut std::ffi::c_void,
        ) -> *mut std::ffi::c_void;
        fn GetWindowLongPtrW(hWnd: *mut std::ffi::c_void, nIndex: i32) -> isize;
        fn SetWindowLongPtrW(hWnd: *mut std::ffi::c_void, nIndex: i32, dwNewLong: isize) -> isize;
        fn SetWindowPos(
            hWnd: *mut std::ffi::c_void,
            hWndInsertAfter: *mut std::ffi::c_void,
            X: i32,
            Y: i32,
            cx: i32,
            cy: i32,
            uFlags: u32,
        ) -> i32;
    }
    if raw_hwnd == 0 || raw_parent == 0 {
        return;
    }

    const GWL_STYLE: i32 = -16;
    const WS_CHILD: isize = 0x4000_0000;
    const WS_POPUP: isize = 0x8000_0000u32 as isize;

    unsafe {
        let hwnd_ptr = raw_hwnd as *mut std::ffi::c_void;
        let parent_ptr = raw_parent as *mut std::ffi::c_void;
        let style = GetWindowLongPtrW(hwnd_ptr, GWL_STYLE);
        let new_style = (style & !WS_POPUP) | WS_CHILD;
        SetWindowLongPtrW(hwnd_ptr, GWL_STYLE, new_style);
        SetParent(hwnd_ptr, parent_ptr);
        // SWP_FRAMECHANGED | SWP_NOZORDER | SWP_NOACTIVATE | SWP_NOMOVE | SWP_NOSIZE
        let _ = SetWindowPos(
            hwnd_ptr,
            std::ptr::null_mut(),
            0,
            0,
            0,
            0,
            0x0020 | 0x0004 | 0x0010 | 0x0002 | 0x0001,
        );
    }
}

#[cfg(not(target_os = "windows"))]
fn win32_attach_as_child(_raw_hwnd: isize, _raw_parent: isize) {}

fn build_notify_call_monitor_script(platform: &str, profile_id: &str, notify_enabled: bool) -> String {
    let tpl = r##"
        (function() {
            if (window.top !== window) return;
            if (window.__BCH_NOTIFY_MONITOR_INIT__) {
                if (typeof window.__BCH_SET_NOTIFY_ENABLED__ === 'function') {
                    window.__BCH_SET_NOTIFY_ENABLED__(__NOTIFY_ENABLED__);
                }
                return;
            }
            window.__BCH_NOTIFY_MONITOR_INIT__ = true;

            const PROF_ID = "__PROFILE_ID__";
            const PLAT_ID = "__PLATFORM_ID__";
            const origStringify = JSON.stringify;

            window.__BCH_NOTIFY_ENABLED__ = __NOTIFY_ENABLED__;
            let lastUserClickAt = 0;
            let lastUnreadCount = -1;
            let lastCallActive = false;
            let lastCallKey = "";
            let lastNotifySentAt = 0;
            let seqCounter = 0;
            const lastChatUnread = {};
            let chatDigestInit = false;

            const bchTitleQueue = [];
            let bchTitleBusy = false;
            window.__BCH_REAL_TITLE__ = String(document.title || "").indexOf("__BCH_") === 0 ? "" : String(document.title || "");

            function pumpBchTitleQueue() {
                if (bchTitleBusy) return;
                if (bchTitleQueue.length === 0) {
                    const cur = String(document.title || "");
                    if (cur.indexOf("__BCH_") === 0 && window.__BCH_REAL_TITLE__) {
                        document.title = window.__BCH_REAL_TITLE__;
                    }
                    return;
                }
                bchTitleBusy = true;
                try {
                    const cur = String(document.title || "");
                    if (cur && cur.indexOf("__BCH_") !== 0) {
                        window.__BCH_REAL_TITLE__ = cur;
                    }
                    const nextItem = bchTitleQueue.shift();
                    if (nextItem) {
                        document.title = nextItem;
                    }
                } catch (e) {}
                setTimeout(function() {
                    bchTitleBusy = false;
                    if (bchTitleQueue.length > 0) {
                        pumpBchTitleQueue();
                    } else {
                        try {
                            const cur = String(document.title || "");
                            if (cur.indexOf("__BCH_") === 0) {
                                document.title = window.__BCH_REAL_TITLE__ || "";
                            }
                        } catch (e) {}
                    }
                }, 34);
            }

            window.__BCH_TITLE_IPC__ = function(rawTitleMsg, urgent) {
                try {
                    if (!rawTitleMsg || rawTitleMsg.length > 3950) return false;
                    if (!urgent && bchTitleQueue.length > 80) {
                        bchTitleQueue.splice(0, 20);
                    }
                    if (urgent) {
                        bchTitleQueue.unshift(rawTitleMsg);
                    } else {
                        bchTitleQueue.push(rawTitleMsg);
                    }
                    pumpBchTitleQueue();
                    return true;
                } catch (e) {
                    return false;
                }
            };

            window.addEventListener('mousedown', function() { lastUserClickAt = Date.now(); }, { passive: true, capture: true });
            window.addEventListener('keydown', function() { lastUserClickAt = Date.now(); }, { passive: true, capture: true });

            function sendSwitchProfile(targetId) {
                if (!targetId) return;
                window.__BCH_TITLE_IPC__("__BCH_SWITCH_PROFILE__:" + targetId, true);
            }
            window.__BCH_JUMP_TO_PROFILE__ = sendSwitchProfile;

            window.__BCH_SET_NOTIFY_ENABLED__ = function(enabled) {
                window.__BCH_NOTIFY_ENABLED__ = !!enabled;
                if (!window.__BCH_NOTIFY_ENABLED__) {
                    try {
                        const audios = document.querySelectorAll('audio');
                        for (let i = 0; i < audios.length; i++) {
                            if (!audios[i].controls) {
                                audios[i].pause();
                            }
                        }
                    } catch (e) {}
                }
            };

            function emitProfileEvent(kind, extra) {
                try {
                    const data = extra || {};
                    const payload = {
                        profile_id: PROF_ID,
                        platform: PLAT_ID,
                        kind: kind || "message",
                        unread_count: typeof data.unread_count === 'number' ? data.unread_count : (lastUnreadCount > 0 ? lastUnreadCount : 0),
                        title: String(data.title || "").slice(0, 180),
                        body: String(data.body || "").slice(0, 320),
                        caller: String(data.caller || "").slice(0, 140),
                        call_kind: String(data.call_kind || "").slice(0, 24),
                        conversation: String(data.conversation || "").slice(0, 160),
                        timestamp: Date.now()
                    };
                    const jsonStr = origStringify(payload);
                    if (jsonStr.length < 3400) {
                        seqCounter = (seqCounter + 1) % 900000;
                        window.__BCH_TITLE_IPC__("__BCH_EVENT__:" + seqCounter + ":" + jsonStr, false);
                    }
                } catch (e) {}
            }

            function looksLikeCallText(str) {
                if (!str) return false;
                if (/(missed call|cuộc gọi nhỡ|cuoc goi nho|đã kết thúc|kết thúc cuộc gọi|call ended|declined|từ chối|đang bận)/i.test(str)) {
                    return false;
                }
                return /(cuộc gọi|đang gọi|gọi đến|gọi video|gọi thoại|incoming call|incoming audio|incoming video|is calling|calling you|video call|audio call|voice call|wants to call|muốn gọi)/i.test(str);
            }

            // 1. Hook Web Notification API & navigator.permissions để nền tảng web luôn gửi sự kiện thông báo/cuộc gọi
            try {
                if (navigator.permissions && typeof navigator.permissions.query === 'function' && !navigator.permissions.query.__bch_wrapped__) {
                    const origPermQuery = navigator.permissions.query.bind(navigator.permissions);
                    const wrappedPermQuery = function(desc) {
                        if (desc && desc.name === 'notifications') {
                            return Promise.resolve({ state: 'granted', onchange: null });
                        }
                        return origPermQuery(desc);
                    };
                    wrappedPermQuery.__bch_wrapped__ = true;
                    navigator.permissions.query = wrappedPermQuery;
                }
            } catch (e) {}

            try {
                if (typeof window.Notification === 'function' && !window.Notification.__bch_wrapped__) {
                    const OrigNotification = window.Notification;
                    const WrappedNotification = function(title, options) {
                        const t = String(title || "");
                        const b = options && options.body ? String(options.body) : "";
                        const combined = t + " " + b;
                        const isCall = looksLikeCallText(combined);
                        lastNotifySentAt = Date.now();
                        emitProfileEvent(isCall ? "call_start" : "message", {
                            title: t,
                            body: b,
                            caller: isCall ? (t.replace(/đang gọi.*/i, '').trim() || b.slice(0, 80)) : "",
                            call_kind: isCall ? (/video/i.test(combined) ? "video" : "voice") : ""
                        });

                        if (!window.__BCH_NOTIFY_ENABLED__ || OrigNotification.permission !== 'granted') {
                            return {
                                title: t,
                                body: b,
                                close: function() {},
                                addEventListener: function() {},
                                removeEventListener: function() {}
                            };
                        }
                        try {
                            return new OrigNotification(title, options);
                        } catch (ne) {
                            return { close: function() {} };
                        }
                    };
                    WrappedNotification.permission = 'granted';
                    WrappedNotification.requestPermission = function(cb) {
                        if (typeof cb === 'function') {
                            try { cb('granted'); } catch (e) {}
                        }
                        return Promise.resolve('granted');
                    };
                    WrappedNotification.prototype = OrigNotification.prototype;
                    WrappedNotification.__bch_wrapped__ = true;
                    window.Notification = WrappedNotification;
                }
            } catch (e) {}

            // 2. Hook ServiceWorkerRegistration.showNotification
            try {
                if (typeof ServiceWorkerRegistration !== 'undefined' && ServiceWorkerRegistration.prototype && typeof ServiceWorkerRegistration.prototype.showNotification === 'function') {
                    const origShowNotif = ServiceWorkerRegistration.prototype.showNotification;
                    if (!origShowNotif.__bch_wrapped__) {
                        const wrappedShow = function(title, options) {
                            const t = String(title || "");
                            const b = options && options.body ? String(options.body) : "";
                            const combined = t + " " + b;
                            const isCall = looksLikeCallText(combined);
                            lastNotifySentAt = Date.now();
                            emitProfileEvent(isCall ? "call_start" : "message", {
                                title: t,
                                body: b,
                                caller: isCall ? (t.replace(/đang gọi.*/i, '').trim() || b.slice(0, 80)) : "",
                                call_kind: isCall ? (/video/i.test(combined) ? "video" : "voice") : ""
                            });
                            if (!window.__BCH_NOTIFY_ENABLED__) {
                                return Promise.resolve();
                            }
                            return origShowNotif.apply(this, arguments);
                        };
                        wrappedShow.__bch_wrapped__ = true;
                        ServiceWorkerRegistration.prototype.showNotification = wrappedShow;
                    }
                }
            } catch (e) {}

            // 3. Chan am thanh thong bao / chuong reo ngam khi nguoi dung tat thong bao cho profile nay
            try {
                if (typeof HTMLMediaElement !== 'undefined' && HTMLMediaElement.prototype && typeof HTMLMediaElement.prototype.play === 'function') {
                    const origMediaPlay = HTMLMediaElement.prototype.play;
                    if (!origMediaPlay.__bch_wrapped__) {
                        const wrappedPlay = function() {
                            if (!window.__BCH_NOTIFY_ENABLED__) {
                                const recentManual = (Date.now() - lastUserClickAt) < 1200;
                                if (!recentManual && (document.hidden || !this.controls)) {
                                    return Promise.resolve();
                                }
                            }
                            return origMediaPlay.apply(this, arguments);
                        };
                        wrappedPlay.__bch_wrapped__ = true;
                        HTMLMediaElement.prototype.play = wrappedPlay;
                    }
                }
            } catch (e) {}

            function parseTitleUnread() {
                let rawTitle = String(document.title || "");
                if (rawTitle.indexOf("__BCH_") === 0) {
                    rawTitle = String(window.__BCH_REAL_TITLE__ || "");
                }
                if (!rawTitle || rawTitle.indexOf("__BCH_") === 0) return -1;
                const m = rawTitle.match(/^\((\d+)\)|\s\((\d+)\)\s*$/);
                if (m) {
                    const n = parseInt(m[1] || m[2] || "0", 10);
                    return isFinite(n) ? Math.min(n, 999) : 0;
                }
                if (/có tin nhắn mới|new message/i.test(rawTitle)) return 1;
                return 0;
            }

            function readPlatformUnreadCount() {
                const titleNum = parseTitleUnread();
                if (titleNum < 0) return lastUnreadCount > 0 ? lastUnreadCount : 0;
                let domCount = 0;
                try {
                    if (PLAT_ID === 'zalo') {
                        const badges = document.querySelectorAll('#conversationListId .conv-unread, #conversationListId .tag-unread, .tab-main .tab-red-dot, .msg-item .conv-unread');
                        for (let i = 0; i < badges.length && i < 60; i++) {
                            const b = badges[i];
                            if (!b || b.offsetParent === null) continue;
                            const num = parseInt((b.textContent || "").replace(/\D+/g, ""), 10);
                            domCount += (isFinite(num) && num > 0) ? Math.min(num, 99) : 1;
                        }
                    } else if (PLAT_ID === 'telegram') {
                        const badges = document.querySelectorAll('.chatlist .dialog-subtitle-badge.unread:not(.is-muted), #folders-container .unread:not(.is-muted), .ChatList .Badge.unread:not(.muted)');
                        for (let i = 0; i < badges.length && i < 50; i++) {
                            const b = badges[i];
                            if (!b || b.offsetParent === null) continue;
                            const num = parseInt((b.textContent || "").replace(/\D+/g, ""), 10);
                            domCount += (isFinite(num) && num > 0) ? Math.min(num, 99) : 1;
                        }
                    } else if (PLAT_ID === 'messenger') {
                        const badges = document.querySelectorAll('[aria-label*="Mark as read" i], [aria-label*="Đánh dấu là đã đọc" i]');
                        domCount = Math.min(badges.length, 99);
                    } else if (PLAT_ID === 'whatsapp') {
                        const badges = document.querySelectorAll('span[aria-label*="unread message" i], span[aria-label*="tin nhắn chưa đọc" i]');
                        for (let i = 0; i < badges.length && i < 50; i++) {
                            const b = badges[i];
                            const num = parseInt((b.textContent || "").replace(/\D+/g, ""), 10);
                            domCount += (isFinite(num) && num > 0) ? Math.min(num, 99) : 1;
                        }
                    }
                } catch (e) {}
                const best = Math.max(titleNum, Math.min(domCount, 999));
                return best;
            }

            // Đọc danh sách hội thoại Telegram đang hiển thị: tên nhóm, tin nhắn mới nhất, số chưa đọc, trạng thái mute.
            function readTelegramChatDigest() {
                const out = [];
                try {
                    const items = document.querySelectorAll(
                        '.chatlist .chatlist-chat, .chatlist .dialog, #folders-container .chatlist-chat, .ChatList .chatlist-chat, .ChatList .dialog'
                    );
                    for (let i = 0; i < items.length && i < 60; i++) {
                        const el = items[i];
                        const titleEl = el.querySelector('.peer-title, .dialog-title, .user-title, .chat-title');
                        let name = titleEl ? String(titleEl.textContent || '').trim() : '';
                        if (!name) {
                            const al = el.getAttribute('aria-label') || '';
                            name = al.split(',')[0].trim();
                        }
                        if (!name) continue;
                        let key = el.getAttribute('data-peer-id') || el.getAttribute('data-chat-id') || '';
                        if (!key) key = 'name:' + name.toLowerCase();
                        const muted = el.classList.contains('is-muted')
                            || !!el.querySelector('.dialog-muted-icon, .peer-muted, .chat-muted, [class*="muted-icon"]');
                        let unread = 0;
                        if (!muted) {
                            const badgeEl = el.querySelector('.dialog-subtitle-badge, .unread, .Badge.unread');
                            if (badgeEl && !badgeEl.classList.contains('is-muted') && !badgeEl.classList.contains('muted')) {
                                const num = parseInt(String(badgeEl.textContent || '').replace(/\D+/g, ''), 10);
                                unread = (isFinite(num) && num > 0) ? Math.min(num, 999) : 1;
                            }
                        }
                        const prevEl = el.querySelector('.dialog-subtitle, .last-message, .chat-subtitle, .peer-subtitle');
                        const preview = prevEl
                            ? String(prevEl.textContent || '').replace(/\s+/g, ' ').trim().slice(0, 180)
                            : '';
                        out.push({ key: key, name: name.slice(0, 120), preview: preview, unread: unread, muted: muted });
                    }
                } catch (e) {}
                return out;
            }

            function isVisibleEl(el) {
                if (!el) return false;
                return el.offsetParent !== null || el.getClientRects().length > 0;
            }

            function detectIncomingCallInDom() {
                try {
                    let container = null;
                    if (PLAT_ID === 'zalo') {
                        container = document.querySelector('[class*="incoming-call"], [class*="call-incoming"], [id*="incoming-call"], .zalo-call-modal');
                        if (container && !isVisibleEl(container)) container = null;
                        if (!container) {
                            const btns = document.querySelectorAll('button[title*="Trả lời"], button[title*="Nghe"], button[aria-label*="Accept" i], [data-id*="btn_Accept"], [data-translate-inner*="INCOMING_CALL"]');
                            for (let i = 0; i < btns.length; i++) {
                                if (isVisibleEl(btns[i]) && !btns[i].closest('#messageView, .message-view, .message-view__scroll')) {
                                    container = btns[i].closest('[role="dialog"], .z-modal, .zl-modal, [class*="call"], [class*="modal"]') || btns[i].parentElement;
                                    break;
                                }
                            }
                        }
                    } else if (PLAT_ID === 'messenger') {
                        const hit = document.querySelector('[role="dialog"] [aria-label*="Answer" i], [role="dialog"] [aria-label*="Trả lời" i], [aria-label*="Incoming audio call" i], [aria-label*="Incoming video call" i], [aria-label*="Cuộc gọi đến" i]');
                        if (hit && isVisibleEl(hit)) {
                            container = hit.closest('[role="dialog"]') || hit.parentElement;
                        }
                    } else if (PLAT_ID === 'telegram') {
                        const hit = document.querySelector('.call-notification, .popup-call, .call-btn-answer, button.tgico-call[class*="answer"]');
                        if (hit && isVisibleEl(hit)) {
                            container = hit.closest('.popup, .call-notification, .popup-call') || hit;
                        }
                    } else if (PLAT_ID === 'whatsapp') {
                        const hit = document.querySelector('[data-testid="incoming-call-banner"], [data-testid="call-accept"], [aria-label*="Incoming voice call" i], [aria-label*="Incoming video call" i]');
                        if (hit && isVisibleEl(hit)) {
                            container = hit.closest('[data-testid="incoming-call-banner"], [role="dialog"]') || hit.parentElement;
                        }
                    } else {
                        const hit = document.querySelector('[class*="incoming-call"], [data-testid*="incoming-call"]');
                        if (hit && isVisibleEl(hit)) container = hit;
                    }

                    if (!container) return null;
                    const txt = String(container.textContent || "").replace(/\s+/g, " ").trim();
                    if (!txt) return null;
                    if (/(kết thúc|call ended|đang gọi đi|calling\.\.\.)/i.test(txt) && !/(cuộc gọi đến|incoming|trả lời|answer|nghe)/i.test(txt)) {
                        return null;
                    }
                    const heading = container.querySelector('h1, h2, h3, h4, .name, .caller-name, .call-name, strong');
                    const caller = (heading && heading.textContent ? heading.textContent.trim() : "") || txt.slice(0, 64);
                    const callKind = /video/i.test(txt) ? "video" : "voice";
                    return { caller: caller, callKind: callKind, snippet: txt.slice(0, 140) };
                } catch (e) {
                    return null;
                }
            }

            let pollTick = 0;
            function pollPassiveStatus() {
                try {
                    pollTick++;
                    const unread = readPlatformUnreadCount();
                    const unreadChanged = unread !== lastUnreadCount;
                    if (unread !== lastUnreadCount) {
                        const increased = lastUnreadCount >= 0 && unread > lastUnreadCount;
                        lastUnreadCount = unread;
                        const justNotified = (Date.now() - lastNotifySentAt) < 2500;
                        if (PLAT_ID === 'telegram') {
                            emitProfileEvent("unread", { unread_count: unread, title: "", body: "" });
                        } else {
                            emitProfileEvent(increased && !justNotified ? "message" : "unread", {
                                unread_count: unread,
                                title: increased ? ("Có tin nhắn mới (" + unread + " chưa đọc)") : "",
                                body: ""
                            });
                        }
                    }

                    // Telegram: quét danh sách hội thoại để phát thông báo theo từng nhóm/tin nhắn.
                    // Quét tới 60 dòng nên chỉ chạy khi tổng chưa đọc đổi, hoặc mỗi 5 vòng (~8 giây).
                    if (PLAT_ID === 'telegram' && (unreadChanged || !chatDigestInit || pollTick % 5 === 0)) {
                        const digest = readTelegramChatDigest();
                        for (let di = 0; di < digest.length; di++) {
                            const chat = digest[di];
                            const had = Object.prototype.hasOwnProperty.call(lastChatUnread, chat.key);
                            const prevUnread = had ? lastChatUnread[chat.key] : 0;
                            const isRecent = di < 5;
                            if (chatDigestInit && !chat.muted && chat.unread > prevUnread && (had || isRecent)) {
                                emitProfileEvent("message", {
                                    unread_count: unread,
                                    title: chat.name,
                                    body: chat.preview,
                                    conversation: chat.key
                                });
                            }
                            lastChatUnread[chat.key] = chat.unread;
                        }
                        chatDigestInit = true;
                    }

                    const callInfo = detectIncomingCallInDom();
                    if (callInfo) {
                        const cKey = callInfo.caller + "|" + callInfo.callKind;
                        if (!lastCallActive || lastCallKey !== cKey) {
                            lastCallActive = true;
                            lastCallKey = cKey;
                            emitProfileEvent("call_start", {
                                unread_count: Math.max(lastUnreadCount, 0),
                                title: callInfo.callKind === "video" ? "Cuộc gọi Video đến" : "Cuộc gọi thoại đến",
                                body: callInfo.snippet,
                                caller: callInfo.caller,
                                call_kind: callInfo.callKind
                            });
                        }
                    } else if (lastCallActive) {
                        lastCallActive = false;
                        lastCallKey = "";
                        emitProfileEvent("call_end", { unread_count: Math.max(lastUnreadCount, 0) });
                    }
                } catch (e) {}
            }

            setInterval(pollPassiveStatus, 1600);
            setTimeout(pollPassiveStatus, 2200);
        })();
    "##;

    tpl.replace("__PROFILE_ID__", profile_id)
        .replace("__PLATFORM_ID__", platform)
        .replace(
            "__NOTIFY_ENABLED__",
            if notify_enabled { "true" } else { "false" },
        )
}

fn build_telegram_injection_script() -> String {
    r#"
        (function() {
            if (window.__BUBBLE_TELEGRAM_MEDIA_FIX_V3__) return;
            window.__BUBBLE_TELEGRAM_MEDIA_FIX_V3__ = true;

            // 1. Dọn dẹp CacheStorage / ServiceWorker bị kẹt 1 lần mỗi phiên (Không chạm vào localStorage nên giữ nguyên đăng nhập 100%)
            try {
                                    if (!sessionStorage.getItem('__bch_tg_cache_healed_v4__')) {
                    sessionStorage.setItem('__bch_tg_cache_healed_v4__', '1');
                    if (window.caches && typeof window.caches.keys === 'function') {
                        window.caches.keys().then(function(names) {
                            names.forEach(function(name) {
                                window.caches.delete(name).catch(function() {});
                            });
                        }).catch(function() {});
                    }
                }
            } catch (e) {}

            // 2. Ngăn Telegram Web K (/k/) thu hồi (revoke) sớm các đường dẫn blob: của ảnh khiến MediaViewer bị lỗi ERR_FILE_NOT_FOUND khi click
            const blobKeepAlivePool = new Map();
            try {
                const origCreateObjectURL = URL.createObjectURL;
                const origRevokeObjectURL = URL.revokeObjectURL;

                URL.createObjectURL = function(obj) {
                    const url = origCreateObjectURL.apply(this, arguments);
                    try {
                        if (obj && typeof url === 'string' && url.startsWith('blob:')) {
                            blobKeepAlivePool.set(url, obj);
                            if (blobKeepAlivePool.size > 160) {
                                const oldestKey = blobKeepAlivePool.keys().next().value;
                                if (oldestKey) {
                                    blobKeepAlivePool.delete(oldestKey);
                                    try { origRevokeObjectURL.call(URL, oldestKey); } catch (e) {}
                                }
                            }
                        }
                    } catch (e) {}
                    return url;
                };

                URL.revokeObjectURL = function(url) {
                    setTimeout(function() {
                        try {
                            blobKeepAlivePool.delete(url);
                            origRevokeObjectURL.call(URL, url);
                        } catch (e) {}
                    }, 600000);
                };
            } catch (e) {}

            // 3. Lưu trữ thông tin ảnh vừa được người dùng click để tự động cứu hộ hoặc mở Lightbox Pro
            let lastClickedSrc = "";
            let lastClickedBackupDataUrl = "";
            let galleryUrls = [];
            let currentGalleryIndex = 0;

            function captureImageSnapshot(imgEl) {
                try {
                    if (!imgEl || !imgEl.complete || !imgEl.naturalWidth || imgEl.naturalWidth < 16) return "";
                    const maxDim = 1600;
                    let w = imgEl.naturalWidth;
                    let h = imgEl.naturalHeight;
                    if (w > maxDim || h > maxDim) {
                        const ratio = Math.min(maxDim / w, maxDim / h);
                        w = Math.round(w * ratio);
                        h = Math.round(h * ratio);
                    }
                    const cvs = document.createElement('canvas');
                    cvs.width = w;
                    cvs.height = h;
                    const ctx = cvs.getContext('2d');
                    if (!ctx) return "";
                    ctx.drawImage(imgEl, 0, 0, w, h);
                    return cvs.toDataURL('image/jpeg', 0.94);
                } catch (e) {
                    return "";
                }
            }

            function collectChatGallery(clickedSrc) {
                const list = [];
                try {
                    const imgs = document.querySelectorAll(
                        '.bubbles img.media-photo:not(.thumbnail), .Message img.full-media, .bubbles .attachment img.media-photo, .bubbles .media-container img.media-photo'
                    );
                    imgs.forEach(function(im) {
                        const s = im.currentSrc || im.src || "";
                        if (s && (s.startsWith('blob:') || s.startsWith('data:') || s.startsWith('http')) && !list.includes(s)) {
                            list.push(s);
                        }
                    });
                } catch (e) {}
                if (clickedSrc && !list.includes(clickedSrc)) {
                    list.push(clickedSrc);
                }
                galleryUrls = list;
                currentGalleryIndex = Math.max(0, galleryUrls.indexOf(clickedSrc));
            }

            // 4. Trình Xem Ảnh Pro tích hợp sẵn (Hỗ trợ Zoom con lăn chuột, Kéo ảnh, Xoay 90°, Chuyển ảnh Trái/Phải, Tải xuống, Esc)
            let lbScale = 1;
            let lbRotate = 0;
            let lbTx = 0;
            let lbTy = 0;
            let isDragging = false;
            let dragStartX = 0;
            let dragStartY = 0;

            function closeBubbleLightbox() {
                const lb = document.getElementById('__bch_tg_lightbox__');
                if (lb) lb.style.display = 'none';
                isDragging = false;
            }

            function forceCloseBrokenNativeViewer() {
                try {
                    const whole = document.querySelector('.media-viewer-whole');
                    if (whole) {
                        const closeBtn = whole.querySelector('.media-viewer-close, .btn-icon.tgico-close');
                        if (closeBtn && typeof closeBtn.click === 'function') {
                            try { closeBtn.click(); } catch (e) {}
                        }
                        whole.classList.remove('active');
                        setTimeout(function() {
                            try {
                                const lingering = document.querySelector('.media-viewer-whole:not(.active)');
                                if (lingering) lingering.remove();
                            } catch (e) {}
                        }, 180);
                    }
                    if (document.body) {
                        document.body.classList.remove('overlay-active');
                    }
                } catch (e) {}
            }

            function applyLightboxTransform() {
                const img = document.getElementById('__bch_tg_lightbox_img__');
                const zoomLabel = document.getElementById('__bch_tg_lightbox_zoom__');
                if (img) {
                    img.style.transform = 'translate3d(' + lbTx + 'px, ' + lbTy + 'px, 0) scale(' + lbScale + ') rotate(' + lbRotate + 'deg)';
                }
                if (zoomLabel) {
                    zoomLabel.textContent = Math.round(lbScale * 100) + '%';
                }
            }

            function openBubbleLightbox(srcToOpen) {
                const finalSrc = srcToOpen || lastClickedBackupDataUrl || lastClickedSrc;
                if (!finalSrc || !document.body) return;

                forceCloseBrokenNativeViewer();

                let lb = document.getElementById('__bch_tg_lightbox__');
                if (!lb) {
                    lb = document.createElement('div');
                    lb.id = '__bch_tg_lightbox__';
                    lb.style.cssText = 'position:fixed;inset:0;z-index:2147483646;background:rgba(7,11,20,0.92);backdrop-filter:blur(8px);display:flex;flex-direction:column;user-select:none;font-family:Segoe UI,sans-serif;';

                    const topBar = document.createElement('div');
                    topBar.style.cssText = 'height:54px;padding:0 18px;display:flex;align-items:center;justify-content:space-between;background:rgba(15,23,42,0.88);border-bottom:1px solid rgba(148,163,184,0.18);z-index:3;';

                    const titleBox = document.createElement('div');
                    titleBox.style.cssText = 'color:#f8fafc;font-size:13px;font-weight:700;display:flex;align-items:center;gap:10px;';
                    titleBox.innerHTML = '<span>🖼️ Trình Xem Ảnh Telegram</span><span id="__bch_tg_lightbox_zoom__" style="background:#1e293b;color:#38bdf8;padding:2px 8px;border-radius:6px;font-size:11.5px;">100%</span>';

                    const actions = document.createElement('div');
                    actions.style.cssText = 'display:flex;align-items:center;gap:8px;';

                    function makeBtn(label, title, onClick, isDanger) {
                        const b = document.createElement('button');
                        b.textContent = label;
                        b.title = title;
                        b.style.cssText = 'background:' + (isDanger ? '#ef4444' : '#1e293b') + ';color:#f8fafc;border:1px solid ' + (isDanger ? '#f87171' : '#334155') + ';border-radius:8px;padding:6px 12px;font-size:12.5px;font-weight:700;cursor:pointer;display:flex;align-items:center;gap:5px;transition:all 0.15s;';
                        b.onclick = function(ev) {
                            ev.stopPropagation();
                            onClick();
                        };
                        return b;
                    }

                    actions.appendChild(makeBtn('◀', 'Ảnh trước (Phím mũi tên trái)', function() {
                        if (galleryUrls.length > 1) {
                            currentGalleryIndex = (currentGalleryIndex - 1 + galleryUrls.length) % galleryUrls.length;
                            openBubbleLightbox(galleryUrls[currentGalleryIndex]);
                        }
                    }, false));
                    actions.appendChild(makeBtn('▶', 'Ảnh tiếp theo (Phím mũi tên phải)', function() {
                        if (galleryUrls.length > 1) {
                            currentGalleryIndex = (currentGalleryIndex + 1) % galleryUrls.length;
                            openBubbleLightbox(galleryUrls[currentGalleryIndex]);
                        }
                    }, false));
                    actions.appendChild(makeBtn('🔍 -', 'Thu nhỏ', function() {
                        lbScale = Math.max(0.25, +(lbScale - 0.25).toFixed(2));
                        applyLightboxTransform();
                    }, false));
                    actions.appendChild(makeBtn('1:1', 'Kích thước gốc', function() {
                        lbScale = 1;
                        lbRotate = 0;
                        lbTx = 0;
                        lbTy = 0;
                        applyLightboxTransform();
                    }, false));
                    actions.appendChild(makeBtn('🔍 +', 'Phóng to', function() {
                        lbScale = Math.min(5, +(lbScale + 0.25).toFixed(2));
                        applyLightboxTransform();
                    }, false));
                    actions.appendChild(makeBtn('↺ Xoay', 'Xoay ảnh 90 độ', function() {
                        lbRotate = (lbRotate + 90) % 360;
                        applyLightboxTransform();
                    }, false));
                    actions.appendChild(makeBtn('💾 Tải về', 'Tải ảnh này về máy', function() {
                        const img = document.getElementById('__bch_tg_lightbox_img__');
                        if (!img || !img.src) return;
                        const a = document.createElement('a');
                        a.href = img.src;
                        a.download = 'telegram_photo_' + Date.now() + '.jpg';
                        document.body.appendChild(a);
                        a.click();
                        a.remove();
                    }, false));
                    actions.appendChild(makeBtn('✕ Đóng (Esc)', 'Đóng trình xem ảnh (Phím Esc)', function() {
                        closeBubbleLightbox();
                    }, true));

                    topBar.appendChild(titleBox);
                    topBar.appendChild(actions);

                    const stage = document.createElement('div');
                    stage.id = '__bch_tg_lightbox_stage__';
                    stage.style.cssText = 'flex:1;position:relative;display:flex;align-items:center;justify-content:center;overflow:hidden;cursor:grab;';
                    stage.onclick = function(ev) {
                        if (ev.target === stage) {
                            closeBubbleLightbox();
                        }
                    };

                    const img = document.createElement('img');
                    img.id = '__bch_tg_lightbox_img__';
                    img.draggable = false;
                    img.style.cssText = 'max-width:92%;max-height:88vh;object-fit:contain;border-radius:6px;box-shadow:0 20px 60px rgba(0,0,0,0.75);transition:transform 0.08s ease-out;cursor:grab;';
                    img.onerror = function() {
                        if (lastClickedBackupDataUrl && img.src !== lastClickedBackupDataUrl) {
                            img.src = lastClickedBackupDataUrl;
                        }
                    };

                    stage.addEventListener('wheel', function(ev) {
                        ev.preventDefault();
                        const delta = ev.deltaY < 0 ? 0.2 : -0.2;
                        lbScale = Math.min(5, Math.max(0.25, +(lbScale + delta).toFixed(2)));
                        applyLightboxTransform();
                    }, { passive: false });

                    stage.addEventListener('mousedown', function(ev) {
                        if (ev.button !== 0) return;
                        isDragging = true;
                        dragStartX = ev.clientX - lbTx;
                        dragStartY = ev.clientY - lbTy;
                        stage.style.cursor = 'grabbing';
                    });
                    window.addEventListener('mousemove', function(ev) {
                        if (!isDragging) return;
                        lbTx = ev.clientX - dragStartX;
                        lbTy = ev.clientY - dragStartY;
                        applyLightboxTransform();
                    });
                    window.addEventListener('mouseup', function() {
                        if (isDragging) {
                            isDragging = false;
                            const st = document.getElementById('__bch_tg_lightbox_stage__');
                            if (st) st.style.cursor = 'grab';
                        }
                    });

                    stage.appendChild(img);
                    lb.appendChild(topBar);
                    lb.appendChild(stage);
                    document.body.appendChild(lb);
                }

                lbScale = 1;
                lbRotate = 0;
                lbTx = 0;
                lbTy = 0;
                const imgEl = document.getElementById('__bch_tg_lightbox_img__');
                if (imgEl) {
                    imgEl.src = finalSrc;
                }
                applyLightboxTransform();
                lb.style.display = 'flex';
            }

            // 5. Chặn window.open cho blob:/image để không bao giờ bị WebView2 chặn trắng hoặc báo lỗi
            try {
                const origWindowOpen = window.open;
                window.open = function(url) {
                    const s = String(url || '');
                    if (s.startsWith('blob:') || s.startsWith('data:image/')) {
                        openBubbleLightbox(s);
                        return window;
                    }
                    return origWindowOpen.apply(this, arguments);
                };
            } catch (e) {}

            // 6. Kiểm tra và tự động sửa chữa `.media-viewer-whole` của Telegram Web K (/k/) hoặc mở Lightbox Pro nếu lỗi
            function inspectAndHealTelegramMediaViewer() {
                try {
                    const whole = document.querySelector('.media-viewer-whole');
                    const webAViewer = document.querySelector('#MediaViewer .MediaViewerSlide--active, #MediaViewer.shown');

                    // Nếu cả 2 trình xem ảnh gốc của Telegram đều không mở sau khi người dùng click vào ảnh
                    if (!whole && !webAViewer) {
                        if (lastClickedBackupDataUrl || lastClickedSrc) {
                            openBubbleLightbox(lastClickedBackupDataUrl || lastClickedSrc);
                        }
                        return;
                    }

                    if (whole) {
                        // Đảm bảo lớp overlay active hiển thị đúng
                        if (!whole.classList.contains('active')) {
                            whole.classList.add('active');
                        }
                        whole.style.opacity = '1';
                        whole.style.visibility = 'visible';

                        // Nếu có video đang phát bên trong thì không can thiệp
                        if (whole.querySelector('video')) {
                            return;
                        }

                        const aspecter = whole.querySelector('.media-viewer-aspecter') || whole.querySelector('.media-viewer-mover');
                        const mover = whole.querySelector('.media-viewer-mover');
                        if (mover) {
                            mover.style.opacity = '1';
                            mover.style.visibility = 'visible';
                        }

                        // Gắn thêm nút "🔍 Trình Xem Ảnh Pro" ngay trên thanh công cụ của Telegram MediaViewer để người dùng có thể phóng to / xoay / tải về bất cứ lúc nào
                        const topbarButtons = whole.querySelector('.media-viewer-buttons');
                        if (topbarButtons && !topbarButtons.querySelector('#__bch_tg_open_pro_btn__')) {
                            const proBtn = document.createElement('button');
                            proBtn.id = '__bch_tg_open_pro_btn__';
                            proBtn.type = 'button';
                            proBtn.textContent = '🔍 Phóng to / Xoay';
                            proBtn.title = 'Mở ảnh này trong Trình Xem Ảnh Pro (Hỗ trợ lăn chuột Zoom, Kéo thả, Xoay 90°, Tải về)';
                            proBtn.style.cssText = 'background:#0284c7;color:#ffffff;border:1px solid #38bdf8;border-radius:8px;padding:5px 10px;font-size:12px;font-weight:700;cursor:pointer;margin-right:8px;height:32px;align-self:center;';
                            proBtn.onclick = function(ev) {
                                ev.preventDefault();
                                ev.stopPropagation();
                                const activeImg = whole.querySelector('img.media-photo:not(.thumbnail)') || whole.querySelector('img.media-photo');
                                const src = (activeImg && (activeImg.currentSrc || activeImg.src)) || lastClickedBackupDataUrl || lastClickedSrc;
                                openBubbleLightbox(src);
                            };
                            topbarButtons.insertBefore(proBtn, topbarButtons.firstChild);
                        }

                        // Kiểm tra xem ảnh full-res trong .media-viewer-whole có tải thành công không
                        let mainImg = whole.querySelector('img.media-photo:not(.thumbnail)');
                        const thumbImg = whole.querySelector('img.media-photo.thumbnail, canvas.media-photo.thumbnail');
                        const hasValidLoadedImg = mainImg && mainImg.complete && mainImg.naturalWidth > 64;

                        if (!hasValidLoadedImg && (lastClickedBackupDataUrl || lastClickedSrc)) {
                            const fallbackSrc = lastClickedBackupDataUrl || lastClickedSrc;
                            if (aspecter) {
                                if (!mainImg) {
                                    mainImg = document.createElement('img');
                                    mainImg.className = 'media-photo';
                                    aspecter.appendChild(mainImg);
                                }
                                mainImg.src = fallbackSrc;
                                mainImg.style.cssText = 'width:100%;height:100%;object-fit:contain;display:block;opacity:1;visibility:visible;z-index:2;';
                            } else if (!thumbImg) {
                                // Nếu cấu trúc .media-viewer-whole bị gãy hoàn toàn -> Chuyển sang Lightbox Pro
                                openBubbleLightbox(fallbackSrc);
                                return;
                            }
                        }

                        // Ẩn vòng xoay preloader bị treo nếu ảnh đã hiển thị
                        const preloaders = whole.querySelectorAll('.preloader-container, .preloader');
                        preloaders.forEach(function(p) {
                            p.style.display = 'none';
                        });
                    }
                } catch (e) {
                    if (lastClickedBackupDataUrl || lastClickedSrc) {
                        openBubbleLightbox(lastClickedBackupDataUrl || lastClickedSrc);
                    }
                }
            }

            // 7. Bắt sự kiện click vào ảnh trong khung chat Telegram
            window.addEventListener('click', function(e) {
                try {
                    window.focus();
                    const target = e.target;
                    if (!target || !(target instanceof Element)) return;

                    // Bỏ qua nếu đang click bên trong Lightbox Pro
                    if (target.closest('#__bch_tg_lightbox__')) return;

                    // Nếu người dùng bấm nút Đóng (X) trên .media-viewer-whole của Telegram mà bị kẹt
                    if (target.closest('.media-viewer-close, .btn-icon.tgico-close')) {
                        setTimeout(function() {
                            const stillOpen = document.querySelector('.media-viewer-whole');
                            if (stillOpen && !stillOpen.classList.contains('active')) {
                                forceCloseBrokenNativeViewer();
                            }
                        }, 220);
                        return;
                    }

                    if (target.closest('.media-viewer-whole, #MediaViewer')) return;

                    // Kiểm tra xem người dùng có đang click vào một khung chứa ảnh trong tin nhắn Telegram không
                    const mediaContainer = target.closest(
                        '.bubble .attachment, .bubble .media-container, .bubble .album-item, .Message .Photo, .Message .media-inner, .webpage-preview'
                    );
                    if (!mediaContainer) return;

                    // Không can thiệp nếu là Video, Audio, Sticker động, File tài liệu hoặc nút bấm
                    if (
                        mediaContainer.querySelector('video, .video-time, .video-play, .audio, .audio-player, rlottie-player, .lottie-player') ||
                        target.closest('a[href^="http"], button, .reply, .document-container, .audio')
                    ) {
                        return;
                    }

                    // Tìm thẻ <img> hoặc <canvas> của bức ảnh vừa click
                    const clickedImg =
                        (target.tagName === 'IMG' ? target : null) ||
                        mediaContainer.querySelector('img.media-photo:not(.thumbnail)') ||
                        mediaContainer.querySelector('img.full-media') ||
                        mediaContainer.querySelector('img.media-photo') ||
                        mediaContainer.querySelector('img');

                    const clickedCanvas = mediaContainer.querySelector('canvas.media-photo, canvas.thumbnail');

                    let bestSrc = "";
                    let backupData = "";

                    if (clickedImg) {
                        bestSrc = clickedImg.currentSrc || clickedImg.src || "";
                        backupData = captureImageSnapshot(clickedImg);
                    }
                    if (!bestSrc && clickedCanvas && typeof clickedCanvas.toDataURL === 'function') {
                        try {
                            bestSrc = clickedCanvas.toDataURL();
                            backupData = bestSrc;
                        } catch (err) {}
                    }

                    if (!bestSrc && !backupData) return;

                    lastClickedSrc = bestSrc;
                    lastClickedBackupDataUrl = backupData || bestSrc;
                    collectChatGallery(lastClickedSrc);

                    // Kiểm tra sau 360ms và 780ms: Nếu trình xem ảnh gốc của Telegram bị lỗi hoặc không mở được -> Tự động sửa hoặc bật Lightbox Pro
                    setTimeout(inspectAndHealTelegramMediaViewer, 360);
                    setTimeout(inspectAndHealTelegramMediaViewer, 780);
                } catch (err) {}
            }, true);

            // 8. Bắt sự kiện lỗi tải ảnh (onerror) bên trong .media-viewer-whole để tự động thế ảnh nét vào ngay lập tức
            window.addEventListener('error', function(e) {
                try {
                    const el = e.target;
                    if (el && el.tagName === 'IMG' && el.closest && el.closest('.media-viewer-whole, #MediaViewer')) {
                        const fallback = lastClickedBackupDataUrl || lastClickedSrc;
                        if (fallback && el.src !== fallback) {
                            el.src = fallback;
                            el.style.opacity = '1';
                            el.style.visibility = 'visible';
                        } else if (fallback) {
                            openBubbleLightbox(fallback);
                        }
                    }
                } catch (err) {}
            }, true);

            // 9. Phím tắt khi đang mở ảnh (Esc để đóng, Mũi tên Trái/Phải để chuyển ảnh)
            window.addEventListener('keydown', function(e) {
                const lb = document.getElementById('__bch_tg_lightbox__');
                const lbVisible = lb && lb.style.display !== 'none';
                if (lbVisible) {
                    if (e.key === 'Escape') {
                        e.preventDefault();
                        e.stopPropagation();
                        closeBubbleLightbox();
                    } else if (e.key === 'ArrowLeft' && galleryUrls.length > 1) {
                        e.preventDefault();
                        currentGalleryIndex = (currentGalleryIndex - 1 + galleryUrls.length) % galleryUrls.length;
                        openBubbleLightbox(galleryUrls[currentGalleryIndex]);
                    } else if (e.key === 'ArrowRight' && galleryUrls.length > 1) {
                        e.preventDefault();
                        currentGalleryIndex = (currentGalleryIndex + 1) % galleryUrls.length;
                        openBubbleLightbox(galleryUrls[currentGalleryIndex]);
                    }
                    return;
                }

                if (e.key === 'Escape') {
                    const whole = document.querySelector('.media-viewer-whole');
                    if (whole) {
                        setTimeout(function() {
                            const stillThere = document.querySelector('.media-viewer-whole');
                            if (stillThere) {
                                forceCloseBrokenNativeViewer();
                            }
                        }, 200);
                    }
                }
            }, true);
        })();
    "#
    .to_string()
}

/// Tự động sửa chữa bộ đệm Media / IndexedDB / Service Worker bị lỗi của Telegram trên ổ cứng
/// (Giữ nguyên 100% thư mục `Local Storage` và `Cookies` để KHÔNG BAO GIỜ làm mất đăng nhập Telegram của người dùng)

/// Kịch bản tiêm theo từng nền tảng:
/// - Mọi nền tảng nhận bộ giám sát thông báo / cuộc gọi (chỉ đọc DOM).
/// - Telegram nhận thêm bộ sửa Media / Blob Viewer.
fn build_injection_script(
    platform: &str,
    profile_id: &str,
    notify_enabled: bool,
    privacy: &PrivacyConfig,
) -> String {
    let notify_script = build_notify_call_monitor_script(platform, profile_id, notify_enabled);
    let mut script = notify_script;
    // Privacy interceptor (chặn đã đọc / đang gõ) — chỉ bật khi có cấu hình.
    if privacy.block_read_receipts || privacy.block_typing {
        script = format!(
            "{}\n{}",
            script,
            build_privacy_script(platform, privacy.block_read_receipts, privacy.block_typing)
        );
    }
    if platform == "telegram" {
        // Tạm tắt script media viewer (có thể gây lỗi gửi tin vào nhóm). Bật lại sau khi xác nhận.
        let telegram_media_fix_enabled: bool = false;
        if telegram_media_fix_enabled {
            return format!("{}\n{}", script, build_telegram_injection_script());
        }
    }
    script
}

/// Telegram Web K giữ cache ảnh cũ trong EBWebView -> xóa 1 lần cho mỗi profile để media hiển thị lại.
fn repair_telegram_profile_media_cache(profile_dir: &Path) {
    let marker = profile_dir.join(".bch_telegram_media_fix_v4");
    if marker.exists() {
        return;
    }
    let default_dir = profile_dir.join("EBWebView").join("Default");
    if default_dir.exists() {
        for folder in ["Cache", "Code Cache"] {
            let target = default_dir.join(folder);
            if target.exists() {
                let _ = fs::remove_dir_all(&target);
            }
        }
    }
    let _ = fs::write(marker, "healed_v4");
}

pub(crate) fn current_epoch_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Đóng bớt WebView của các tài khoản ít dùng gần đây để giới hạn RAM.
/// Phiên đăng nhập vẫn nằm trên ổ đĩa (`chat_profiles/...`) nên mở lại vẫn còn đăng nhập.
fn evict_idle_webviews(app: &AppHandle, state: &Arc<WorkspaceState>) {
    let limit = state.max_live_webviews.lock().map(|g| *g).unwrap_or(3);
    if limit == 0 {
        return;
    }

    let active = state.active_chat_label.lock().ok().and_then(|g| g.clone());
    let labels: Vec<String> = app
        .webview_windows()
        .keys()
        .filter(|l| l.starts_with("chat_"))
        .cloned()
        .collect();
    if labels.len() <= limit {
        return;
    }

    let last_used = state.last_used.lock().map(|m| m.clone()).unwrap_or_default();
    let mut candidates: Vec<(u64, String)> = labels
        .iter()
        .filter(|l| Some((*l).clone()) != active)
        .map(|l| (last_used.get(l).copied().unwrap_or(0), l.clone()))
        .collect();
    candidates.sort_by_key(|(ts, _)| *ts);

    let to_close = labels.len().saturating_sub(limit);
    for (_, label) in candidates.into_iter().take(to_close) {
        if let Some(win) = app.get_webview_window(&label) {
            let _ = win.close();
        }
        if let Ok(mut map) = state.created_hwnds.lock() {
            map.remove(&label);
        }
        if let Ok(mut m) = state.last_used.lock() {
            m.remove(&label);
        }
        if let Ok(mut m) = state.label_profile.lock() {
            m.remove(&label);
        }
    }
}

fn make_window_label(platform: &str, profile_id: &str) -> String {
    let clean_plat: String = platform
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();
    let clean_id: String = profile_id
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();
    format!("chat_{}_{}", clean_plat, clean_id)
}

/// Ẩn triệt để cửa sổ WebView con ở cấp cửa sổ ngoài (Tauri Window + Win32 HWND).
/// Không dùng `as_ref().hide()` vì có thể làm WebView2 đóng băng requestAnimationFrame của trang.
/// Đặt chế độ tiết kiệm cho WebView2 của một tài khoản.
/// - `low_memory`: `MemoryUsageTargetLevel = Low` (WebView2 tự giảm bộ nhớ, JS vẫn chạy).
/// - `sleep`: `IsVisible = false` + `TrySuspend` (dừng JS/timer, không còn báo tin mới).
///   `sleep = false` thì bật lại `IsVisible` + `Resume`.
#[cfg(target_os = "windows")]
fn set_webview_power(win: &tauri::WebviewWindow, sleep: bool, low_memory: bool) {
    use webview2_com::Microsoft::Web::WebView2::Win32::{
        ICoreWebView2_19, ICoreWebView2_3, COREWEBVIEW2_MEMORY_USAGE_TARGET_LEVEL_LOW,
        COREWEBVIEW2_MEMORY_USAGE_TARGET_LEVEL_NORMAL,
    };
    use windows::core::Interface;

    let _ = win.with_webview(move |pw| unsafe {
        let controller = pw.controller();
        let Ok(core) = controller.CoreWebView2() else {
            return;
        };
        let set_memory_level = || {
            if let Ok(core19) = core.cast::<ICoreWebView2_19>() {
                let level = if low_memory {
                    COREWEBVIEW2_MEMORY_USAGE_TARGET_LEVEL_LOW
                } else {
                    COREWEBVIEW2_MEMORY_USAGE_TARGET_LEVEL_NORMAL
                };
                let _ = core19.SetMemoryUsageTargetLevel(level);
            }
        };
        let core3 = core.cast::<ICoreWebView2_3>().ok();
        if sleep {
            set_memory_level();
            let _ = controller.SetIsVisible(false);
            if let Some(core3) = core3 {
                let handler =
                    webview2_com::TrySuspendCompletedHandler::create(Box::new(|_, _| Ok(())));
                let _ = core3.TrySuspend(&handler);
            }
        } else {
            // Resume đặt lại mức bộ nhớ về Normal, nên phải đặt mức sau khi đã đánh thức.
            let _ = controller.SetIsVisible(true);
            if let Some(core3) = core3 {
                let _ = core3.Resume();
            }
            set_memory_level();
        }
    });
}

#[cfg(not(target_os = "windows"))]
fn set_webview_power(_win: &tauri::WebviewWindow, _sleep: bool, _low_memory: bool) {}

fn hide_chat_window_completely(win: &tauri::WebviewWindow) {
    #[cfg(target_os = "windows")]
    {
        // Cửa sổ con: chỉ cần SW_HIDE ở cấp HWND, không di chuyển để tránh lệch toạ độ client.
        if let Ok(hwnd) = win.hwnd() {
            win32_hide_hwnd(hwnd.0 as isize);
        }
    }

    #[cfg(not(target_os = "windows"))]
    {
        let _ = win.hide();
        let _ = win.set_position(Position::Physical(PhysicalPosition {
            x: -32000,
            y: -32000,
        }));
    }
}

/// Ẩn toàn bộ cửa sổ chat (cả trên luồng hiện tại lẫn đồng bộ trên Main UI Thread)
fn hide_all_chat_windows_everywhere(app: &AppHandle, state: &Arc<WorkspaceState>) {
    if let Ok(mut lb) = state.last_bounds.lock() {
        *lb = None;
    }
    let mut hwnds_to_hide: Vec<isize> = Vec::new();

    if let Ok(map) = state.created_hwnds.lock() {
        for &h in map.values() {
            if h != 0 {
                hwnds_to_hide.push(h);
            }
        }
    }

    for (label, win) in app.webview_windows() {
        if label.starts_with("chat_") {
            #[cfg(target_os = "windows")]
            if let Ok(hwnd) = win.hwnd() {
                let raw = hwnd.0 as isize;
                if raw != 0 && !hwnds_to_hide.contains(&raw) {
                    hwnds_to_hide.push(raw);
                }
            }
            hide_chat_window_completely(&win);
        }
    }

    for &raw_hwnd in &hwnds_to_hide {
        win32_hide_hwnd(raw_hwnd);
    }

    let app_clone = app.clone();
    let state_clone = Arc::clone(state);
    let _ = app.run_on_main_thread(move || {
        let modal_still_open = state_clone
            .modal_overlay_open
            .lock()
            .map(|g| *g)
            .unwrap_or(false);
        let main_minimized = app_clone
            .get_webview_window("main")
            .and_then(|w| w.is_minimized().ok())
            .unwrap_or(false);
        let no_active = state_clone
            .active_chat_label
            .lock()
            .map(|g| g.is_none())
            .unwrap_or(true);

        if modal_still_open || main_minimized || no_active {
            for (label, win) in app_clone.webview_windows() {
                if label.starts_with("chat_") {
                    hide_chat_window_completely(&win);
                }
            }
            for &raw_hwnd in &hwnds_to_hide {
                win32_hide_hwnd(raw_hwnd);
            }
        }
    });
}

/// Hiển thị lại cửa sổ chat đúng khung bên phải cửa sổ chính (chống gọi thừa & nháy khung hình)
fn show_chat_window_at_bounds(
    app: &AppHandle,
    state: &Arc<WorkspaceState>,
    active_label: &str,
    win: &tauri::WebviewWindow,
    x: i32,
    y: i32,
    w: u32,
    h: u32,
) {
    if state
        .modal_overlay_open
        .lock()
        .map(|g| *g)
        .unwrap_or(false)
    {
        if let Ok(mut lb) = state.last_bounds.lock() {
            *lb = None;
        }
        hide_chat_window_completely(win);
        return;
    }

    let next_bounds = (active_label.to_string(), x, y, w, h);
    let win_visible = win.is_visible().unwrap_or(false);
    if let Ok(mut lb) = state.last_bounds.lock() {
        if win_visible && lb.as_ref() == Some(&next_bounds) {
            return;
        }
        *lb = Some(next_bounds.clone());
    }

    #[cfg(target_os = "windows")]
    let raw_hwnd = win.hwnd().map(|hwnd| hwnd.0 as isize).unwrap_or(0);
    #[cfg(not(target_os = "windows"))]
    let raw_hwnd: isize = 0;

    let win_clone = win.clone();
    let state_clone = Arc::clone(state);
    let _ = app.run_on_main_thread(move || {
        if state_clone
            .modal_overlay_open
            .lock()
            .map(|g| *g)
            .unwrap_or(false)
        {
            hide_chat_window_completely(&win_clone);
            return;
        }
        // Bỏ qua các frame kéo/thay đổi kích thước cũ đã lỗi thời trong hàng đợi
        if let Ok(lb) = state_clone.last_bounds.lock() {
            if lb.as_ref() != Some(&next_bounds) {
                return;
            }
        }
        // Windows: x,y là toạ độ CLIENT của cửa sổ chính (cửa sổ con tự di chuyển theo cha).
        // Không dùng API Tauri (set_size/set_position/show) vì chúng xử lý như cửa sổ top-level.
        #[cfg(target_os = "windows")]
        if raw_hwnd != 0 {
            win32_show_hwnd(raw_hwnd, x, y, w, h);
        }

        #[cfg(not(target_os = "windows"))]
        {
            let _ = win_clone.set_size(Size::Physical(PhysicalSize {
                width: w,
                height: h,
            }));
            let _ = win_clone.set_position(Position::Physical(PhysicalPosition { x, y }));
            let _ = win_clone.show();
        }
    });
}

/// Đồng bộ tọa độ & kích thước khung Chat khớp vào phần bên phải cửa sổ chính (theo nấc thu gọn Sidebar)
fn sync_embedded_chat_bounds(app: &AppHandle, state: &Arc<WorkspaceState>) {
    let active_label_opt = state.active_chat_label.lock().ok().and_then(|g| g.clone());
    let modal_open = state
        .modal_overlay_open
        .lock()
        .map(|g| *g)
        .unwrap_or(false);
    let sidebar_logical_w = state
        .sidebar_logical_width
        .lock()
        .map(|g| *g)
        .unwrap_or(DEFAULT_SIDEBAR_LOGICAL_WIDTH);

    let Some(main_win) = app.get_webview_window("main") else {
        return;
    };

    // Đang mở modal / thu nhỏ cửa sổ chính / chưa chọn tài khoản -> ẩn hết cửa sổ chat
    if modal_open || main_win.is_minimized().unwrap_or(false) || active_label_opt.is_none() {
        hide_all_chat_windows_everywhere(app, state);
        return;
    }

    let Some(active_label) = active_label_opt else {
        return;
    };

    for (label, win) in app.webview_windows() {
        if label.starts_with("chat_")
            && label != active_label
            && win.is_visible().unwrap_or(false)
        {
            hide_chat_window_completely(&win);
        }
    }

    let Some(chat_win) = app.get_webview_window(&active_label) else {
        return;
    };

    if let (Ok(inner_size), Ok(scale)) = (main_win.inner_size(), main_win.scale_factor()) {
        let right_logical_w = state
            .right_inset_logical
            .lock()
            .map(|g| *g)
            .unwrap_or(0.0);
        let left_px = (sidebar_logical_w * scale).round() as u32;
        let right_px = (right_logical_w * scale).round() as u32;
        // Cửa sổ chat là CON của cửa sổ chính -> toạ độ tính theo vùng client của cha.
        let chat_x = left_px as i32;
        let chat_y = 0;
        let chat_w = inner_size
            .width
            .saturating_sub(left_px)
            .saturating_sub(right_px)
            .max(200);
        let chat_h = inner_size.height.max(200);

        show_chat_window_at_bounds(
            app,
            state,
            &active_label,
            &chat_win,
            chat_x,
            chat_y,
            chat_w,
            chat_h,
        );
    }
}

/// Đổi độ rộng Sidebar (310px đầy đủ / 68px chỉ icon) và cập nhật khung Chat ngay lập tức
#[tauri::command]
async fn set_sidebar_width(
    app: AppHandle,
    state: State<'_, Arc<WorkspaceState>>,
    width: f64,
) -> Result<(), String> {
    if let Ok(mut guard) = state.sidebar_logical_width.lock() {
        *guard = width.clamp(28.0, 500.0);
    }
    sync_embedded_chat_bounds(&app, state.inner());
    Ok(())
}

/// Đặt vùng chừa hai bên cho khung Chat (thanh điều hướng có thể ở trái hoặc phải,
/// và khung tài khoản có thể bị ẩn hoàn toàn).
#[tauri::command]
async fn set_chat_insets(
    app: AppHandle,
    state: State<'_, Arc<WorkspaceState>>,
    left: f64,
    right: f64,
) -> Result<(), String> {
    if let Ok(mut guard) = state.sidebar_logical_width.lock() {
        *guard = left.clamp(0.0, 2000.0);
    }
    if let Ok(mut guard) = state.right_inset_logical.lock() {
        *guard = right.clamp(0.0, 2000.0);
    }
    sync_embedded_chat_bounds(&app, state.inner());
    Ok(())
}

/// Chuyển tài khoản đang hiển thị ở khung Chat bên phải.
/// Cửa sổ của từng tài khoản được tạo một lần, giữ nguyên phiên đăng nhập, chỉ ẩn/hiện.
#[tauri::command]
async fn switch_active_chat_account(
    app: AppHandle,
    state: State<'_, Arc<WorkspaceState>>,
    platform: String,
    url: String,
    profile_id: String,
    profile_name: Option<String>,
    sidebar_width: Option<f64>,
    web_config: Option<String>,
) -> Result<String, String> {
    let _op_guard = state.window_op_lock.lock().map_err(|e| e.to_string())?;

    if let Some(w) = sidebar_width {
        if let Ok(mut guard) = state.sidebar_logical_width.lock() {
            // Cho phép 0: khi thanh điều hướng nằm bên phải (hoặc khung tài khoản bị ẩn)
            // thì khung chat phải bám sát mép trái cửa sổ, không chừa vệt trống.
            *guard = w.clamp(0.0, 2000.0);
        }
    }

    let target_label = make_window_label(&platform, &profile_id);

    if let Ok(mut guard) = state.active_chat_label.lock() {
        *guard = Some(target_label.clone());
    }
    if let Ok(mut m) = state.last_used.lock() {
        m.insert(target_label.clone(), current_epoch_ms());
    }
    if let Ok(mut lb) = state.last_bounds.lock() {
        *lb = None;
    }
    if let Ok(mut m) = state.label_profile.lock() {
        m.insert(target_label.clone(), profile_id.clone());
    }

    // Ẩn ngay các cửa sổ khác và cho chúng chạy tiết kiệm
    for (label, win) in app.webview_windows() {
        if label.starts_with("chat_") && label != target_label {
            hide_chat_window_completely(&win);
            set_webview_power(&win, state.should_sleep_hidden(&label), true);
        }
    }
    if let Some(win) = app.get_webview_window(&target_label) {
        set_webview_power(&win, false, false);
    }
    if let Ok(map) = state.created_hwnds.lock() {
        for (label, &raw_hwnd) in map.iter() {
            if label != &target_label && raw_hwnd != 0 {
                win32_hide_hwnd(raw_hwnd);
            }
        }
    }

    if app.get_webview_window(&target_label).is_none() {
        let app_data_dir = app.path().app_local_data_dir().map_err(|e| e.to_string())?;
        let profile_dir = app_data_dir
            .join("chat_profiles")
            .join(&platform)
            .join(&profile_id);
        fs::create_dir_all(&profile_dir).map_err(|e| e.to_string())?;

        let is_telegram =
            platform == "telegram" || url.to_ascii_lowercase().contains("telegram.org");
        if is_telegram {
            repair_telegram_profile_media_cache(&profile_dir);
        }

        let parsed_url = tauri::Url::parse(&url).map_err(|e| e.to_string())?;
        let user_agent = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36";
        let display_name = profile_name.unwrap_or_else(|| profile_id.clone());
        let sidebar_logical_w = state
            .sidebar_logical_width
            .lock()
            .map(|g| *g)
            .unwrap_or(DEFAULT_SIDEBAR_LOGICAL_WIDTH);

        let mut init_logical_w = 960.0;
        let mut init_logical_h = 760.0;

        if let Some(main_win) = app.get_webview_window("main") {
            if let (Ok(inner_size), Ok(scale)) = (main_win.inner_size(), main_win.scale_factor()) {
                let right_logical_w = state
                    .right_inset_logical
                    .lock()
                    .map(|g| *g)
                    .unwrap_or(0.0);
                let left_px = (sidebar_logical_w * scale).round() as u32;
                let right_px = (right_logical_w * scale).round() as u32;
                let init_w = inner_size
                    .width
                    .saturating_sub(left_px)
                    .saturating_sub(right_px)
                    .max(200);
                let init_h = inner_size.height.max(200);
                init_logical_w = (init_w as f64 / scale).max(200.0);
                init_logical_h = (init_h as f64 / scale).max(200.0);
            }
        }

        let state_for_title = Arc::clone(state.inner());

        // Luôn tạo cửa sổ ở trạng thái ẩn + tọa độ âm + nền tối để không nháy khung trắng
        // trong lúc WebView2 khởi tạo tiến trình con cho profile mới.
        let mut builder =
            WebviewWindowBuilder::new(&app, &target_label, WebviewUrl::External(parsed_url))
                .title(format!("{} — [{}]", display_name, platform.to_uppercase()))
                .position(-32000.0, -32000.0)
                .inner_size(init_logical_w, init_logical_h)
                .background_color(tauri::window::Color(7, 11, 20, 255))
                .decorations(false)
                .resizable(false)
                .skip_taskbar(true)
                .focused(false)
                .visible(false)
                .accept_first_mouse(true)
                .disable_drag_drop_handler()
                .enable_clipboard_access()
                // WebView2 không tự cấp quyền micro/camera: nếu không xử lý, cuộc gọi đến
                // sẽ không thể bắt máy vì getUserMedia bị từ chối.
                .on_permission_request(|_webview, kind| match kind {
                    tauri::webview::PermissionKind::Microphone
                    | tauri::webview::PermissionKind::Camera => tauri::webview::PermissionResponse::Allow,
                    _ => tauri::webview::PermissionResponse::Default,
                })
                .zoom_hotkeys_enabled(true)
                .on_document_title_changed(move |_win, title| {
                    if title.starts_with("__BCH_") {
                        state_for_title.handle_webview_title_ipc(&title);
                    }
                })
                .on_new_window(|new_url, _features| {
                    let host = new_url.host_str().unwrap_or("").to_ascii_lowercase();
                    let is_auth_host = host.contains("facebook.com")
                        || host.contains("messenger.com")
                        || host.contains("accounts.google.com")
                        || host.contains("login.microsoftonline.com");
                    // Với luồng đăng nhập Messenger/Facebook, không mở trình duyệt ngoài (làm đứt phiên).
                    // Để site tự xử lý trong cùng WebView (redirect/popup nội bộ).
                    if is_auth_host {
                        return tauri::webview::NewWindowResponse::Deny;
                    }
                    if new_url.scheme() == "http" || new_url.scheme() == "https" {
                        #[cfg(target_os = "windows")]
                        {
                            let _ = std::process::Command::new("rundll32.exe")
                                .args(["url.dll,FileProtocolHandler", new_url.as_str()])
                                .spawn();
                        }
                    }
                    tauri::webview::NewWindowResponse::Deny
                })
                .data_directory(profile_dir);

        if !is_telegram {
            builder = builder.user_agent(user_agent);
        }

        // Cấu hình mạng riêng của profile (proxy + chống lộ IP thật).
        // Chromium gắn proxy ở mức environment, nên chỉ áp dụng được lúc tạo WebView.
        let profile_web: ProfileWebConfig = web_config
            .as_deref()
            .and_then(|raw| serde_json::from_str(raw).ok())
            .unwrap_or_default();
        // Bật cờ tắt tracking prevention của WebView2: Messenger/Facebook và một số site
        // otherwise chặn cookie third-party, khiến phiên đăng nhập không được lưu.
        let mut extra_args = String::from("--disable-features=msTrackingPrevention,ThirdPartyStoragePartitioning,BlockThirdPartyCookies --disable-blink-features=AutomationControlled");
        match net::build_proxy_args(&profile_web.proxy) {
            Ok(Some(proxy_args)) => {
                extra_args.push(' ');
                extra_args.push_str(&proxy_args);
            }
            Ok(None) => {}
            Err(err) => return Err(format!("Cấu hình proxy không hợp lệ: {err}")),
        }
        #[cfg(target_os = "windows")]
        {
            builder = builder.additional_browser_args(&extra_args);
        }
        #[cfg(not(target_os = "windows"))]
        {
            let _ = extra_args;
        }

        if let Some(main_win) = app.get_webview_window("main") {
            builder = builder.parent(&main_win).map_err(|e| e.to_string())?;
        }

        let notify_enabled = state.is_profile_notify_enabled(&profile_id);
        let script_platform = if is_telegram { "telegram" } else { platform.as_str() };
        let profile_privacy = profile_web.privacy.clone();
        let mut script = build_injection_script(
            script_platform,
            &profile_id,
            notify_enabled,
            &profile_privacy,
        );
        if !script.is_empty() {
            script.push('\n');
        }
        script.push_str(&bridge::shim_script(script_platform, &profile_id));
        builder = builder.initialization_script(script);

        let built_win = match builder.build() {
            Ok(win) => win,
            Err(err) => {
                eprintln!("[chat-window] tạo cửa sổ lỗi: {err}");
                return Err(err.to_string());
            }
        };
        bridge::attach(
            &built_win,
            app.clone(),
            script_platform.to_string(),
            profile_id.clone(),
        );

        #[cfg(target_os = "windows")]
        if let Ok(hwnd) = built_win.hwnd() {
            let raw_hwnd = hwnd.0 as isize;
            let parent_hwnd = app
                .get_webview_window("main")
                .and_then(|w| w.hwnd().ok())
                .map(|h| h.0 as isize)
                .unwrap_or(0);
            win32_allow_drag_drop_messages(raw_hwnd);
            win32_hide_hwnd(raw_hwnd);
            // Gắn thành cửa sổ CON thật sự để không nổi lên trên app khác và bị cắt theo cửa sổ chính.
            win32_attach_as_child(raw_hwnd, parent_hwnd);
            if let Ok(mut map) = state.created_hwnds.lock() {
                map.insert(target_label.clone(), raw_hwnd);
            }
        }

        if state
            .modal_overlay_open
            .lock()
            .map(|g| *g)
            .unwrap_or(false)
        {
            hide_chat_window_completely(&built_win);
        }
    }

    sync_embedded_chat_bounds(&app, state.inner());
    // Giữ số WebView trong giới hạn RAM cho phép (mặc định 3)
    evict_idle_webviews(&app, state.inner());
    Ok("switched".to_string())
}

#[tauri::command]
async fn open_chat_window(
    app: AppHandle,
    state: State<'_, Arc<WorkspaceState>>,
    platform: String,
    url: String,
    profile_id: String,
    profile_name: Option<String>,
    web_config: Option<String>,
) -> Result<String, String> {
    switch_active_chat_account(
        app,
        state,
        platform,
        url,
        profile_id,
        profile_name,
        None,
        web_config,
    )
    .await
}

#[tauri::command]
async fn toggle_chat_window(
    app: AppHandle,
    state: State<'_, Arc<WorkspaceState>>,
    platform: String,
    profile_id: String,
    url: String,
    profile_name: Option<String>,
    web_config: Option<String>,
) -> Result<String, String> {
    switch_active_chat_account(
        app,
        state,
        platform,
        url,
        profile_id,
        profile_name,
        None,
        web_config,
    )
    .await
}

/// Bật/tắt chế độ phủ modal: ẩn toàn bộ WebView con để modal React hiển thị đè lên
#[tauri::command]
async fn set_modal_overlay_mode(
    app: AppHandle,
    state: State<'_, Arc<WorkspaceState>>,
    open: bool,
) -> Result<(), String> {
    let prev = state
        .modal_overlay_open
        .lock()
        .map(|g| *g)
        .unwrap_or(false);
    if prev == open {
        return Ok(());
    }
    if let Ok(mut guard) = state.modal_overlay_open.lock() {
        *guard = open;
    }
    if let Ok(mut lb) = state.last_bounds.lock() {
        *lb = None;
    }
    if open {
        hide_all_chat_windows_everywhere(&app, state.inner());
        if let Some(main_win) = app.get_webview_window("main") {
            let _ = main_win.set_focus();
        }
    } else {
        sync_embedded_chat_bounds(&app, state.inner());
    }
    Ok(())
}

#[tauri::command]
async fn reload_chat_account(
    app: AppHandle,
    platform: String,
    profile_id: String,
) -> Result<(), String> {
    let label = make_window_label(&platform, &profile_id);
    if let Some(win) = app.get_webview_window(&label) {
        if platform == "telegram" {
            let _ = win.eval(
                r#"
                (function() {
                    try {
                        if (window.caches && typeof window.caches.keys === 'function') {
                            window.caches.keys().then(function(names) {
                                Promise.all(names.map(function(n) { return window.caches.delete(n); }))
                                    .finally(function() { window.location.reload(); });
                            }).catch(function() { window.location.reload(); });
                            return;
                        }
                    } catch (e) {}
                    window.location.reload();
                })();
                "#,
            );
        } else {
            let _ = win.eval("window.location.reload();");
        }
    }
    Ok(())
}

#[tauri::command]
async fn close_chat_window(
    app: AppHandle,
    state: State<'_, Arc<WorkspaceState>>,
    platform: String,
    profile_id: String,
) -> Result<String, String> {
    let _op_guard = state.window_op_lock.lock().map_err(|e| e.to_string())?;
    let label = make_window_label(&platform, &profile_id);
    if let Ok(mut guard) = state.active_chat_label.lock() {
        if guard.as_deref() == Some(label.as_str()) {
            *guard = None;
        }
    }
    if let Ok(mut lb) = state.last_bounds.lock() {
        *lb = None;
    }
    if let Ok(mut map) = state.created_hwnds.lock() {
        if let Some(raw_hwnd) = map.remove(&label) {
            win32_hide_hwnd(raw_hwnd);
        }
    }
    if let Ok(mut m) = state.last_used.lock() {
        m.remove(&label);
    }
    if let Some(win) = app.get_webview_window(&label) {
        let _ = win.close();
    }
    Ok("closed".to_string())
}

/// Ghi bản sao lưu danh sách tài khoản/nền tảng ra `workspace_snapshot.json`
/// (localStorage của WebView2 có thể bị xoá hoặc đầy dung lượng).
#[tauri::command]
async fn save_workspace_snapshot(app: AppHandle, payload: String) -> Result<(), String> {
    let base = app.path().app_local_data_dir().map_err(|e| e.to_string())?;
    fs::create_dir_all(&base).map_err(|e| e.to_string())?;
    let tmp = base.join("workspace_snapshot.json.tmp");
    let dst = base.join("workspace_snapshot.json");
    fs::write(&tmp, payload).map_err(|e| e.to_string())?;
    fs::rename(&tmp, &dst).map_err(|e| e.to_string())?;
    Ok(())
}

/// Đọc bản sao lưu nếu có.
#[tauri::command]
async fn load_workspace_snapshot(app: AppHandle) -> Result<Option<String>, String> {
    let base = app.path().app_local_data_dir().map_err(|e| e.to_string())?;
    let path = base.join("workspace_snapshot.json");
    if !path.exists() {
        return Ok(None);
    }
    fs::read_to_string(&path).map(Some).map_err(|e| e.to_string())
}

/// JS điền tin nhắn vào ô soạn thảo của khung chat (không phụ thuộc framework của site).
/// @@TEXT@@ = chuỗi JSON, @@SEL@@ = mảng selector ưu tiên, @@SEND@@ = true/false.
const REPLY_JS_TEMPLATE: &str = r#"(function () {
  try {
    var text = @@TEXT@@;
    var shouldSend = @@SEND@@;
    function visible(el) {
      if (!el) return false;
      var r = el.getBoundingClientRect();
      return r.width > 0 && r.height > 0;
    }
    var sels = @@SEL@@;
    var el = null;
    for (var i = 0; i < sels.length; i++) {
      var cand = document.querySelector(sels[i]);
      if (visible(cand)) { el = cand; break; }
    }
    if (!el) {
      var all = Array.prototype.slice.call(
        document.querySelectorAll('div[contenteditable="true"], textarea')
      ).filter(visible);
      all.sort(function (a, b) {
        return b.getBoundingClientRect().top - a.getBoundingClientRect().top;
      });
      el = all[0] || null;
    }
    if (!el) return;
    el.focus();
    if (el.tagName === 'TEXTAREA' || el.tagName === 'INPUT') {
      var proto = el.tagName === 'TEXTAREA'
        ? window.HTMLTextAreaElement.prototype
        : window.HTMLInputElement.prototype;
      var setter = Object.getOwnPropertyDescriptor(proto, 'value').set;
      setter.call(el, text);
      el.dispatchEvent(new Event('input', { bubbles: true }));
    } else {
      var sel = window.getSelection();
      var range = document.createRange();
      range.selectNodeContents(el);
      range.collapse(false);
      sel.removeAllRanges();
      sel.addRange(range);
      document.execCommand('insertText', false, text);
      el.dispatchEvent(new InputEvent('input', { bubbles: true, data: text, inputType: 'insertText' }));
    }
    if (!shouldSend) return;
    var opts = { key: 'Enter', code: 'Enter', keyCode: 13, which: 13, bubbles: true, cancelable: true };
    el.dispatchEvent(new KeyboardEvent('keydown', opts));
    el.dispatchEvent(new KeyboardEvent('keypress', opts));
    el.dispatchEvent(new KeyboardEvent('keyup', opts));
  } catch (e) {}
})();"#;

fn build_reply_script(platform: &str, text: &str, send: bool) -> String {
    let text_js = serde_json::to_string(text).unwrap_or_else(|_| String::from("\"\""));
    let selectors = match platform {
        "zalo" => {
            r##"["#rich-input",".chat-input-content div[contenteditable='true']","div[contenteditable='true']"]"##
        }
        "messenger" => {
            r##"["div[contenteditable='true'][role='textbox']","[aria-label='Message'] div[contenteditable='true']"]"##
        }
        "telegram" => {
            r##"["div.input-message-input[contenteditable='true']","#editable-message-text"]"##
        }
        "whatsapp" => {
            r##"["footer div[contenteditable='true']","div[contenteditable='true'][data-tab='10']"]"##
        }
        _ => "[]",
    };
    REPLY_JS_TEMPLATE
        .replace("@@TEXT@@", &text_js)
        .replace("@@SEL@@", selectors)
        .replace("@@SEND@@", if send { "true" } else { "false" })
}

/// Điền (và tuỳ chọn gửi) một tin nhắn vào ô soạn thảo của khung chat đang mở.
/// `send = false` chỉ điền để người dùng tự kiểm tra trước khi gửi.
#[tauri::command]
async fn send_chat_message(
    app: AppHandle,
    platform: String,
    profile_id: String,
    text: String,
    send: bool,
) -> Result<String, String> {
    let trimmed = text.trim().to_string();
    if trimmed.is_empty() {
        return Err("Nội dung tin nhắn trống".to_string());
    }
    if trimmed.chars().count() > 4000 {
        return Err("Tin nhắn quá dài (tối đa 4000 ký tự)".to_string());
    }
    let label = make_window_label(&platform, &profile_id);
    let Some(win) = app.get_webview_window(&label) else {
        return Err("Khung chat chưa mở — hãy mở tài khoản trước khi trả lời".to_string());
    };
    let js = build_reply_script(&platform, &trimmed, send);
    win.eval(&js).map_err(|e| e.to_string())?;
    Ok(if send { "sent" } else { "filled" }.to_string())
}

/// Kiểm tra proxy của profile: kết nối tới `api.ipify.org` qua proxy và trả về IP công khai.
#[tauri::command]
async fn test_profile_proxy(web_config: String) -> Result<net::ProxyTestResult, String> {
    let cfg: ProfileWebConfig =
        serde_json::from_str(&web_config).map_err(|e| format!("Cấu hình không hợp lệ: {e}"))?;
    tauri::async_runtime::spawn_blocking(move || net::test_proxy(&cfg.proxy))
        .await
        .map_err(|e| e.to_string())
}

/// Gửi lệnh ping qua bridge v2 vào khung chat đang mở (kết quả về qua event `bch-bridge-message`).
#[tauri::command]
async fn bch_bridge_ping(
    app: AppHandle,
    platform: String,
    profile_id: String,
) -> Result<(), String> {
    let label = make_window_label(&platform, &profile_id);
    let Some(win) = app.get_webview_window(&label) else {
        return Err("Khung chat chưa mở — hãy chọn tài khoản này trước khi kiểm tra".to_string());
    };
    win.eval("try { window.__BCH_BRIDGE__ && window.__BCH_BRIDGE__.ping(); } catch (e) {}")
        .map_err(|e| e.to_string())?;
    Ok(())
}

/// Liệt kê mọi profile đang có trên đĩa dưới dạng `<platform>/<profile_id>`.
/// Dùng để khôi phục tài khoản bị mất khỏi danh sách nhưng dữ liệu vẫn còn.
#[tauri::command]
async fn list_profile_dirs(app: AppHandle) -> Result<Vec<String>, String> {
    let base = app.path().app_local_data_dir().map_err(|e| e.to_string())?;
    let root = base.join("chat_profiles");
    let mut out: Vec<String> = Vec::new();

    let Ok(platforms) = fs::read_dir(&root) else {
        return Ok(out);
    };
    for platform_entry in platforms.flatten() {
        if !platform_entry.path().is_dir() {
            continue;
        }
        let platform = platform_entry.file_name().to_string_lossy().to_string();
        let Ok(profiles) = fs::read_dir(platform_entry.path()) else {
            continue;
        };
        for profile_entry in profiles.flatten() {
            if profile_entry.path().is_dir() {
                let id = profile_entry.file_name().to_string_lossy().to_string();
                out.push(format!("{}/{}", platform, id));
            }
        }
    }
    Ok(out)
}

/// Xóa sạch một tài khoản: đóng cửa sổ và xóa toàn bộ thư mục phiên đăng nhập
/// `chat_profiles/<platform>/<profile_id>` (retry vì WebView2 nhả file lock chậm).
#[tauri::command]
async fn delete_chat_profile_completely(
    app: AppHandle,
    state: State<'_, Arc<WorkspaceState>>,
    platform: String,
    profile_id: String,
) -> Result<String, String> {
    {
        let _op_guard = state.window_op_lock.lock().map_err(|e| e.to_string())?;
        let label = make_window_label(&platform, &profile_id);

        if let Ok(mut guard) = state.active_chat_label.lock() {
            if guard.as_deref() == Some(label.as_str()) {
                *guard = None;
            }
        }
        if let Ok(mut lb) = state.last_bounds.lock() {
            *lb = None;
        }
        if let Ok(mut map) = state.created_hwnds.lock() {
            if let Some(raw_hwnd) = map.remove(&label) {
                win32_hide_hwnd(raw_hwnd);
            }
        }
        if let Some(win) = app.get_webview_window(&label) {
            hide_chat_window_completely(&win);
            let _ = win.close();
        }
    }

    if let Ok(app_data_dir) = app.path().app_local_data_dir() {
        let profile_dir = app_data_dir
            .join("chat_profiles")
            .join(&platform)
            .join(&profile_id);

        thread::spawn(move || {
            for _ in 0..8 {
                if !profile_dir.exists() {
                    break;
                }
                if fs::remove_dir_all(&profile_dir).is_ok() {
                    break;
                }
                thread::sleep(Duration::from_millis(350));
            }
        });
    }

    Ok("Đã xóa toàn bộ dữ liệu đăng nhập của tài khoản".to_string())
}

#[tauri::command]
async fn set_workspace_always_on_top(
    app: AppHandle,
    always_on_top: bool,
) -> Result<(), String> {
    if let Some(main_win) = app.get_webview_window("main") {
        let _ = main_win.set_always_on_top(always_on_top);
    }
    Ok(())
}

#[tauri::command]
async fn set_chat_always_on_top(app: AppHandle, always_on_top: bool) -> Result<(), String> {
    set_workspace_always_on_top(app, always_on_top).await
}

/// Đặt số WebView tối đa được giữ chạy (0 = không giới hạn). Đóng ngay các WebView ít dùng nhất.
/// Hiện cửa sổ chính khi React đã render xong (tránh khung trắng lúc khởi động).
#[tauri::command]
async fn show_main_window(
    app: AppHandle,
    state: State<'_, Arc<WorkspaceState>>,
) -> Result<(), String> {
    reveal_main_window(&app, state.inner());
    Ok(())
}

/// Hiện lại cửa sổ chính (từ khay / thông báo) và đặt lại khung chat đang chọn:
/// lúc thu xuống khay mọi cửa sổ chat đã bị ẩn và không có sự kiện Resized nào kéo chúng lên lại.
fn reveal_main_window(app: &AppHandle, state: &Arc<WorkspaceState>) {
    let Some(win) = app.get_webview_window("main") else {
        return;
    };
    let _ = win.show();
    if win.is_minimized().unwrap_or(false) {
        let _ = win.unminimize();
    }
    let _ = win.set_focus();
    if let Ok(mut lb) = state.last_bounds.lock() {
        *lb = None;
    }
    sync_embedded_chat_bounds(app, state);

    // Cửa sổ chính vừa hiện lại có thể chưa ổn định kích thước/tiêu điểm, và WebView2 của khung
    // chat có thể còn giữ bề mặt trống từ lúc cha bị ẩn: đặt lại vài lần và ép vẽ lại.
    let app_clone = app.clone();
    let state_clone = Arc::clone(state);
    std::thread::spawn(move || {
        for delay_ms in [150u64, 450, 1200] {
            std::thread::sleep(Duration::from_millis(delay_ms));
            let visible = app_clone
                .get_webview_window("main")
                .and_then(|w| w.is_visible().ok())
                .unwrap_or(false);
            if !visible {
                return;
            }
            let active = state_clone.active_chat_label.lock().ok().and_then(|g| g.clone());
            if let Some(win) = active.and_then(|l| app_clone.get_webview_window(&l)) {
                repaint_webview(&win);
            }
            if let Ok(mut lb) = state_clone.last_bounds.lock() {
                *lb = None;
            }
            sync_embedded_chat_bounds(&app_clone, &state_clone);
        }
    });
}

/// Ép WebView2 vẽ lại bằng cách tắt/bật `IsVisible` của controller.
#[cfg(target_os = "windows")]
fn repaint_webview(win: &tauri::WebviewWindow) {
    let _ = win.with_webview(|pw| unsafe {
        let controller = pw.controller();
        let _ = controller.SetIsVisible(false);
        let _ = controller.SetIsVisible(true);
    });
}

#[cfg(not(target_os = "windows"))]
fn repaint_webview(_win: &tauri::WebviewWindow) {}

#[tauri::command]
async fn set_sleep_hidden_profiles(
    app: AppHandle,
    state: State<'_, Arc<WorkspaceState>>,
    enabled: bool,
) -> Result<(), String> {
    if let Ok(mut guard) = state.sleep_hidden_profiles.lock() {
        *guard = enabled;
    }
    let active = state.active_chat_label.lock().ok().and_then(|g| g.clone());
    for (label, win) in app.webview_windows() {
        if label.starts_with("chat_") && Some(&label) != active.as_ref() {
            set_webview_power(&win, state.should_sleep_hidden(&label), true);
        }
    }
    Ok(())
}

#[tauri::command]
async fn set_max_live_webviews(
    app: AppHandle,
    state: State<'_, Arc<WorkspaceState>>,
    max_live: usize,
) -> Result<(), String> {
    if let Ok(mut guard) = state.max_live_webviews.lock() {
        *guard = max_live.min(20);
    }
    evict_idle_webviews(&app, state.inner());
    Ok(())
}

/// Bật/tắt thông báo cho một tài khoản: lưu vào state và cập nhật ngay script đang chạy trong WebView.
#[tauri::command]
async fn set_profile_notifications_enabled(
    app: AppHandle,
    state: State<'_, Arc<WorkspaceState>>,
    platform: String,
    profile_id: String,
    enabled: bool,
) -> Result<(), String> {
    if let Ok(mut map) = state.profile_notify_enabled.lock() {
        map.insert(profile_id.clone(), enabled);
    }
    let label = make_window_label(&platform, &profile_id);
    if let Some(win) = app.get_webview_window(&label) {
        let _ = win.eval(&format!(
            "window.__BCH_SET_NOTIFY_ENABLED__ && window.__BCH_SET_NOTIFY_ENABLED__({});",
            enabled
        ));
        let active = state.active_chat_label.lock().ok().and_then(|g| g.clone());
        if active.as_deref() != Some(label.as_str()) {
            set_webview_power(&win, state.should_sleep_hidden(&label), true);
        }
    }
    Ok(())
}

#[tauri::command]
async fn open_app_data_folder(app: AppHandle) -> Result<String, String> {
    if let Ok(dir) = app.path().app_local_data_dir() {
        let _ = fs::create_dir_all(&dir);
        #[cfg(target_os = "windows")]
        {
            let _ = std::process::Command::new("explorer").arg(&dir).spawn();
        }
        Ok(dir.to_string_lossy().to_string())
    } else {
        Err("Không tìm thấy thư mục dữ liệu ứng dụng".to_string())
    }
}

/// Mở URL bằng trình duyệt hệ thống (dự phòng khi một website chặn nhúng / cần đăng nhập khác).
#[tauri::command]
async fn open_system_browser(url: String) -> Result<(), String> {
    #[cfg(target_os = "windows")]
    {
        // Dùng rundll32 thay vì `cmd /c start` vì cmd.exe cắt URL tại dấu `&`
        let spawned = std::process::Command::new("rundll32.exe")
            .args(["url.dll,FileProtocolHandler", &url])
            .spawn();

        if spawned.is_err() {
            let escaped = url.replace('\'', "''");
            let _ = std::process::Command::new("powershell")
                .args([
                    "-NoProfile",
                    "-WindowStyle",
                    "Hidden",
                    "-Command",
                    &format!("Start-Process '{}'", escaped),
                ])
                .spawn();
        }
    }
    Ok(())
}

/// Thoát ứng dụng sau khi đóng toàn bộ WebView con (giải phóng tiến trình WebView2 sạch sẽ).
#[tauri::command]
async fn confirm_and_exit_app(
    app: AppHandle,
    state: State<'_, Arc<WorkspaceState>>,
) -> Result<(), String> {
    if let Ok(mut guard) = state.allow_exit.lock() {
        *guard = true;
    }
    // Ẩn cửa sổ trước khi đóng WebView con để không lộ khung trắng trong lúc teardown.
    if let Some(main) = app.get_webview_window("main") {
        let _ = main.hide();
    }
    hide_all_chat_windows_everywhere(&app, state.inner());
    for (label, win) in app.webview_windows() {
        if label != "main" {
            let _ = win.close();
        }
    }
    // Chờ WebView2 ghi cookie/localStorage xuống đĩa rồi mới thoát.
    // Thoát ngay lập tức có thể làm mất phiên đăng nhập (đặc biệt Messenger).
    let app_for_exit = app.clone();
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(1200));
        app_for_exit.exit(0);
    });
    Ok(())
}

#[derive(serde::Deserialize)]
pub struct TrayProfileInput {
    pub id: String,
    pub name: String,
    pub platform_name: String,
}

/// Cập nhật chế độ "thu xuống khay khi đóng".
#[tauri::command]
async fn set_minimize_to_tray(
    state: State<'_, Arc<WorkspaceState>>,
    enabled: bool,
) -> Result<(), String> {
    if let Ok(mut g) = state.minimize_to_tray.lock() {
        *g = enabled;
    }
    Ok(())
}

/// Cập nhật danh sách profile trong menu khay (gọi từ frontend khi accounts thay đổi).
#[tauri::command]
async fn update_tray_profiles(
    app: AppHandle,
    state: State<'_, Arc<WorkspaceState>>,
    profiles: Vec<TrayProfileInput>,
) -> Result<(), String> {
    let list: Vec<(String, String, String)> =
        profiles.into_iter().map(|p| (p.id, p.name, p.platform_name)).collect();
    if let Ok(mut g) = state.tray_profiles.lock() {
        *g = list;
    }
    rebuild_tray_menu(&app, &state);
    Ok(())
}

/// Ẩn toàn bộ cửa sổ xuống khay (gọi khi bấm đóng và chế độ tray bật).
#[tauri::command]
async fn hide_to_tray(
    app: AppHandle,
    state: State<'_, Arc<WorkspaceState>>,
) -> Result<(), String> {
    hide_all_chat_windows_everywhere(&app, state.inner());
    if let Some(main) = app.get_webview_window("main") {
        let _ = main.hide();
    }
    // Balloon thông báo (chỉ Windows) qua Shell_NotifyIcon
    #[cfg(target_os = "windows")]
    {
        show_tray_balloon(&app, "Henry Chat Pro", "Ứng dụng đang chạy trong khay hệ thống.");
    }
    Ok(())
}

/// Cập nhật chế độ riêng tư (chặn đã đọc/đang gõ) cho tất cả WebView đang chạy.
#[tauri::command]
async fn set_privacy_mode(
    app: AppHandle,
    state: State<'_, Arc<WorkspaceState>>,
    block_read_receipts: bool,
    block_typing: bool,
) -> Result<(), String> {
    if let Ok(mut g) = state.privacy_mode.lock() {
        *g = (block_read_receipts, block_typing);
    }
    let js = format!(
        "window.__BCH_PRIVACY__ = {{ enabled: {}, read: {}, typing: {} }};",
        block_read_receipts || block_typing,
        block_read_receipts,
        block_typing
    );
    for (label, win) in app.webview_windows() {
        if label.starts_with("chat_") {
            let _ = win.eval(&js);
        }
    }
    Ok(())
}

fn rebuild_tray_menu(app: &AppHandle, state: &Arc<WorkspaceState>) {
    let tray = match app.tray_by_id("main-tray") {
        Some(t) => t,
        None => return,
    };
    let profiles = state.tray_profiles.lock().ok().map(|g| g.clone()).unwrap_or_default();

    let mut menu = tauri::menu::MenuBuilder::new(app);
    let open_item = tauri::menu::MenuItemBuilder::with_id("open", "Mở Henry Chat Pro").build(app);
    if let Ok(i) = open_item { let _ = menu = menu.item(&i); }

    // Submenu chọn tài khoản
    if !profiles.is_empty() {
        let mut sub = tauri::menu::SubmenuBuilder::new(app, "Chọn tài khoản");
        for (id, name, plat) in profiles.iter() {
            let label = format!("{} — {}", name, plat);
            if let Ok(i) = tauri::menu::MenuItemBuilder::with_id(format!("profile:{}", id), label).build(app) {
                let _ = sub = sub.item(&i);
            }
        }
        if let Ok(s) = sub.build() {
            let _ = menu = menu.item(&s);
        }
    }

    if let Ok(i) = tauri::menu::MenuItemBuilder::with_id("notifications", "Trung tâm thông báo").build(app) {
        let _ = menu = menu.item(&i);
    }
    if let Ok(sep) = tauri::menu::PredefinedMenuItem::separator(app) {
        let _ = menu = menu.item(&sep);
    }
    if let Ok(i) = tauri::menu::MenuItemBuilder::with_id("quit", "Thoát").build(app) {
        let _ = menu = menu.item(&i);
    }

    if let Ok(m) = menu.build() {
        let _ = tray.set_menu(Some(m));
    }
}

#[cfg(target_os = "windows")]
fn show_tray_balloon(app: &AppHandle, title: &str, msg: &str) {
    // Dùng Shell_NotifyIconW để hiện balloon. Đơn giản hoá: gọi qua windows crate.
    use windows::Win32::UI::Shell::{Shell_NotifyIconW, NOTIFYICONDATAW, NIM_MODIFY, NIF_INFO};
    use windows::Win32::Foundation::HWND;
    // Cần hwnd của cửa sổ ẩn; dùng main window hwnd nếu có.
    let hwnd = app.get_webview_window("main").and_then(|w| w.hwnd().ok()).map(|h| h.0 as isize).unwrap_or(0);
    let mut nid: NOTIFYICONDATAW = unsafe { std::mem::zeroed() };
    nid.cbSize = std::mem::size_of::<NOTIFYICONDATAW>() as u32;
    nid.hWnd = HWND(hwnd as _);
    nid.uID = 1;
    nid.uFlags = NIF_INFO;
    // szInfoTitle / szInfo
    let mut t: [u16; 64] = [0; 64];
    for (i, c) in title.encode_utf16().take(63).enumerate() { t[i] = c; }
    nid.szInfoTitle = t;
    let mut m: [u16; 256] = [0; 256];
    for (i, c) in msg.encode_utf16().take(255).enumerate() { m[i] = c; }
    nid.szInfo = m;
    use windows::Win32::UI::Shell::NIM_ADD;
    unsafe {
        let _ = Shell_NotifyIconW(NIM_ADD, &nid);
        let _ = Shell_NotifyIconW(NIM_MODIFY, &nid);
    }
}

/// Mở (hoặc tái sử dụng) cửa sổ nhỏ chỉ hiển thị trung tâm thông báo.
fn show_notification_window(app: &AppHandle) -> Result<(), String> {
    if let Some(win) = app.get_webview_window("notif") {
        let _ = win.show();
        let _ = win.set_focus();
        return Ok(());
    }
    let width = 430.0_f64;
    let height = 620.0_f64;
    let mut builder = WebviewWindowBuilder::new(app, "notif", WebviewUrl::App("index.html".into()))
        .title("Trung tâm thông báo")
        .inner_size(width, height)
        .min_inner_size(360.0, 420.0)
        .resizable(true)
        .decorations(false)
        .always_on_top(true)
        .skip_taskbar(true)
        .focused(true)
        .visible(false);
    #[cfg(target_os = "windows")]
    {
        if let Ok(Some(mon)) = app.primary_monitor() {
            let sf = mon.scale_factor();
            let msize = mon.size();
            let mpos = mon.position();
            let w = (width * sf).round() as i32;
            let h = (height * sf).round() as i32;
            let x = mpos.x + msize.width as i32 - w - 12;
            let y = mpos.y + msize.height as i32 - h - 72;
            builder = builder.position(x as f64 / sf, y as f64 / sf);
        }
    }
    let win = builder.build().map_err(|e| e.to_string())?;
    let _ = win.show();
    let _ = win.set_focus();
    Ok(())
}

#[tauri::command]
async fn open_notification_window(app: AppHandle) -> Result<(), String> {
    show_notification_window(&app)
}

/// Đưa focus vào khung chat đang hiển thị để thao tác được với hộp thoại cuộc gọi.
#[tauri::command]
async fn focus_chat_window(
    app: AppHandle,
    state: State<'_, Arc<WorkspaceState>>,
) -> Result<(), String> {
    let label = state.active_chat_label.lock().ok().and_then(|g| g.clone());
    let raw_hwnd = label
        .and_then(|l| state.created_hwnds.lock().ok().and_then(|m| m.get(&l).copied()))
        .unwrap_or(0);
    if raw_hwnd == 0 {
        return Ok(());
    }
    let _ = app.run_on_main_thread(move || {
        #[cfg(target_os = "windows")]
        win32_focus_hwnd(raw_hwnd);
        #[cfg(not(target_os = "windows"))]
        let _ = raw_hwnd;
    });
    Ok(())
}

#[tauri::command]
async fn hide_notification_window(app: AppHandle) -> Result<(), String> {
    if let Some(win) = app.get_webview_window("notif") {
        let _ = win.hide();
    }
    Ok(())
}

/// Từ cửa sổ thông báo: mở cửa sổ chính và chọn đúng profile.
#[tauri::command]
async fn open_profile_from_notification(
    app: AppHandle,
    state: State<'_, Arc<WorkspaceState>>,
    profile_id: String,
) -> Result<(), String> {
    reveal_main_window(&app, state.inner());
    let _ = app.emit_to("main", "bch-tray-select-profile", profile_id);
    if let Some(n) = app.get_webview_window("notif") {
        let _ = n.hide();
    }
    Ok(())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let workspace = Arc::new(WorkspaceState::new());

    let state_for_setup = Arc::clone(&workspace);
    let state_for_event = Arc::clone(&workspace);

    let lan_state = Arc::new(lan::LanState::new());

    tauri::Builder::default()
        .manage(Arc::clone(&workspace))
        .manage(Arc::clone(&lan_state))
        .setup(move |app| {
            if let Ok(mut guard) = state_for_setup.app_handle.lock() {
                *guard = Some(app.handle().clone());
            }

            // Tạo icon khay hệ thống
            let tray_icon = app.default_window_icon().cloned();
            let mut tray_builder = tauri::tray::TrayIconBuilder::with_id("main-tray")
                .tooltip("Henry Chat Pro");
            if let Some(icon) = tray_icon {
                tray_builder = tray_builder.icon(icon);
            }
            let state_for_tray = Arc::clone(&state_for_setup);
            let state_for_menu = Arc::clone(&state_for_setup);
            let app_for_tray = app.handle().clone();
            let tray = tray_builder
                .show_menu_on_left_click(false)
                .on_tray_icon_event(|tray, event| {
                    if let tauri::tray::TrayIconEvent::Click {
                        button: tauri::tray::MouseButton::Left,
                        button_state: tauri::tray::MouseButtonState::Up,
                        ..
                    } = event
                    {
                        let _ = show_notification_window(tray.app_handle());
                    }
                })
                .on_menu_event(move |app, event| {
                    let id = event.id.as_ref();
                    match id {
                        "open" => reveal_main_window(app, &state_for_menu),
                        "notifications" => {
                            let _ = show_notification_window(app);
                        }
                        "quit" => {
                            if let Some(main) = app.get_webview_window("main") {
                                let _ = main.hide();
                            }
                            for (label, win) in app.webview_windows() {
                                if label != "main" {
                                    let _ = win.close();
                                }
                            }
                            let app_for_exit = app.clone();
                            std::thread::spawn(move || {
                                std::thread::sleep(Duration::from_millis(1200));
                                app_for_exit.exit(0);
                            });
                        }
                        other if other.starts_with("profile:") => {
                            let pid = other.trim_start_matches("profile:");
                            let _ = app.emit_to("main", "bch-tray-select-profile", pid.to_string());
                            reveal_main_window(app, &state_for_menu);
                        }
                        _ => {}
                    }
                })
                .build(app);
            if tray.is_ok() {
                rebuild_tray_menu(&app_for_tray, &state_for_tray);
            }

            // Dọn WebView ít dùng định kỳ (phòng khi giới hạn bị hạ xuống hoặc app chạy lâu)
            let evict_state = Arc::clone(&state_for_setup);
            let evict_app = app.handle().clone();
            std::thread::spawn(move || loop {
                std::thread::sleep(Duration::from_secs(60));
                evict_idle_webviews(&evict_app, &evict_state);
            });

            if let Some(main_win) = app.get_webview_window("main") {
                #[cfg(target_os = "windows")]
                if let Ok(hwnd) = main_win.hwnd() {
                    win32_allow_drag_drop_messages(hwnd.0 as isize);
                }
                let _ = main_win.set_size(Size::Logical(LogicalSize {
                    width: 1280.0,
                    height: 800.0,
                }));
                let _ = main_win.center();

                // Cửa sổ tạo với visible=false: chỉ hiện khi giao diện đã vẽ xong
                // (tránh khung trắng của WebView2 lúc khởi động). Nếu frontend không gọi
                // kịp thì vẫn tự hiện sau 4 giây để không bị "mất cửa sổ".
                let fallback_win = main_win.clone();
                std::thread::spawn(move || {
                    std::thread::sleep(Duration::from_secs(4));
                    if !fallback_win.is_visible().unwrap_or(true) {
                        let _ = fallback_win.show();
                        let _ = fallback_win.set_focus();
                    }
                });
            }
            Ok(())
        })
        .on_window_event(move |window, event| {
            if window.label() == "main" {
                match event {
                    WindowEvent::Moved(_)
                    | WindowEvent::Resized(_)
                    | WindowEvent::ScaleFactorChanged { .. } => {
                        sync_embedded_chat_bounds(window.app_handle(), &state_for_event);
                    }
                    WindowEvent::CloseRequested { api, .. } => {
                        let minimize = state_for_event
                            .minimize_to_tray
                            .lock()
                            .map(|g| *g)
                            .unwrap_or(true);
                        if minimize {
                            // Thu xuống khay thay vì thoát
                            api.prevent_close();
                            hide_all_chat_windows_everywhere(
                                window.app_handle(),
                                &state_for_event,
                            );
                            let _ = window.hide();
                            #[cfg(target_os = "windows")]
                            {
                                show_tray_balloon(
                                    &window.app_handle(),
                                    "Henry Chat Pro",
                                    "Ứng dụng đang chạy trong khay hệ thống. Chuột phải vào icon khay để chọn profile.",
                                );
                            }
                        } else {
                            hide_all_chat_windows_everywhere(
                                window.app_handle(),
                                &state_for_event,
                            );
                            let _ = window.hide();
                        }
                    }
                    _ => {}
                }
            }
        })
        .invoke_handler(tauri::generate_handler![
            set_sidebar_width,
            set_chat_insets,
            switch_active_chat_account,
            open_chat_window,
            toggle_chat_window,
            set_modal_overlay_mode,
            reload_chat_account,
            close_chat_window,
            delete_chat_profile_completely,
            set_workspace_always_on_top,
            set_chat_always_on_top,
            show_main_window,
            set_max_live_webviews,
            set_sleep_hidden_profiles,
            set_profile_notifications_enabled,
            open_app_data_folder,
            list_profile_dirs,
            send_chat_message,
            test_profile_proxy,
            bch_bridge_ping,
            set_minimize_to_tray,
            update_tray_profiles,
            hide_to_tray,
            set_privacy_mode,
            open_notification_window,
            open_profile_from_notification,
            hide_notification_window,
            focus_chat_window,
            lan::lan_list_peers,
            lan::lan_start,
            lan::lan_set_pair_code,
            lan::lan_get_device_name,
            lan::lan_set_device_name,
            lan::lan_send_text,
            lan::lan_send_file,
            save_workspace_snapshot,
            load_workspace_snapshot,
            open_system_browser,
            confirm_and_exit_app
        ])
        .run(tauri::generate_context!())
        .expect("Lỗi chạy ứng dụng Tauri");
}

/// Script chặn tín hiệu "đã đọc" và "đang gõ" cho từng nền tảng.
fn build_privacy_script(platform: &str, block_read: bool, block_typing: bool) -> String {
    let enabled = block_read || block_typing;
    format!(
        r#"
        (function() {{
            if (window.__BCH_PRIVACY_SCRIPT__) return;
            window.__BCH_PRIVACY_SCRIPT__ = true;
            window.__BCH_PRIVACY__ = {{ enabled: {enabled}, read: {block_read}, typing: {block_typing} }};

            function isBlocked(url, body) {{
                if (!window.__BCH_PRIVACY__ || !window.__BCH_PRIVACY__.enabled) return false;
                const u = String(url || '').toLowerCase();
                const b = String(body || '').toLowerCase();
                const plat = '{platform}';
                // Quy tắc theo nền tảng (khớp URL/body).
                // Không dùng chuỗi quá ngắn như 'read'/'seen': chúng khớp cả 'thread', 'already'…
                // và sẽ chặn nhầm cả request cuộc gọi. Chỉ khớp đúng API đánh dấu đã đọc.
                const readKeys = ['markread', 'mark_read', 'readreceipt', 'read_receipt', 'readhistory', 'seenmessages'];
                const typingKeys = ['typing', 'settyping', 'set_typing', 'istyping'];
                let blocked = false;
                if (window.__BCH_PRIVACY__.read) {{
                    for (const k of readKeys) {{
                        if (u.includes(k) || b.includes(k)) {{ blocked = true; break; }}
                    }}
                }}
                if (!blocked && window.__BCH_PRIVACY__.typing) {{
                    for (const k of typingKeys) {{
                        if (u.includes(k) || b.includes(k)) {{ blocked = true; break; }}
                    }}
                }}
                // Quy tắc riêng Messenger (GraphQL operationName).
                if (plat === 'messenger') {{
                    if (window.__BCH_PRIVACY__.read && (b.includes('markthreadasread') || b.includes('mark_thread_as_read'))) blocked = true;
                    if (window.__BCH_PRIVACY__.typing && (b.includes('settypingindicator') || b.includes('set_typing_indicator'))) blocked = true;
                }}
                // Quy tắc riêng Zalo.
                if (plat === 'zalo') {{
                    if (window.__BCH_PRIVACY__.read && (u.includes('/seen') || u.includes('zalo.me/api/seen'))) blocked = true;
                    if (window.__BCH_PRIVACY__.typing && (u.includes('typing'))) blocked = true;
                }}
                if (blocked) {{
                    try {{ console.log('[BCH privacy] blocked', url); }} catch (e) {{}}
                }}
                return blocked;
            }}

            // fetch
            try {{
                const origFetch = window.fetch;
                window.fetch = function(input, init) {{
                    try {{
                        const url = (typeof input === 'string') ? input : (input && input.url) ? input.url : '';
                        const body = init && init.body ? init.body : '';
                        if (isBlocked(url, body)) {{
                            return Promise.resolve(new Response('{{}}', {{ status: 200, headers: {{ 'Content-Type': 'application/json' }} }}));
                        }}
                    }} catch (e) {{}}
                    return origFetch.apply(this, arguments);
                }};
            }} catch (e) {{}}

            // XMLHttpRequest
            try {{
                const origOpen = XMLHttpRequest.prototype.open;
                const origSend = XMLHttpRequest.prototype.send;
                XMLHttpRequest.prototype.open = function(method, url) {{
                    this.__bch_url = url;
                    return origOpen.apply(this, arguments);
                }};
                XMLHttpRequest.prototype.send = function(body) {{
                    try {{
                        if (isBlocked(this.__bch_url, body)) {{
                            // Giả lập thành công
                            try {{
                                Object.defineProperty(this, 'status', {{ value: 200, writable: true }});
                            }} catch (e) {{}}
                            return;
                        }}
                    }} catch (e) {{}}
                    return origSend.apply(this, arguments);
                }};
            }} catch (e) {{}}

            // sendBeacon
            try {{
                const origBeacon = navigator.sendBeacon;
                navigator.sendBeacon = function(url, data) {{
                    try {{
                        if (isBlocked(url, data)) return true;
                    }} catch (e) {{}}
                    return origBeacon.apply(this, arguments);
                }};
            }} catch (e) {{}}

            // WebSocket: chặn frame chứa từ khoá (best-effort, chỉ kiểm tra chuỗi)
            try {{
                const OrigWS = window.WebSocket;
                function WrappedWS(url, protocols) {{
                    const ws = protocols ? new OrigWS(url, protocols) : new OrigWS(url);
                    const origSend = ws.send.bind(ws);
                    ws.send = function(data) {{
                        try {{
                            const s = (typeof data === 'string') ? data : '';
                            if (s && isBlocked(url, s)) return;
                        }} catch (e) {{}}
                        return origSend(data);
                    }};
                    return ws;
                }}
                WrappedWS.prototype = OrigWS.prototype;
                // Giữ nguyên các hằng số tĩnh (WebSocket.OPEN…) — nếu mất, code kiểm tra
                // readyState === WebSocket.OPEN sẽ hỏng và cuộc gọi không kết nối được.
                try {{ Object.setPrototypeOf(WrappedWS, OrigWS); }} catch (e) {{}}
                try {{
                    ['CONNECTING', 'OPEN', 'CLOSING', 'CLOSED'].forEach(function (k) {{
                        WrappedWS[k] = OrigWS[k];
                    }});
                }} catch (e) {{}}
                window.WebSocket = WrappedWS;
            }} catch (e) {{}}
        }})();
        "#,
        enabled = enabled,
        block_read = block_read,
        block_typing = block_typing,
        platform = platform
    )
}
