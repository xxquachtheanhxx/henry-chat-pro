# Phân tích nâng cấp — Henry Chat Pro v3.0.0

> Tài liệu chỉ phân tích, **không thay đổi production code**. Mọi tham chiếu dạng `file:dòng` dựa trên commit `b0cfd24` (nhánh `main`).
> Quy ước: ✅ = đã xác nhận bằng code/lệnh; ⚠️ = suy luận từ code, cần xác minh thực tế trên Windows.

## Mục lục

0. [Tóm tắt nhanh (Top 5)](#0-tóm-tắt-nhanh-top-5)
1. [Kết quả kiểm tra build](#1-kết-quả-kiểm-tra-build)
2. [Bản đồ kiến trúc](#2-bản-đồ-kiến-trúc)
3. [Bug đã xác nhận](#3-bug-đã-xác-nhận)
4. [Tính năng dang dở / stub](#4-tính-năng-dang-dở--stub)
5. [Bảo mật](#5-bảo-mật)
6. [Code health](#6-code-health)
7. [Roadmap P0/P1/P2](#7-roadmap-p0p1p2)
8. [Sprint sửa lỗi đầu tiên (5 hạng mục)](#8-sprint-sửa-lỗi-đầu-tiên-5-hạng-mục)
9. [Kế hoạch cập nhật: Bảo mật · Chạy nhẹ · Mượt](#9-kế-hoạch-cập-nhật-bảo-mật--chạy-nhẹ--mượt)

---

## 0. Tóm tắt nhanh (Top 5)

| # | Phát hiện | Mức độ | Bằng chứng chính |
|---|-----------|--------|------------------|
| 1 | **Privacy mode (chặn "đã xem"/"đang gõ") không hoạt động end-to-end**: frontend không bao giờ gửi cấu hình `privacy` sang Rust, nên script chặn không bao giờ được inject. Khi được inject thì XHR bị chặn sẽ treo vĩnh viễn. | major | `src/App.tsx:196-199`, `src-tauri/src/lib.rs:1368`, `lib.rs:2988-2996` |
| 2 | **Bản sao lưu `workspace_snapshot.json` có thể bị ghi đè/không được dùng** → rủi ro mất danh sách tài khoản khi localStorage bị xoá. | major | `src/App.tsx:463`, `App.tsx:926-1037` |
| 3 | **LAN share mở TCP listener trên mọi interface, không xác thực, tự khởi động** — bất kỳ máy nào trong LAN cũng có thể ghi file (kích thước không giới hạn) vào thư mục Downloads. | critical (security) | `src-tauri/src/lan.rs:105`, `lan.rs:131-188`, `lib.rs:2731` |
| 4 | **Trả lời nhanh từ thông báo gõ vào cuộc trò chuyện đang mở**, không phải cuộc trò chuyện vừa gửi thông báo → có thể **gửi nhầm người**; luôn báo "sent" dù không tìm thấy ô nhập. | major | `lib.rs:2141-2239` |
| 5 | **Secrets lưu plaintext & bề mặt tấn công rộng**: mật khẩu proxy trong localStorage + file snapshot, "kho mật khẩu" bridge lưu JSON thô, relay proxy local không xác thực, `csp: null`, tự cấp quyền mic/camera cho mọi trang. | major | `App.tsx:23-31,1557`, `lib.rs:2118`, `bridge.rs:20-52`, `net.rs:254`, `tauri.conf.json:31`, `lib.rs:1844-1848` |

Ngoài ra: `src/App.tsx` là 1 component duy nhất 3.785 dòng (57 `useState`, 27 `useEffect`), không có test, không lint/CI, `callTauri` nuốt mọi lỗi.

---

## 1. Kết quả kiểm tra build

Môi trường: Ubuntu 22.04 (VM), Node v24 (qua nvm), npm 11, Rust/cargo 1.99.1.

| Bước | Lệnh chính xác | Kết quả |
|------|----------------|---------|
| Cài dependency | `npm install` | ✅ OK. Cảnh báo `2 vulnerabilities (1 moderate, 1 high)` — `esbuild <=0.24.2` qua `vite` (advisory GHSA-67mh-4wv8-2f99, dev server). `npm audit fix --force` sẽ nâng lên Vite 8 (breaking). Lưu ý: `npm install` trên Linux làm thay đổi `package-lock.json` (−56 dòng, optional deps theo nền tảng) — đã revert, không commit. |
| Typecheck | `npx tsc --noEmit -p .` | ✅ exit 0, 0 lỗi. **Nhưng** `tsconfig.json` đặt `"strict": false`. |
| Typecheck chặt hơn | `npx tsc --noEmit -p . --noUnusedLocals --noUnusedParameters` | 2 lỗi: `src/App.tsx(1801,9): TS6133 'toggleAccountGroup' is declared but its value is never read.` và `src/main.tsx(1,1): TS6133 'React' is declared but its value is never read.` (`--strict` riêng lẻ: 0 lỗi.) |
| Build frontend | `npx vite build` | ✅ OK (Vite 5.4.21, 38 modules, JS ~223 KB, CSS ~43 KB). |
| Rust (Linux host) | `cd src-tauri && cargo check` | ✅ exit 0, **10 warnings**: `lib.rs:1585` `raw_hwnd` unused; `lib.rs:2434` `url` unused (→ `open_system_browser` là no-op ngoài Windows); `lib.rs:2640` `mut` thừa; các stub `win32_*` không dùng (`lib.rs:260,308,362`); `bridge.rs:20,30,42,55` không dùng ngoài Windows. |
| Rust (Windows target) | `rustup target add x86_64-pc-windows-gnu` rồi `cargo check --target x86_64-pc-windows-gnu --target-dir target-win` | Lần 1 ❌ exit 101: `tauri-winres ... NotAttempted("x86_64-w64-mingw32-windres")` (thiếu toolchain MinGW trên VM — lỗi môi trường, không phải lỗi code). Sau `sudo apt-get install -y mingw-w64`: ✅ exit 0, **0 warnings**. |
| `cargo build`/`tauri build`, chạy app | — | ❌ Không chạy: app phụ thuộc WebView2 + Win32 (nhúng HWND), VM là Linux nên không thể kiểm tra runtime. Các mục ⚠️ bên dưới cần xác minh trên Windows. |
| Test | — | Không có test nào (không có script `test` trong `package.json`, không có `#[test]` trong Rust). |

---

## 2. Bản đồ kiến trúc

### 2.1 Cấu trúc thư mục

```
henry-chat-pro/
├─ index.html, vite.config.ts (port 1420, strictPort), tsconfig.json (strict:false)
├─ src/
│  ├─ main.tsx            → render <App/>
│  ├─ App.tsx  (3.785 dòng, ~144 KB) → TOÀN BỘ UI + state + gọi Tauri
│  ├─ styles.css (2.584 dòng)
│  └─ assets/logo.png
├─ public/favicon.png
├─ src-tauri/
│  ├─ tauri.conf.json, capabilities/default.json, Cargo.toml, build.rs
│  └─ src/
│     ├─ main.rs   → desktop_bubble_chat_pro_lib::run()
│     ├─ lib.rs    (3.046 dòng) → WebView/profile, Win32 embed, tray, thông báo, privacy, commands
│     ├─ bridge.rs (255)  → WebView2 postMessage bridge + JSON store
│     ├─ lan.rs    (418)  → UDP discovery + TCP text/file
│     └─ net.rs    (534)  → proxy HTTP/SOCKS5, relay có auth, test proxy
├─ scripts/launcher.ps1 (390 dòng), CLEAN_AND_RUN.bat, KHOI_CHAY_UNG_DUNG.vbs
├─ setup.mjs.js → tạo icon placeholder
└─ value/readme.txt (thư mục lạc, không được dùng)
```

### 2.2 Mô hình runtime

- **Cửa sổ `main`** (React, local `index.html`): sidebar/rail tài khoản, nhóm, cài đặt, inbox thông báo, LAN panel.
- **Cửa sổ `notif`** (cùng bundle React, `IS_NOTIF_WINDOW` tại `App.tsx:277-283`): chỉ vẽ trung tâm thông báo; được tạo động ở `lib.rs:2640`.
- **Mỗi tài khoản = 1 `WebviewWindow` riêng** tải URL từ xa (Zalo/Messenger/Telegram/WhatsApp/custom), data dir riêng `app_local_data_dir/chat_profiles/<platform>/<profile_id>` (`lib.rs:1778-1783`), trên Windows được **nhúng làm child HWND** vào cửa sổ main (`lib.rs:362` & các hàm `win32_*`). Do WebView là cửa sổ native nằm trên DOM, mọi modal React phải gọi `set_modal_overlay_mode` để ẩn WebView (`App.tsx:621-630, 1088-1090`).
- **Giới hạn RAM**: `max_live_webviews` + vòng lặp eviction 60s (`lib.rs:1412`, `lib.rs:2803`).

### 2.3 Kênh giao tiếp Frontend ⇄ Rust

| Hướng | Cơ chế | Ví dụ |
|-------|--------|-------|
| React → Rust | `invoke` qua wrapper `callTauri` (`App.tsx:286-297`) | `switch_active_chat_account`, `close_chat_window`, `send_chat_message`, `lan_send_file`, `save_workspace_snapshot`, `open_system_browser`, `confirm_and_exit_app`… (đăng ký tại `lib.rs:2875-2915`) |
| Rust → React | `app.emit_to("main", …)` / `app.emit(…)` + `listen` | `bch-profile-event` (`lib.rs:131`), `bch-tray-select-profile` (`lib.rs:2711, 2786`), `bch-bridge-message` (`bridge.rs:109`), `bch-lan-peers/message/file` (`lan.rs:159,178,248`) |
| Trang web từ xa → Rust | (1) **Title IPC**: script inject đặt `document.title = "__BCH_EVENT__:…"`/`"__BCH_SWITCH_PROFILE__:…"`, Rust bắt sự kiện đổi title (`lib.rs:135-161`). (2) **WebView2 `chrome.webview.postMessage`** → `bridge::handle_message` (chỉ Windows). | thông báo tin mới, cuộc gọi đến, số chưa đọc |
| Rust → Trang web từ xa | `initialization_script` lúc tạo WebView (monitor, privacy, bridge shim) + `win.eval(js)` | `send_chat_message` (`lib.rs:2236`), `set_privacy_mode` |

### 2.4 Luồng chọn tài khoản

`selectedAccountId` đổi → effect `App.tsx:1092-1110` gọi `switch_active_chat_account(platform, url, profileId, sidebarWidth, webConfig)` → Rust (`lib.rs:1732-1971`): nếu chưa có WebView thì tạo (proxy args, inject scripts, auto-allow mic/camera, `on_new_window` → mở trình duyệt hệ thống), nhúng HWND, ẩn WebView cũ, cập nhật `last_used`, có thể evict WebView ít dùng.

### 2.5 Lưu trữ dữ liệu

| Dữ liệu | Nơi lưu | Mã hoá |
|---------|---------|--------|
| Accounts, platforms, groups, settings, notifications, selected, tombstones | `localStorage` (`App.tsx:201-208`) | Không |
| Bản sao accounts+platforms (gồm proxy user/pass) | `app_local_data_dir/workspace_snapshot.json` (`lib.rs:2118-2137`) | Không |
| Cookie/phiên đăng nhập | WebView2 user data dir từng profile | DPAPI của Chromium |
| Bridge store (`storage:set`) | `bridge_store/<platform>/<profile>/store.json` (`bridge.rs:20-52`) | Không |

---

## 3. Bug đã xác nhận

| ID | Mức độ | Bug | Bằng chứng | Triệu chứng người dùng |
|----|--------|-----|------------|------------------------|
| B1 ✅ | **major** | Privacy mode không bao giờ được áp dụng | `webConfigJson` chỉ serialize `{ proxy }` (`App.tsx:196-199`), bỏ qua `acc.web.privacy`. Rust chỉ inject interceptor khi `web_config.privacy` bật (`lib.rs:1368`). Không có UI nào đặt `privacy` per-profile (chỉ có toggle toàn cục `App.tsx:3335-3347`). `set_privacy_mode` (`lib.rs:2542-2563`) chỉ set `window.__BCH_PRIVACY__`, vô tác dụng khi chưa có interceptor; `state.privacy_mode` được ghi nhưng không bao giờ đọc. | Bật "Chặn đã xem / Chặn đang gõ" trong Cài đặt nhưng bên kia vẫn thấy "đã xem"/"đang soạn tin". |
| B2 ✅ | **major** | Interceptor XHR làm treo request | `XMLHttpRequest.prototype.send` override trả về sớm, chỉ set `status=200`, không bao giờ phát `readystatechange`/`load`/`loadend` (`lib.rs:2988-2996`). Từ khoá chặn rộng (`typing`… quanh `lib.rs:2937`). | Khi B1 được sửa: một số thao tác của Zalo/Messenger có thể "đứng" (spinner vô hạn) hoặc chặn nhầm request không liên quan. |
| B3 ✅ | **major** | Khôi phục từ snapshot bị bỏ qua và snapshot có thể bị ghi đè | Khi localStorage trống, `accounts` được seed bằng `DEFAULT_ACCOUNTS` demo (`App.tsx:463`), nên điều kiện khôi phục `baseAccounts.length === 0` (`App.tsx:975`) không bao giờ đúng. Đồng thời effect `save_workspace_snapshot` (`App.tsx:1032-1037`) chạy ngay lần mount với danh sách demo/rỗng, song song với `load_workspace_snapshot` (`App.tsx:930-933`) → race. | Sau khi WebView2/localStorage bị reset, người dùng thấy 4 tài khoản demo thay vì danh sách thật; bản sao lưu trên đĩa có thể bị ghi đè bằng danh sách demo (mất tên/URL/proxy). Phiên đăng nhập chỉ được dựng lại dạng "Khôi phục · <id>". |
| B4 ✅ | **major** | Trả lời nhanh có thể gửi nhầm cuộc trò chuyện | `send_chat_message` chỉ `eval` script vào WebView của profile (`lib.rs:2217-2239`); script tìm ô nhập của **cuộc trò chuyện đang mở** (`lib.rs:2141-2190`), không điều hướng tới hội thoại đã tạo thông báo. Fallback chọn bất kỳ `contenteditable`/`textarea` hiển thị thấp nhất (`lib.rs:2156-2163`) — có thể là ô tìm kiếm. `catch(e){}` nuốt lỗi; Rust trả `"sent"` vô điều kiện. Platform custom có selector rỗng (`lib.rs:2207`). | Trả lời từ inbox/toast có thể đến sai người; UI báo "đã gửi" dù không gửi được. |
| B5 ✅ | minor | Listener chết `bch-open-notification-center` | `App.tsx:1282` lắng nghe nhưng Rust không bao giờ emit (chỉ có `bch-profile-event`, `bch-tray-select-profile`, `bch-bridge-message`, `bch-lan-*`). | Hành động "mở trung tâm thông báo" từ khay (nếu dự định có) không hoạt động. |
| B6 ⚠️ | major | Tắt "thu nhỏ xuống khay" → đóng cửa sổ nhưng process có thể vẫn chạy ngầm | Nhánh `minimize == false` không gọi `prevent_close`/`app.exit` mà chỉ ẩn (`lib.rs:2863-2869`); các WebView profile và cửa sổ `notif` vẫn tồn tại ẩn nên Tauri không thoát. `allow_exit` được ghi (`lib.rs:2464`) nhưng không bao giờ đọc. | Bấm X → app biến mất nhưng vẫn ngốn RAM, vẫn nhận thông báo; mở lại có thể bị launcher kill cưỡng bức. |
| B7 ⚠️ | minor | Balloon khay tạo icon "ma" | `show_tray_balloon` gọi `Shell_NotifyIconW(NIM_ADD)` với HWND main + `uID=1` và không có icon, mỗi lần đóng cửa sổ (`lib.rs:2606-2629`), không phải icon của `TrayIcon` Tauri; không bao giờ `NIM_DELETE`. | Icon trống/thừa ở khay hệ thống; balloon lúc hiện lúc không. |
| B8 ✅ | minor | `open_system_browser` không làm gì ngoài Windows | Toàn bộ thân hàm nằm trong `#[cfg(target_os = "windows")]`; cargo cảnh báo `url` unused (`lib.rs:2434`). Tương tự embed HWND chỉ có trên Windows (stub rỗng `lib.rs:260,308,362`). Trong khi `tauri.conf.json` bundle `targets: "all"`. | Trên macOS/Linux: nút "Mở bằng trình duyệt" im lặng; WebView không nhúng vào khung. Thực tế app **chỉ chạy được trên Windows**. |
| B9 ✅ | minor | Effect chuyển tài khoản chạy lại thừa | Deps gồm `leftInset` và `flashStatus` (`App.tsx:1110`) → mỗi lần thay đổi độ rộng sidebar/rail sẽ gọi lại `switch_active_chat_account` (đi qua `window_op_lock`, ẩn/hiện WebView). | Giật/nháy khung chat khi kéo sidebar hoặc mở/đóng panel. |
| B10 ✅ | minor | Mở Inbox hoặc LAN panel ẩn luôn khung chat | `anyModalOpen` gồm `showInbox`, `showLan` (`App.tsx:627-628`) → `set_modal_overlay_mode(true)`. | Không thể vừa xem inbox vừa chat. (Có thể là chủ ý do giới hạn native WebView — nên ghi rõ trong UI.) |
| B11 ✅ | minor | Xoá profile báo thành công trước khi xoá xong | `delete_chat_profile_completely` xoá trong thread nền, retry 8 lần rồi im lặng bỏ qua (`lib.rs:2325-2347`); UI báo "Đã xóa … cùng dữ liệu đăng nhập" ngay (`App.tsx:1701-1709`). | Dữ liệu đăng nhập có thể còn trên đĩa khi file bị khoá; người dùng không được biết. |
| B12 ✅ | minor | `icon.icns` giả & script tạo icon thiếu | `src-tauri/icons/icon.icns` chỉ 70 byte (PNG 1×1 do `setup.mjs.js:35` tạo); `setup.mjs.js:4` tham chiếu `scripts/make-icons.ps1` không tồn tại. | Bundle macOS (nếu thử) hỏng icon; hướng dẫn tái tạo icon không dùng được. |
| B13 ✅ | minor | `TS6133`: `toggleAccountGroup` không dùng (`App.tsx:1801`), `React` import thừa (`main.tsx:1`) | `tsc --noUnusedLocals` | Không ảnh hưởng người dùng; dấu hiệu tính năng gán nhóm nhanh bị bỏ dở (xem U4). |

---

## 4. Tính năng dang dở / stub

| ID | Hạng mục | Bằng chứng | Trạng thái |
|----|----------|------------|------------|
| U1 | **Privacy mode** | Xem B1/B2. Kiểu `PrivacyConfig` có (`App.tsx:34-37, 41`) nhưng không có UI per-profile, không được gửi xuống Rust. | Started, không nối end-to-end |
| U2 | **Telegram media viewer fix** bị tắt cứng | `let telegram_media_fix_enabled: bool = false;` (`lib.rs:1375-1381`) → hàm `build_telegram_injection_script` (~`lib.rs:827-1355`, ~530 dòng) là dead code. | Tắt tạm, chưa xác nhận lại |
| U3 | **Bridge "kho mật khẩu (P3) & tiện ích (P4)"** | Comment đầu `bridge.rs:1-14`; `addons:list` luôn trả `[]` (`bridge.rs:104`); store plaintext; UI chỉ có nút "ping bridge" (`App.tsx:1467-1490`). | Nền móng, chưa có tính năng |
| U4 | Gán nhóm nhanh cho tài khoản | `toggleAccountGroup` (`App.tsx:1801-1812`) không được gọi ở đâu. | Dead code |
| U5 | Mở trung tâm thông báo từ khay | Listener `App.tsx:1282` không có emitter (B5). | Nửa vời |
| U6 | Hỗ trợ đa nền tảng | Win32-only embed, `open_system_browser`, tray balloon, RAM trim (`lib.rs:164-…`), WebView2 bridge; `icon.icns` giả; bundle `targets: "all"`. | Chỉ Windows thực sự hoạt động |
| U7 | LAN share | Không pairing/xác nhận nhận file, giới hạn 80 MB chỉ ở phía gửi (`App.tsx:857`), file đi qua IPC dạng base64 (`App.tsx:862-871`), `device_id` sinh lại mỗi lần chạy (`lan.rs:45`), không lịch sử, không tiến trình. | Prototype |
| U8 | Trả lời nhanh | Không điều hướng tới hội thoại đích, không xác nhận đã gửi (B4); custom platform không có selector. | Prototype |
| U9 | Script tạo icon | `scripts/make-icons.ps1` được tham chiếu nhưng không có (B12). | Thiếu file |
| U10 | Thư mục `value/readme.txt` | Không được tham chiếu ở đâu. | Rác repo |

Không tìm thấy comment `TODO`/`FIXME` nào trong `src/`, `src-tauri/src/`, `scripts/` — các phần dang dở chỉ lộ ra qua cờ `false` cứng, stub và listener/hàm không được dùng.

---

## 5. Bảo mật

> Ứng dụng không dùng API key của dịch vụ AI/LLM nào; "secrets" ở đây là **mật khẩu proxy**, **cookie phiên đăng nhập** và dữ liệu "kho mật khẩu" dự kiến.

| ID | Mức độ | Vấn đề | Bằng chứng | Đề xuất |
|----|--------|--------|------------|---------|
| S1 | **critical** | **LAN listener không xác thực, mở trên mọi interface, tự khởi động** | `TcpListener::bind("0.0.0.0:0")` (`lan.rs:105`); `lan::start` gọi ngay trong `setup` dù người dùng chưa mở LAN (`lib.rs:2731`); header JSON đọc bằng `read_line` không giới hạn độ dài (`lan.rs:138`); file ghi thẳng vào Downloads với `size` tự khai báo, không giới hạn (`lan.rs:163-188`); thread mỗi kết nối không giới hạn; trường `from` tự khai báo (giả mạo được); không mã hoá. | Chỉ bật LAN khi người dùng mở panel; pairing bằng mã PIN/khoá chia sẻ; hỏi xác nhận trước khi nhận file; giới hạn kích thước header & file, số kết nối; xoá file dở khi lỗi; cân nhắc TLS/Noise. |
| S2 | major | **Mật khẩu proxy lưu plaintext** | `ProxyConfig.password` trong state (`App.tsx:23-31`), lưu vào `localStorage` (`App.tsx:1557`) và vào `workspace_snapshot.json` (`App.tsx:1034-1036` → `lib.rs:2118-2137`). | Lưu bằng Windows Credential Manager/DPAPI (crate `keyring`), chỉ giữ tham chiếu trong JSON. |
| S3 | major | **Relay proxy local không xác thực** | `TcpListener::bind(("127.0.0.1", 0))` (`net.rs:254`) tự chèn `Proxy-Authorization` cho mọi kết nối; relay không bao giờ dừng khi đổi cấu hình. | Bất kỳ process nào trên máy dùng được proxy trả phí của người dùng. Thêm token bí mật/kiểm tra PID, dọn relay cũ. |
| S4 | major | **`csp: null` + `withGlobalTauri: true`** | `tauri.conf.json:13, 31`. Cửa sổ `main`/`notif` có toàn quyền gọi mọi command custom (không có app ACL cho command riêng; `capabilities/default.json` chỉ khai `core:default`). | Đặt CSP chặt cho bundle local (`default-src 'self'`); tắt `withGlobalTauri` (code đã import `@tauri-apps/api` nên không cần); định nghĩa permission cho từng command. |
| S5 | major | **`open_system_browser` nhận URL tuỳ ý** | `rundll32.exe url.dll,FileProtocolHandler <url>` không kiểm tra scheme (`lib.rs:2434-2455`); fallback PowerShell `Start-Process '<url>'` (`lib.rs:2450`, có escape `'`). `on_new_window` có lọc http/https (`lib.rs:1855-1875`) nhưng command thì không. | `FileProtocolHandler` mở được `file://`, đường dẫn `.exe`, UNC. Chỉ cho phép `http`/`https` (parse bằng `url::Url`), bỏ fallback PowerShell. |
| S6 | major | **Tự cấp quyền mic/camera cho mọi trang** | `PermissionKind::Microphone \| Camera => Allow` (`lib.rs:1844-1848`), áp dụng cho cả platform custom và trang được điều hướng tới trong WebView. | Chỉ auto-allow cho origin của platform đã biết; còn lại hỏi người dùng. |
| S7 | minor | **Title IPC có thể bị giả mạo** | Bất kỳ script nào trong trang chat (quảng cáo, link mở trong WebView) đặt `document.title` dạng `__BCH_EVENT__:` / `__BCH_SWITCH_PROFILE__:` (`lib.rs:135-161`). | Có thể spam thông báo/cuộc gọi giả và chuyển profile. Thêm nonce bí mật sinh mỗi lần tạo WebView, kiểm tra origin. |
| S8 | minor | **Bridge store plaintext, không giới hạn** | `fs::write` JSON thô (`bridge.rs:42-52`), `storage:set` không giới hạn kích thước; được quảng bá là nền cho "kho mật khẩu". | Không được làm password vault trên nền này nếu chưa mã hoá (DPAPI/age) và giới hạn origin. |
| S9 | minor | **Giảm bảo vệ trình duyệt** | `--disable-features=msTrackingPrevention,ThirdPartyStoragePartitioning,BlockThirdParty…` (`lib.rs:1890`). | Cần cho đăng nhập một số dịch vụ, nhưng nên giới hạn theo platform. |
| S10 | minor | **Path traversal tiềm ẩn qua `profile_id`/`platform`** | Label được sanitize (`lib.rs:1453`) nhưng đường dẫn dùng giá trị thô: `join(&platform).join(&profile_id)` (`lib.rs:1780-1783`, `lib.rs:2329-2332`), sau đó `remove_dir_all` trong luồng xoá. ID có thể đến từ snapshot/localStorage bị sửa. | Sanitize/validate `^[A-Za-z0-9_-]+$` ở Rust (giống `bridge.rs:22-27`), kiểm tra đường dẫn nằm trong `chat_profiles`. |
| S11 | minor | **Launcher** | `-ExecutionPolicy Bypass`; `Stop-Process -Force` app đang chạy (`scripts/launcher.ps1:252`) — có thể mất dữ liệu WebView chưa flush. | Gửi yêu cầu thoát êm trước, chỉ kill khi timeout. |
| S12 | minor | **Dependency** | `esbuild <=0.24.2` (qua Vite 5) — chỉ ảnh hưởng dev server. | Nâng Vite ≥6 có kế hoạch. |

**Shell/command access từ frontend**: không có plugin `shell`/`fs`/`process` của Tauri; truy cập shell duy nhất là `rundll32`/`powershell` trong `open_system_browser` (S5) và `on_new_window`. Trang từ xa **không** gọi được `invoke` (không nằm trong capability), nhưng tương tác được qua title IPC (S7) và WebView2 bridge (S8).

---

## 6. Code health

### 6.1 `src/App.tsx` nguyên khối
- 3.785 dòng; 1 component `App()` từ `App.tsx:452` tới cuối file; **57 `useState`, 27 `useEffect`**; types (`App.tsx:14-163`), helpers, hằng số, persistence, event bus, toàn bộ JSX modal/panel/inbox/LAN trong cùng file.
- Hậu quả: mọi thay đổi state re-render toàn bộ cây; dễ sinh bug deps (B9); không thể test đơn vị; effect lồng nhau phụ thuộc thứ tự (B3).
- Đề xuất tách:
  - `types.ts`, `constants.ts`, `lib/storage.ts` (`loadJson`/`saveJson`), `lib/tauri.ts` (`callTauri` + typed commands)
  - hooks: `useAccounts`, `useSettings`, `useNotifications`, `useLan`, `useTauriEvents`, `useActiveWebview`
  - components: `Sidebar`, `Rail`, `AccountEditor`, `SettingsModal`, `InboxPanel`, `LanPanel`, `NotificationWindow`, `GroupManager`
  - Cân nhắc store nhỏ (Zustand) hoặc `useReducer` + context cho accounts/settings.

### 6.2 `src-tauri/src/lib.rs` nguyên khối
- 3.046 dòng trộn: Win32 FFI, state, script JS dạng string khổng lồ (monitor/privacy/telegram/reply), tray, commands. Nên tách `win32.rs`, `scripts/*.js` (dùng `include_str!`), `profiles.rs`, `tray.rs`, `notify.rs`, `privacy.rs`.

### 6.3 Trùng lặp
- Hai bản base64 tự viết: `net.rs:73` và `lan.rs:305` (nên dùng crate `base64`).
- `open_chat_window` và `toggle_chat_window` là wrapper giống hệt (`lib.rs:1975-2017`).
- Logic sanitize tên rải rác (`make_window_label` `lib.rs:1453`, `bridge.rs:22-27`, không áp cho path ở `lib.rs:1780`).
- Nhiều chỗ gọi `set_profile_notifications_enabled` lặp lại (`App.tsx:1562, 1619, 1838`).

### 6.4 Xử lý lỗi
- `callTauri` bắt mọi lỗi, `console.warn`, trả `null` (`App.tsx:286-297`) → UI không phân biệt "không có dữ liệu" và "lỗi", thông điệp lỗi Rust bị mất.
- Rust: rất nhiều `let _ = …` và `.ok()` bỏ qua lỗi (ví dụ `bridge.rs:50`, `lib.rs:2339`); `Mutex::lock().ok()` che poisoning.
- Command `async` dùng `std::sync::Mutex` và I/O blocking (ví dụ `TcpStream::connect` không timeout ở `lan.rs:376, 403`) → có thể chặn runtime.
- Hack thời gian: `setTimeout(focus_chat_window, 350/1200)` (`App.tsx:2212-2213`), `sleep 1200ms` khi thoát.
- `localStorage.setItem` không try/catch (`App.tsx:900`).

### 6.5 Tooling & test
- Không có test (frontend lẫn Rust), không ESLint/Prettier, không `cargo clippy`/`fmt` trong quy trình, không CI.
- `tsconfig` `strict: false`; `package.json` không có script `typecheck`/`lint`/`test`.
- Comment doc đặt sai chỗ (`lib.rs:2372-2373`), tên cũ "BubbleChatPro" còn trong launcher/log (`scripts/launcher.ps1:24-25, 194`), identifier `com.theanh.bubblechathubpro`, lib crate `desktop_bubble_chat_pro_lib`.

---

## 7. Roadmap P0/P1/P2

Ước lượng: **S** ≤ 0,5 ngày · **M** 1–2 ngày · **L** 3–5 ngày · **XL** > 1 tuần (1 dev quen codebase).

### P0 — Sửa ngay (an toàn & mất dữ liệu)

| # | Hạng mục | Liên quan | Effort |
|---|----------|-----------|--------|
| P0-1 | LAN: không tự start; pairing/xác nhận; giới hạn header/file/kết nối; xoá file dở | S1, U7 | M |
| P0-2 | Sửa khôi phục snapshot: không seed demo khi có snapshot, chỉ ghi snapshot sau khi load xong, ghi kèm bản `.bak` | B3 | S–M |
| P0-3 | `open_system_browser`: allowlist `http/https`, bỏ fallback PowerShell | S5 | S |
| P0-4 | Trả lời nhanh: tắt "Gửi" tự động (chỉ "điền"), hoặc điều hướng tới hội thoại đích + xác nhận kết quả từ script | B4 | S (tạm) / L (đúng) |
| P0-5 | Validate `platform`/`profile_id` ở Rust trước mọi thao tác file system | S10 | S |

### P1 — Quan trọng (tính năng hỏng, bảo mật trung bình)

| # | Hạng mục | Liên quan | Effort |
|---|----------|-----------|--------|
| P1-1 | Privacy mode end-to-end: gửi `privacy` trong `webConfigJson`, inject interceptor theo cờ toàn cục, sửa XHR (phát event giả đầy đủ), thu hẹp từ khoá; hoặc ẩn tính năng cho tới khi xong | B1, B2, U1 | M |
| P1-2 | Mật khẩu proxy → Credential Manager (`keyring`); loại khỏi localStorage/snapshot | S2 | M |
| P1-3 | Relay proxy: token xác thực + dọn relay cũ | S3 | M |
| P1-4 | CSP chặt, tắt `withGlobalTauri`, permission cho command custom | S4 | M |
| P1-5 | Mic/camera chỉ auto-allow cho origin platform đã biết | S6 | S |
| P1-6 | Thoát đúng khi tắt minimize-to-tray; sửa tray balloon dùng API Tauri/notification | B6, B7 | S–M |
| P1-7 | `callTauri` trả lỗi có kiểu (`Result`) + toast lỗi; xoá profile trả kết quả thật | 6.4, B11 | M |
| P1-8 | Nonce cho title IPC | S7 | S |

### P2 — Nâng cấp nền tảng

| # | Hạng mục | Liên quan | Effort |
|---|----------|-----------|--------|
| P2-1 | Tách `App.tsx` thành hooks/components (theo 6.1), từng bước, không đổi hành vi | 6.1 | XL |
| P2-2 | Tách `lib.rs`, đưa script JS ra file riêng `include_str!` | 6.2 | L |
| P2-3 | Tooling: ESLint + Prettier, `strict: true`, `cargo clippy/fmt`, GitHub Actions (typecheck, build, `cargo check --target x86_64-pc-windows-msvc` trên `windows-latest`) | 6.5 | M |
| P2-4 | Test: Vitest cho helpers/hooks (storage, restore, normalizeProxy), Rust unit test cho `net.rs`/`lan.rs`/`bridge.rs` | 6.5 | L |
| P2-5 | Quyết định Telegram media fix: xác nhận & bật, hoặc xoá ~530 dòng dead code | U2 | S–M |
| P2-6 | Bridge/password vault: thiết kế lại có mã hoá trước khi phát triển P3/P4 | U3, S8 | XL |
| P2-7 | Phạm vi nền tảng: giới hạn bundle `targets` về `nsis`/`msi` cho tới khi hỗ trợ macOS/Linux thật; dọn `icon.icns`, `setup.mjs.js`, `value/` | B8, B12, U6, U9, U10 | S |
| P2-8 | Nâng Vite ≥6 (esbuild advisory), dọn dependency | S12 | S–M |
| P2-9 | Tối ưu effect chuyển tài khoản (bỏ `leftInset` khỏi trigger, dùng `set_layout_insets` riêng) | B9 | S |
| P2-10 | Đổi tên còn sót ("BubbleChatPro", identifier) — cần migration data dir | 6.5 | M |

---

## 8. Sprint sửa lỗi đầu tiên (5 hạng mục)

Mục tiêu: chặn rủi ro bảo mật lớn nhất và mất dữ liệu, đồng thời đặt lưới an toàn trước khi refactor. Tổng ~5–7 ngày công.

1. **P0-1 — Khoá LAN share** (M): start theo yêu cầu, pairing PIN, xác nhận nhận file, giới hạn kích thước/kết nối.
2. **P0-2 — Sửa khôi phục snapshot** (S–M): bỏ seed demo khi đã có snapshot/profile dirs, chặn ghi snapshot trước khi restore xong, giữ bản `.bak`.
3. **P0-3 + P0-5 — Hardening command** (S): allowlist scheme cho `open_system_browser`, validate `platform`/`profile_id` ở Rust.
4. **P0-4 — Trả lời nhanh an toàn** (S): tạm thời chỉ "điền" vào ô soạn thảo + focus đúng profile, không tự nhấn Enter; script trả kết quả thật về Rust.
5. **P1-1 (rút gọn) + P2-3 (tối thiểu) — Privacy mode & lưới an toàn** (M): hoặc sửa privacy end-to-end, hoặc ẩn toggle cho tới khi sửa; thêm script `typecheck`/`lint` và workflow CI chạy `tsc`, `vite build`, `cargo check` trên Windows runner.

Sau sprint 1: tiến hành P1-2/P1-3/P1-4 (secrets & CSP), rồi bắt đầu tách `App.tsx` (P2-1) dưới bảo vệ của CI + test.

---

## 9. Kế hoạch cập nhật: Bảo mật · Chạy nhẹ · Mượt

> Bổ sung theo yêu cầu của chủ dự án: ưu tiên **bảo mật**, **độ mượt** và **chạy nhẹ**. Mục này **thay thế mục 8** làm thứ tự thực hiện khuyến nghị; mục 7 vẫn là danh sách đầy đủ.
> Các con số hiệu năng dưới đây là **ước lượng từ code**, chưa đo trên Windows (VM phân tích là Linux, không chạy được WebView2).

### 9.1 Vì sao app nặng / giật (bằng chứng trong code)

| # | Nguyên nhân | Bằng chứng | Ảnh hưởng |
|---|-------------|------------|-----------|
| H1 | Mỗi tài khoản = 1 WebView2 với user data dir riêng → mỗi tài khoản là 1 nhóm process Edge riêng; mặc định giữ 3 WebView sống | `lib.rs:1778-1783`, `lib.rs:111` (`max_live_webviews = 3`) | RAM tăng gần tuyến tính theo số tài khoản đang mở |
| H2 | WebView bị ẩn vẫn chạy JS bình thường (không suspend); script giám sát quét DOM mỗi 1,6 s ở **mọi** WebView sống, Telegram quét cả danh sách chat mỗi vòng | `lib.rs:753-814` (`setInterval(pollPassiveStatus, 1600)`) | CPU nền liên tục, tốn pin |
| H3 | `trim_process_ram` gọi `SetProcessWorkingSetSize` cho **process chính** sau mỗi lần evict/tối ưu — chỉ làm số trong Task Manager giảm tạm, sau đó page fault nạp lại | `lib.rs:166-183`, `lib.rs:1450`, `lib.rs:2368` | Giật ngay sau khi "tối ưu RAM" |
| H4 | Eviction WebView dư chạy theo vòng 60 s | `lib.rs:2803-2806` | WebView thừa sống thêm tới 60 s |
| H5 | Effect chuyển tài khoản phụ thuộc `leftInset` → kéo sidebar/mở panel là gọi lại `switch_active_chat_account` (qua `window_op_lock`) | `App.tsx:1092-1110` | Khung chat nháy/giật |
| H6 | `App.tsx` là 1 component (57 `useState`, 27 `useEffect`) → mỗi thông báo/toast/unread re-render toàn bộ UI | `App.tsx:452` trở đi | Giao diện lag khi nhiều tin đến |
| H7 | LAN tự khởi động: UDP broadcast mỗi 3 s + TCP listener + thread, dù không dùng | `lib.rs:2731`, `lan.rs:192-223` | Tốn tài nguyên + rủi ro bảo mật (S1) |
| H8 | Relay proxy không được dọn khi đổi cấu hình | `net.rs:254` | Rò rỉ thread/port |
| H9 | Modal/Inbox/LAN panel phải ẩn native WebView | `App.tsx:621-630` | Nháy trắng khi mở/đóng panel |
| H10 | CSS: `backdrop-filter` (3 chỗ) + animation `infinite` cho cảnh báo cuộc gọi | `styles.css:694, 865, 936, 1011, 1110` | Tải GPU nhẹ trên máy yếu |
| H11 | `Cargo.toml` không có `[profile.release]` tuỳ chỉnh | `src-tauri/Cargo.toml` | Binary lớn hơn cần thiết |

### 9.2 Lộ trình theo giai đoạn

Effort: **S** ≤ 0,5 ngày · **M** 1–2 ngày · **L** 3–5 ngày · **XL** > 1 tuần.

**Giai đoạn 0 — Đo baseline & lưới an toàn (~1 ngày)**

| Việc | Effort |
|------|--------|
| Đo trên Windows: RAM với 3 và 6 tài khoản, CPU idle 5 phút, thời gian chuyển tài khoản, thời gian khởi động (Task Manager / `Get-Process msedgewebview2`) | S |
| CI GitHub Actions trên `windows-latest`: `tsc --noEmit`, `vite build`, `cargo check` | S–M |

**Giai đoạn 1 — Bảo mật P0 (~1 tuần)**

| Việc | Liên quan | Effort |
|------|-----------|--------|
| LAN tắt mặc định, chỉ bật khi mở panel; pairing PIN; xác nhận trước khi nhận file; giới hạn header/file/số kết nối | S1, H7 | M |
| `open_system_browser` chỉ cho `http`/`https`, bỏ fallback PowerShell | S5 | S |
| Validate `platform`/`profile_id` (`^[A-Za-z0-9_-]+$`) ở Rust trước mọi thao tác file | S10 | S |
| Trả lời nhanh: chỉ "điền" vào ô soạn, không tự Enter; script trả kết quả thật | B4 | S |
| Sửa khôi phục snapshot (không seed demo khi có snapshot, chỉ ghi sau khi load xong, giữ `.bak`) | B3 | S–M |

**Giai đoạn 2 — Chạy nhẹ (~1–1,5 tuần)**

| Việc | Liên quan | Lợi ích dự kiến | Effort |
|------|-----------|-----------------|--------|
| Cho WebView bị ẩn "ngủ": `ICoreWebView2_3::TrySuspend` + `ICoreWebView2_19::MemoryUsageTargetLevel = Low`; `Resume` khi chọn lại | H1, H2 | Giảm RAM/CPU lớn nhất | M |
| Tuỳ chọn mỗi tài khoản: "Luôn chạy nền để nhận thông báo" / "Cho ngủ" (WebView ngủ sẽ không phát thông báo) | H1, H2 | Người dùng tự cân bằng RAM ↔ thông báo | S |
| Thay polling 1,6 s bằng `MutationObserver` (title/badge); khi ẩn chỉ quét thưa 5–10 s | H2 | Ít đánh thức CPU | M |
| Bỏ `trim_process_ram`; evict ngay khi chuyển tài khoản thay vì chờ vòng 60 s | H3, H4 | Hết giật sau "tối ưu RAM" | S |
| Lazy start: chỉ tạo WebView của tài khoản đang chọn khi khởi động | H1 | Khởi động nhanh | S |
| Dọn relay proxy cũ khi đổi cấu hình/đóng profile | H8 | Hết rò rỉ thread | S |
| Thêm `[profile.release]`: `lto = true`, `codegen-units = 1`, `strip = true`, `panic = "abort"`, `opt-level = "s"` | H11 | Binary nhỏ hơn | S |

**Giai đoạn 3 — Mượt + sửa tính năng hỏng (~1 tuần)**

| Việc | Liên quan | Effort |
|------|-----------|--------|
| Tách cập nhật vị trí/kích thước khung chat (command riêng, chỉ `SetWindowPos`) khỏi `switch_active_chat_account` | H5, B9 | S–M |
| Đưa notifications/toasts/alerts vào store riêng (Zustand hoặc context tách), `React.memo` cho sidebar/rail/khung chat | H6 | M |
| `callTauri` trả lỗi có kiểu + toast lỗi; xoá profile trả kết quả thật | 6.4, B11 | M |
| Privacy mode: sửa end-to-end (gửi `privacy` trong `webConfigJson`, XHR giả lập đủ event) hoặc ẩn toggle | B1, B2 | M |
| Thoát hẳn khi tắt minimize-to-tray; thay balloon bằng notification của Tauri | B6, B7 | S–M |
| Giảm `backdrop-filter`, hỗ trợ `prefers-reduced-motion`; cân nhắc ảnh chụp WebView (`CapturePreview`) làm nền khi mở modal để hết nháy trắng | H9, H10 | S (CSS) / L (CapturePreview) |

**Giai đoạn 4 — Bảo mật P1 + dọn code (2–3 tuần)**

| Việc | Liên quan | Effort |
|------|-----------|--------|
| Mật khẩu proxy → Windows Credential Manager (crate `keyring`), loại khỏi localStorage/snapshot | S2 | M |
| Relay proxy có token xác thực | S3 | M |
| CSP chặt cho bundle local, tắt `withGlobalTauri`, permission cho từng command | S4 | M |
| Mic/camera chỉ auto-allow cho origin Zalo/Messenger/Telegram/WhatsApp | S6 | S |
| Nonce cho title IPC | S7 | S |
| Tách `App.tsx` thành hooks/components, tách `lib.rs`, script JS ra file `include_str!` | 6.1, 6.2 | XL |
| Test: Vitest (storage, restore, proxy normalize) + Rust unit test (`lan.rs`, `net.rs`, `bridge.rs`) | 6.5 | L |

### 9.3 Sprint 1 khuyến nghị (~1,5 tuần, tối đa 5 hạng mục)

1. **Khoá LAN share** — tắt mặc định, pairing PIN, xác nhận nhận file, giới hạn kích thước (bảo mật + bớt tải nền).
2. **Sửa khôi phục snapshot** — chống mất danh sách tài khoản.
3. **WebView ẩn "ngủ"** — `TrySuspend` + `MemoryUsageTargetLevel=Low`, kèm tuỳ chọn "luôn chạy nền" cho tài khoản cần thông báo; bỏ `trim_process_ram`.
4. **Hết nháy khi chuyển tài khoản / kéo sidebar** — tách command cập nhật bounds khỏi `switch_active_chat_account`.
5. **CI + đo baseline** — chạy `tsc`, `vite build`, `cargo check` trên Windows; ghi số RAM/CPU trước–sau để chứng minh hiệu quả.

### 9.4 Tiêu chí hoàn thành (đề xuất, đo trên Windows)

| Chỉ số | Mục tiêu |
|--------|----------|
| RAM khi mở 6 tài khoản, chỉ 1 hiển thị | Giảm rõ rệt so với baseline (mục tiêu ≥ 30%) |
| CPU idle 5 phút | Gần 0% khi tất cả tài khoản ẩn đang "ngủ" |
| Chuyển tài khoản đã mở | Không nháy trắng, < 200 ms cảm nhận |
| Kéo sidebar / mở panel | Không gọi lại `switch_active_chat_account` |
| Cổng mạng mở khi chưa dùng LAN | Không có |
