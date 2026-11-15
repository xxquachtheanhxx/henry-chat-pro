import React, { useState, useEffect, useMemo, useCallback, useRef } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";
import logoUrl from "./assets/logo.png";

/* ============================================================================
   Henry Chat Pro — Workspace đa tài khoản web
   Mỗi tài khoản là một WebView2 riêng (do lớp Rust quản lý), giữ phiên đăng nhập
   độc lập. React chỉ lo giao diện: danh sách tài khoản, nền tảng, thông báo,
   và các bảng thêm/sửa. Không còn kho dữ liệu Zalo / backup / Google Drive.
   ============================================================================ */

export interface PlatformItem {
  id: string;
  name: string;
  url: string;
  color: string;
  badge: string;
  isBuiltIn?: boolean;
}

export interface ProxyConfig {
  enabled: boolean;
  kind: "http" | "socks5";
  host: string;
  port: number;
  username: string;
  password: string;
  bypass: string;
}

/** Cấu hình web riêng của profile (schema v3). P2 sẽ thêm `fingerprint` vào đây. */
export interface PrivacyConfig {
  blockReadReceipts: boolean;
  blockTyping: boolean;
}

export interface ProfileWebConfig {
  proxy: ProxyConfig;
  privacy?: PrivacyConfig;
}

export interface ProxyTestResult {
  ok: boolean;
  ip: string;
  latency_ms: number;
  via: string;
  message: string;
}

export interface UserGroup {
  id: string;
  name: string;
  color: string;
  createdAt: number;
}

export interface AccountProfile {
  id: string;
  name: string;
  platformId: string;
  /** Thứ tự hiển thị (kéo thả để thay đổi). */
  order?: number;
  customUrl?: string;
  avatarDataUrl?: string;
  pinned?: boolean;
  notificationsEnabled?: boolean;
  /** Nhóm người dùng tự tạo (một tài khoản có thể thuộc nhiều nhóm). */
  groupIds?: string[];
  /** Schema v3: cấu hình mạng/vân tay riêng cho từng profile. */
  web?: ProfileWebConfig;
  createdAt: number;
}

export interface ProfileAlertEvent {
  profile_id: string;
  platform: string;
  kind: string;
  unread_count: number;
  title: string;
  body: string;
  caller: string;
  call_kind: string;
  /** Khoá hội thoại/nhóm nếu nền tảng xác định được. */
  conversation?: string;
  timestamp: number;
}

export interface ProfileAlertState {
  unreadCount: number;
  lastTitle?: string;
  lastBody?: string;
  lastAt?: number;
  hasIncomingCall: boolean;
  caller?: string;
  callKind?: string;
}

export interface NotificationItem {
  id: string;
  profileId: string;
  title: string;
  body?: string;
  isCall?: boolean;
  unread: number;
  at: number;
  /** Khoá hội thoại/nhóm để gộp thông báo cùng một nhóm. */
  conversation?: string;
}

export interface AlertToastItem {
  id: string;
  profileId: string;
  profileName: string;
  platformName: string;
  title: string;
  body: string;
  isCall: boolean;
  timestamp: number;
}

export interface WorkspaceSettings {
  muteSound: boolean;
  alwaysOnTop: boolean;
  /** Thu xuống khay hệ thống khi bấm đóng thay vì thoát */
  minimizeToTray: boolean;
  /** Chặn gửi tín hiệu "đã đọc" */
  privacyBlockReadReceipts: boolean;
  /** Chặn gửi tín hiệu "đang gõ" */
  privacyBlockTyping: boolean;
  /** Số WebView giữ chạy đồng thời (0 = không giới hạn) */
  maxLiveWebviews: number;
  /** Sáng / Tối / Theo hệ thống */
  theme: ThemeMode;
}

export type ThemeMode = "light" | "dark" | "system";

export interface LanPeer {
  id: string;
  name: string;
  ip: string;
  port: number;
}

export interface LanFeedItem {
  id: string;
  from: string;
  to?: string;
  text?: string;
  fileName?: string;
  path?: string;
  size?: number;
  at: number;
  outgoing?: boolean;
}

export type SidebarMode = "full" | "collapsed";

const RAIL_WIDTH = 56;

const SIDEBAR_WIDTH_MAP: Record<SidebarMode, number> = {
  full: 310,
  collapsed: 68,
};

const CUSTOM_URL_OPTION_VALUE = "__custom_url__";

const DEFAULT_PROXY: ProxyConfig = {
  enabled: false,
  kind: "http",
  host: "",
  port: 0,
  username: "",
  password: "",
  bypass: "",
};

function normalizeProxy(input?: Partial<ProxyConfig> | null): ProxyConfig {
  const src = input || {};
  return {
    enabled: !!src.enabled,
    kind: src.kind === "socks5" ? "socks5" : "http",
    host: String(src.host || "").trim(),
    port: Math.max(0, Math.min(65535, Number(src.port) || 0)),
    username: String(src.username || ""),
    password: String(src.password || ""),
    bypass: String(src.bypass || "").trim(),
  };
}

/** Chuỗi JSON gửi xuống Rust để tạo WebView với proxy của profile. */
function webConfigJson(acc: AccountProfile | null): string {
  const proxy = normalizeProxy(acc?.web?.proxy);
  return JSON.stringify({ proxy });
}

const LS_PLATFORMS = "bch_pro_platforms_v1";
const LS_ACCOUNTS = "bch_pro_accounts_v1";
const LS_GROUPS = "bch_pro_groups_v1";
const LS_SIDEBAR = "bch_pro_sidebar_mode_v1";
const LS_SETTINGS = "bch_pro_settings_v1";
const LS_SELECTED = "bch_pro_selected_account_v1";
/** Profile đã bị người dùng xoá hẳn — không tự khôi phục lại từ đĩa. */
const LS_DELETED = "bch_pro_deleted_profiles_v1";
const LS_NOTIF = "bch_pro_notification_log_v1";
const MAX_NOTIFICATIONS = 60;

const DEFAULT_PLATFORMS: PlatformItem[] = [
  {
    id: "zalo",
    name: "Zalo Web",
    url: "https://chat.zalo.me",
    color: "#0068FF",
    badge: "ZL",
    isBuiltIn: true,
  },
  {
    id: "messenger",
    name: "Messenger",
    url: "https://www.messenger.com",
    color: "#8B5CF6",
    badge: "MS",
    isBuiltIn: true,
  },
  {
    id: "telegram",
    name: "Telegram",
    url: "https://web.telegram.org/k/",
    color: "#0284C7",
    badge: "TG",
    isBuiltIn: true,
  },
  {
    id: "whatsapp",
    name: "WhatsApp",
    url: "https://web.whatsapp.com",
    color: "#10B981",
    badge: "WA",
    isBuiltIn: true,
  },
];

const DEFAULT_ACCOUNTS: AccountProfile[] = [
  {
    id: "zalo_cong_viec",
    name: "Zalo Công Việc",
    platformId: "zalo",
    pinned: true,
    createdAt: Date.now() - 120000,
  },
  {
    id: "zalo_ca_nhan",
    name: "Zalo Cá Nhân",
    platformId: "zalo",
    createdAt: Date.now() - 90000,
  },
  {
    id: "messenger_chinh",
    name: "Messenger Chính",
    platformId: "messenger",
    createdAt: Date.now() - 60000,
  },
  {
    id: "tele_main",
    name: "Telegram Chính",
    platformId: "telegram",
    createdAt: Date.now() - 30000,
  },
];

const TAURI_READY = typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;

/** Cửa sổ "notif" chỉ vẽ trung tâm thông báo, không vẽ giao diện chính. */
const IS_NOTIF_WINDOW = (() => {
  try {
    return getCurrentWindow().label === "notif";
  } catch {
    return false;
  }
})();

/** Gọi lệnh Tauri an toàn: khi mở bằng trình duyệt thường (vite preview) sẽ bỏ qua thay vì crash. */
async function callTauri<T>(
  cmd: string,
  args?: Record<string, unknown>
): Promise<T | null> {
  if (!TAURI_READY) return null;
  try {
    return await invoke<T>(cmd, args);
  } catch (err) {
    console.warn(`[bubble-chat] lệnh ${cmd} thất bại:`, err);
    return null;
  }
}

let storageWarning = "";

function loadJson<T>(key: string, fallback: T): T {
  let raw: string | null = null;
  try {
    raw = localStorage.getItem(key);
    if (!raw) return fallback;
    const parsed = JSON.parse(raw) as T;
    return parsed ?? fallback;
  } catch {
    // Dữ liệu hỏng: giữ bản gốc để chẩn đoán thay vì ghi đè bằng dữ liệu mặc định.
    storageWarning =
      "⚠️ Dữ liệu cài đặt bị lỗi định dạng — đã sao lưu bản gốc và khôi phục tài khoản từ đĩa.";
    try {
      if (raw) localStorage.setItem(`${key}__corrupt_backup`, raw);
    } catch {
      /* bỏ qua */
    }
    return fallback;
  }
}

/** Ghi localStorage; nếu vượt hạn mức thì bỏ bớt ảnh đại diện (dữ liệu lớn nhất) rồi thử lại. */
function saveJson(key: string, value: unknown): boolean {
  try {
    localStorage.setItem(key, JSON.stringify(value));
    return true;
  } catch {
    try {
      const slim = JSON.stringify(value, (k, v) => (k === "avatarDataUrl" ? undefined : v));
      localStorage.setItem(key, slim);
      return false;
    } catch {
      return false;
    }
  }
}

function normalizeUrl(raw: string): string {
  const t = (raw || "").trim();
  if (!t) return "";
  if (/^https?:\/\//i.test(t)) return t;
  return `https://${t}`;
}

function makeAccountId(): string {
  return `acc_${Date.now().toString(36)}${Math.random().toString(36).slice(2, 6)}`;
}

function makePlatformId(): string {
  return `plat_${Date.now().toString(36)}${Math.random().toString(36).slice(2, 6)}`;
}

/**
 * Bổ sung nền tảng còn thiếu cho các tài khoản cũ: dữ liệu từ phiên bản trước có thể trỏ tới
 * một platformId không còn trong danh sách nền tảng (nền tảng tự thêm đã bị xóa), khiến tên
 * hiển thị thành id thô và số đếm trên chip không khớp tổng số tài khoản.
 */
function reconcilePlatforms(
  platforms: PlatformItem[],
  accounts: AccountProfile[]
): PlatformItem[] {
  const known = new Set(platforms.map((p) => p.id));
  const extras: PlatformItem[] = [];

  accounts.forEach((acc) => {
    if (!acc.platformId || known.has(acc.platformId)) return;
    known.add(acc.platformId);

    let host = "";
    try {
      host = new URL(normalizeUrl(acc.customUrl || "")).hostname.replace(/^www\./, "");
    } catch {
      host = "";
    }
    const label = host || acc.platformId;
    extras.push({
      id: acc.platformId,
      name: label,
      url: acc.customUrl ? normalizeUrl(acc.customUrl) : "",
      color: "#64748B",
      badge: label.slice(0, 2).toUpperCase(),
    });
  });

  return extras.length > 0 ? [...platforms, ...extras] : platforms;
}

function playAlertTone(isCall: boolean) {
  try {
    const AudioCtx =
      window.AudioContext ||
      (window as unknown as { webkitAudioContext?: typeof AudioContext })
        .webkitAudioContext;
    if (!AudioCtx) return;
    const ctx = new AudioCtx();
    const playBeep = (freq: number, startOffset: number, dur: number, vol: number) => {
      const osc = ctx.createOscillator();
      const gain = ctx.createGain();
      osc.type = isCall ? "triangle" : "sine";
      osc.frequency.setValueAtTime(freq, ctx.currentTime + startOffset);
      gain.gain.setValueAtTime(0.001, ctx.currentTime + startOffset);
      gain.gain.exponentialRampToValueAtTime(vol, ctx.currentTime + startOffset + 0.02);
      gain.gain.exponentialRampToValueAtTime(0.0001, ctx.currentTime + startOffset + dur);
      osc.connect(gain);
      gain.connect(ctx.destination);
      osc.start(ctx.currentTime + startOffset);
      osc.stop(ctx.currentTime + startOffset + dur + 0.02);
    };
    if (isCall) {
      playBeep(784, 0, 0.22, 0.14);
      playBeep(987.77, 0.25, 0.28, 0.16);
      playBeep(1174.66, 0.56, 0.34, 0.14);
    } else {
      playBeep(660, 0, 0.14, 0.09);
      playBeep(880, 0.15, 0.22, 0.08);
    }
    setTimeout(() => {
      ctx.close().catch(() => {});
    }, 1500);
  } catch {}
}

/** Nén ảnh đại diện về 128×128 để nhẹ localStorage. */
function resizeImageToDataUrl(file: File, maxSize = 128): Promise<string> {
  return new Promise((resolve, reject) => {
    const reader = new FileReader();
    reader.onload = () => {
      const img = new Image();
      img.onload = () => {
        const canvas = document.createElement("canvas");
        canvas.width = maxSize;
        canvas.height = maxSize;
        const ctx = canvas.getContext("2d");
        if (!ctx) {
          resolve(String(reader.result || ""));
          return;
        }
        const minSide = Math.min(img.width, img.height);
        const sx = (img.width - minSide) / 2;
        const sy = (img.height - minSide) / 2;
        ctx.clearRect(0, 0, maxSize, maxSize);
        ctx.drawImage(img, sx, sy, minSide, minSide, 0, 0, maxSize, maxSize);
        resolve(canvas.toDataURL("image/png", 0.92));
      };
      img.onerror = () => reject(new Error("Không đọc được định dạng ảnh này"));
      img.src = String(reader.result || "");
    };
    reader.onerror = () => reject(new Error("Lỗi đọc file ảnh"));
    reader.readAsDataURL(file);
  });
}

export default function App() {
  /* ------------------------------- Dữ liệu ------------------------------- */
  const [platforms, setPlatforms] = useState<PlatformItem[]>(() => {
    const saved = loadJson<PlatformItem[]>(LS_PLATFORMS, DEFAULT_PLATFORMS);
    const base = Array.isArray(saved) && saved.length > 0 ? saved : DEFAULT_PLATFORMS;
    return reconcilePlatforms(base, loadJson<AccountProfile[]>(LS_ACCOUNTS, []));
  });

  const [accounts, setAccounts] = useState<AccountProfile[]>(() => {
    // Chỉ seed dữ liệu mẫu khi THẬT SỰ chưa có gì. Nếu dữ liệu đã lưu bị hỏng thì giữ
    // danh sách rỗng (không ghi đè) để bước khôi phục từ đĩa dựng lại tài khoản.
    if (!localStorage.getItem(LS_ACCOUNTS)) return DEFAULT_ACCOUNTS;
    const saved = loadJson<AccountProfile[] | null>(LS_ACCOUNTS, null);
    if (!Array.isArray(saved)) return [];
    return saved.map((a, i) => ({ ...a, order: typeof a.order === "number" ? a.order : i * 10 }));
  });

  const [draggedProfileId, setDraggedProfileId] = useState<string | null>(null);
  const [dragOverProfileId, setDragOverProfileId] = useState<string | null>(null);

  const [groups, setGroups] = useState<UserGroup[]>(() => {
    const saved = loadJson<UserGroup[] | null>(LS_GROUPS, null);
    return Array.isArray(saved) ? saved : [];
  });

  const [sidebarMode, setSidebarMode] = useState<SidebarMode>(() =>
    localStorage.getItem(LS_SIDEBAR) === "collapsed" ? "collapsed" : "full"
  );

  const [selectedAccountId, setSelectedAccountId] = useState<string>(() => {
    const saved = localStorage.getItem(LS_SELECTED);
    if (saved) return saved;
    const first = loadJson<AccountProfile[]>(LS_ACCOUNTS, DEFAULT_ACCOUNTS)[0];
    return first?.id || "";
  });

  type SidebarFilter =
    | { kind: "all" }
    | { kind: "platform"; id: string }
    | { kind: "group"; id: string };
  const [sidebarFilter, setSidebarFilter] = useState<SidebarFilter>({ kind: "all" });
  const [settings, setSettings] = useState<WorkspaceSettings>(() => {
    const saved = loadJson<Partial<WorkspaceSettings>>(LS_SETTINGS, {});
    return {
      muteSound: !!saved.muteSound,
      alwaysOnTop: !!saved.alwaysOnTop,
      minimizeToTray:
        typeof saved.minimizeToTray === "boolean" ? saved.minimizeToTray : true,
      privacyBlockReadReceipts:
        typeof saved.privacyBlockReadReceipts === "boolean"
          ? saved.privacyBlockReadReceipts
          : false,
      privacyBlockTyping:
        typeof saved.privacyBlockTyping === "boolean" ? saved.privacyBlockTyping : false,
      maxLiveWebviews:
        typeof saved.maxLiveWebviews === "number" ? saved.maxLiveWebviews : 3,
      theme: saved.theme === "light" || saved.theme === "dark" ? saved.theme : "system",
    };
  });

  /* ------------------------- Thông báo / cuộc gọi ------------------------- */
  const [profileAlerts, setProfileAlerts] = useState<Record<string, ProfileAlertState>>({});
  const [alertToasts, setAlertToasts] = useState<AlertToastItem[]>([]);

  /* ------------------------ Trung tâm thông báo ------------------------- */
  const [showInbox, setShowInbox] = useState(IS_NOTIF_WINDOW);
  const [showLan, setShowLan] = useState(false);
  const [notifications, setNotifications] = useState<NotificationItem[]>(() =>
    loadJson<NotificationItem[]>(LS_NOTIF, [])
  );
  const [replyTargetId, setReplyTargetId] = useState<string | null>(null);
  const [replyText, setReplyText] = useState("");
  const [replySending, setReplySending] = useState(false);

  /* -------------------------------- Modals -------------------------------- */
  const [showSettings, setShowSettings] = useState(false);
  const [showAddAccount, setShowAddAccount] = useState(false);
  const [showEditAccount, setShowEditAccount] = useState(false);
  const [showAddPlatform, setShowAddPlatform] = useState(false);
  const [deleteTarget, setDeleteTarget] = useState<AccountProfile | null>(null);
  const [showGroupManager, setShowGroupManager] = useState(false);
  const [newGroupName, setNewGroupName] = useState("");
  const [newGroupColor, setNewGroupColor] = useState("#6366F1");
  const [editingGroupId, setEditingGroupId] = useState<string | null>(null);

  /* ------------------------------ Form state ------------------------------ */
  const [newAccName, setNewAccName] = useState("");
  const [newAccPlatformId, setNewAccPlatformId] = useState("zalo");
  const [newAccCustomUrl, setNewAccCustomUrl] = useState("");
  const [newAccNotify, setNewAccNotify] = useState(true);
  const [newAccAvatar, setNewAccAvatar] = useState<string | undefined>(undefined);
  const [newAccGroupIds, setNewAccGroupIds] = useState<string[]>([]);

  const [editingAccount, setEditingAccount] = useState<AccountProfile | null>(null);
  const [editName, setEditName] = useState("");
  const [editCustomUrl, setEditCustomUrl] = useState("");
  const [editGroupIds, setEditGroupIds] = useState<string[]>([]);
  const [editNotify, setEditNotify] = useState(true);
  const [editAvatar, setEditAvatar] = useState<string | undefined>(undefined);
  const [editTab, setEditTab] = useState<"general" | "network">("general");
  const [editProxy, setEditProxy] = useState<ProxyConfig>(DEFAULT_PROXY);
  const [proxyTesting, setProxyTesting] = useState(false);
  const [proxyResult, setProxyResult] = useState<ProxyTestResult | null>(null);
  const [bridgeStatus, setBridgeStatus] = useState("");
  /** Tăng lên để buộc tạo lại WebView (áp dụng proxy mới). */
  const [webConfigVersion, setWebConfigVersion] = useState(0);

  const [newPlatName, setNewPlatName] = useState("");
  const [newPlatUrl, setNewPlatUrl] = useState("");
  const [newPlatColor, setNewPlatColor] = useState("#6366F1");
  const [newPlatBadge, setNewPlatBadge] = useState("");

  const [statusMessage, setStatusMessage] = useState(
    "Sẵn sàng — mỗi tài khoản giữ phiên đăng nhập riêng"
  );

  /* -------------------------------- Helper -------------------------------- */
  const getPlatform = useCallback(
    (id: string): PlatformItem => {
      return (
        platforms.find((p) => p.id === id) || {
          id,
          name: id,
          url: "",
          color: "#475569",
          badge: (id || "?").slice(0, 2).toUpperCase(),
        }
      );
    },
    [platforms]
  );

  const profileInitials = useCallback((name: string): string => {
    const words = (name || "").trim().split(/\s+/).filter(Boolean);
    if (words.length === 0) return "?";
    if (words.length === 1) return words[0].slice(0, 2).toUpperCase();
    return (words[0][0] + words[words.length - 1][0]).toUpperCase();
  }, []);

  const accountUrl = useCallback(
    (acc: AccountProfile): string => {
      const plat = getPlatform(acc.platformId);
      return normalizeUrl((acc.customUrl || "").trim() || plat.url);
    },
    [getPlatform]
  );

  const anyModalOpen =
    showSettings ||
    showAddAccount ||
    showEditAccount ||
    showAddPlatform ||
    showGroupManager ||
    showInbox ||
    showLan ||
    deleteTarget !== null;

  const closeInboxPanel = useCallback(() => {
    if (IS_NOTIF_WINDOW) {
      void callTauri("hide_notification_window");
      return;
    }
    setShowInbox(false);
  }, []);

  const closeAllModals = useCallback(() => {
    setShowSettings(false);
    setShowAddAccount(false);
    setShowEditAccount(false);
    setShowAddPlatform(false);
    setShowGroupManager(false);
    setShowInbox(false);
    setShowLan(false);
    setDeleteTarget(null);
  }, []);

  const flashStatus = useCallback((msg: string) => {
    setStatusMessage(msg);
  }, []);

  /* ----------------------------- Persistence ------------------------------ */
  useEffect(() => {
    saveJson(LS_PLATFORMS, platforms);
  }, [platforms]);

  useEffect(() => {
    saveJson(LS_GROUPS, groups);
  }, [groups]);

  useEffect(() => {
    if (!saveJson(LS_ACCOUNTS, accounts)) {
      flashStatus("⚠️ Bộ nhớ lưu trữ đầy: đã lưu danh sách tài khoản không kèm ảnh đại diện.");
    }
  }, [accounts]);

  useEffect(() => {
    localStorage.setItem(LS_SIDEBAR, sidebarMode);
    void callTauri("set_sidebar_width", {
      width: RAIL_WIDTH + SIDEBAR_WIDTH_MAP[sidebarMode],
    });
  }, [sidebarMode]);

  useEffect(() => {
    saveJson(LS_SETTINGS, settings);
    void callTauri("set_minimize_to_tray", { enabled: settings.minimizeToTray });
    void callTauri("set_privacy_mode", {
      blockReadReceipts: settings.privacyBlockReadReceipts,
      blockTyping: settings.privacyBlockTyping,
    });
  }, [settings]);

  /* ------------------------------ Theme ------------------------------ */
  useEffect(() => {
    const media = window.matchMedia("(prefers-color-scheme: dark)");
    const apply = () => {
      const resolved =
        settings.theme === "system" ? (media.matches ? "dark" : "light") : settings.theme;
      document.documentElement.dataset.theme = resolved;
    };
    apply();
    media.addEventListener("change", apply);
    return () => media.removeEventListener("change", apply);
  }, [settings.theme]);

  /* --------------------------- Gửi file LAN --------------------------- */
  const [lanPeers, setLanPeers] = useState<LanPeer[]>([]);
  const [lanPeerId, setLanPeerId] = useState("");
  const [lanText, setLanText] = useState("");
  const [lanFeed, setLanFeed] = useState<LanFeedItem[]>([]);
  const [lanSending, setLanSending] = useState(false);
  const [lanDeviceName, setLanDeviceName] = useState("");

  useEffect(() => {
    if (!TAURI_READY) return;
    let cancelled = false;
    const unlisteners: Array<() => void> = [];

    listen<LanPeer[]>("bch-lan-peers", (e) => setLanPeers(e.payload || []))
      .then((fn) => (cancelled ? fn() : unlisteners.push(fn)))
      .catch(() => {});
    listen<{ from: string; text: string; at: number }>("bch-lan-message", (e) => {
      setLanFeed((prev) =>
        [
          {
            id: `lan_${e.payload.at}_${Math.random().toString(36).slice(2, 6)}`,
            from: e.payload.from,
            text: e.payload.text,
            at: e.payload.at || Date.now(),
          },
          ...prev,
        ].slice(0, 80)
      );
    })
      .then((fn) => (cancelled ? fn() : unlisteners.push(fn)))
      .catch(() => {});
    listen<{ from: string; name: string; path: string; size: number; at: number }>(
      "bch-lan-file",
      (e) => {
        setLanFeed((prev) =>
          [
            {
              id: `lanf_${e.payload.at}_${Math.random().toString(36).slice(2, 6)}`,
              from: e.payload.from,
              fileName: e.payload.name,
              path: e.payload.path,
              size: e.payload.size,
              at: e.payload.at || Date.now(),
            },
            ...prev,
          ].slice(0, 80)
        );
        flashStatus(`📥 Đã nhận file "${e.payload.name}" từ ${e.payload.from}`);
      }
    )
      .then((fn) => (cancelled ? fn() : unlisteners.push(fn)))
      .catch(() => {});

    void callTauri<string>("lan_get_device_name").then((n) => {
      if (n) setLanDeviceName(n);
    });
    void callTauri<LanPeer[]>("lan_list_peers").then((p) => setLanPeers(p || []));

    return () => {
      cancelled = true;
      unlisteners.forEach((fn) => fn());
    };
  }, [flashStatus]);

  const saveLanDeviceName = async () => {
    const name = lanDeviceName.trim();
    if (!name) return;
    await callTauri("lan_set_device_name", { name });
    flashStatus(`📡 Tên thiết bị: ${name}`);
  };

  const sendLanText = async () => {
    if (!lanPeerId || !lanText.trim()) return;
    const target = lanPeers.find((p) => p.id === lanPeerId);
    setLanSending(true);
    const res = await callTauri("lan_send_text", {
      peerId: lanPeerId,
      text: lanText.trim(),
    });
    setLanSending(false);
    if (res === null) {
      flashStatus("⚠️ Không gửi được — thiết bị có thể đã offline");
      return;
    }
    setLanFeed((prev) =>
      [
        {
          id: `lan_${Date.now()}`,
          from: "Bạn",
          to: target?.name,
          text: lanText.trim(),
          at: Date.now(),
          outgoing: true,
        },
        ...prev,
      ].slice(0, 80)
    );
    setLanText("");
  };

  const sendLanFile = (file: File) => {
    if (!lanPeerId) {
      flashStatus("⚠️ Chọn thiết bị nhận trước khi gửi file");
      return;
    }
    if (file.size > 80 * 1024 * 1024) {
      flashStatus("⚠️ File lớn hơn 80 MB chưa được hỗ trợ");
      return;
    }
    const target = lanPeers.find((p) => p.id === lanPeerId);
    const reader = new FileReader();
    reader.onload = async () => {
      const raw = String(reader.result || "");
      const b64 = raw.slice(raw.indexOf(",") + 1);
      setLanSending(true);
      const res = await callTauri("lan_send_file", {
        peerId: lanPeerId,
        name: file.name,
        dataBase64: b64,
      });
      setLanSending(false);
      if (res === null) {
        flashStatus("⚠️ Gửi file thất bại — thiết bị có thể đã offline");
        return;
      }
      setLanFeed((prev) =>
        [
          {
            id: `lanf_${Date.now()}`,
            from: "Bạn",
            to: target?.name,
            fileName: file.name,
            size: file.size,
            at: Date.now(),
            outgoing: true,
          },
          ...prev,
        ].slice(0, 80)
      );
      flashStatus(`📤 Đã gửi "${file.name}" tới ${target?.name || "thiết bị"}`);
    };
    reader.readAsDataURL(file);
  };

  useEffect(() => {
    saveJson(LS_NOTIF, notifications.slice(0, MAX_NOTIFICATIONS));
  }, [notifications]);

  useEffect(() => {
    localStorage.setItem(LS_SELECTED, selectedAccountId);
  }, [selectedAccountId]);

  // Đồng bộ danh sách profile sang menu khay hệ thống
  useEffect(() => {
    if (!TAURI_READY) return;
    void callTauri("update_tray_profiles", {
      profiles: accounts.map((a) => ({
        id: a.id,
        name: a.name,
        platformName: getPlatform(a.platformId).name,
      })),
    });
  }, [accounts, platforms]);

  // Cửa sổ chính được tạo ở trạng thái ẩn: hiện nó sau khi React đã vẽ xong khung chat
  // (tránh khung trắng của WebView2 khi khởi động).
  useEffect(() => {
    if (!TAURI_READY || IS_NOTIF_WINDOW) return;
    void callTauri("show_main_window");
  }, []);

  // Tự khôi phục tài khoản bị mất khỏi danh sách, theo 2 nguồn:
  //  1) bản sao lưu `workspace_snapshot.json` trên đĩa (đủ tên/URL/nền tảng)
  //  2) các thư mục `chat_profiles/<nền tảng>/<mã>` còn sót — giữ nguyên phiên đăng nhập
  useEffect(() => {
    if (!TAURI_READY) return;
    let cancelled = false;
    void (async () => {
      const [dirs, snapRaw] = await Promise.all([
        callTauri<string[]>("list_profile_dirs"),
        callTauri<string>("load_workspace_snapshot"),
      ]);
      if (cancelled) return;

      let snapAccounts: AccountProfile[] = [];
      let snapPlatforms: PlatformItem[] = [];
      if (typeof snapRaw === "string" && snapRaw) {
        try {
          const parsed = JSON.parse(snapRaw) as {
            accounts?: AccountProfile[];
            platforms?: PlatformItem[];
          };
          if (Array.isArray(parsed.accounts)) snapAccounts = parsed.accounts;
          if (Array.isArray(parsed.platforms)) snapPlatforms = parsed.platforms;
        } catch {
          /* bản sao lưu hỏng thì bỏ qua */
        }
      }

      // Bổ sung tên/URL còn thiếu từ bản sao lưu (vd. thẻ vừa dựng lại bằng placeholder).
      const mergedAccounts = accounts.map((a) => {
        const fromBackup = snapAccounts.find((s) => s.id === a.id);
        if (
          fromBackup &&
          a.name.startsWith("Khôi phục ·") &&
          typeof fromBackup.name === "string" &&
          !fromBackup.name.startsWith("Khôi phục ·")
        ) {
          return { ...a, name: fromBackup.name, customUrl: fromBackup.customUrl };
        }
        return a;
      });
      const mergedPlatforms = platforms.map((p) => {
        const fromBackup = snapPlatforms.find((s) => s.id === p.id);
        if (fromBackup && (!p.url || p.url === "") && fromBackup.url) {
          return { ...p, name: fromBackup.name, url: fromBackup.url, color: fromBackup.color || p.color };
        }
        return p;
      });

      let baseAccounts = mergedAccounts;
      let basePlatforms = mergedPlatforms;
      let restoredFromBackup = false;
      if (baseAccounts.length === 0 && snapAccounts.length > 0) {
        baseAccounts = snapAccounts;
        if (snapPlatforms.length > 0) basePlatforms = snapPlatforms;
        restoredFromBackup = true;
      }

      const tombstones = loadJson<string[]>(LS_DELETED, []);
      const known = new Set(baseAccounts.map((a) => `${a.platformId}/${a.id}`));
      const diskDirs = Array.isArray(dirs) ? dirs : [];
      const missing = diskDirs.filter((d) => !known.has(d) && !tombstones.includes(d));

      const recovered: AccountProfile[] = missing.map((entry) => {
        const [platformId, profileId] = entry.split("/");
        const fromBackup = snapAccounts.find((a) => a.id === profileId);
        if (fromBackup) {
          return { ...fromBackup, platformId: fromBackup.platformId || platformId };
        }
        return {
          id: profileId,
          name: `Khôi phục · ${profileId}`,
          platformId,
          notificationsEnabled: true,
          createdAt: Date.now(),
        };
      });

      const namesChanged = mergedAccounts.some((a, i) => a.name !== accounts[i]?.name);
      if (recovered.length === 0 && !restoredFromBackup && !namesChanged) return;

      const knownPlatforms = new Set(basePlatforms.map((p) => p.id));
      const newPlatforms: PlatformItem[] = Array.from(
        new Set(recovered.map((a) => a.platformId))
      )
        .filter((id) => !knownPlatforms.has(id))
        .map((id) => ({
          id,
          name: id === "custom" ? "Trang web khôi phục" : id,
          url: "",
          color: "#64748B",
          badge: id.slice(0, 2).toUpperCase(),
        }));

      setAccounts([...baseAccounts, ...recovered]);
      setPlatforms([...basePlatforms, ...newPlatforms]);
      flashStatus(
        recovered.length > 0
          ? `♻️ Đã khôi phục ${recovered.length} tài khoản từ dữ liệu trên đĩa — bấm ✎ Sửa để đặt lại tên/URL.`
          : "♻️ Đã khôi phục danh sách tài khoản từ bản sao lưu trên đĩa."
      );
    })();
    return () => {
      cancelled = true;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  // Bản sao lưu cấu hình ra file JSON trên đĩa (localStorage có thể bị mất).
  useEffect(() => {
    if (!TAURI_READY) return;
    void callTauri("save_workspace_snapshot", {
      payload: JSON.stringify({ accounts, platforms, savedAt: Date.now() }),
    });
  }, [accounts, platforms]);

  // Cảnh báo khi dữ liệu cài đặt đã lưu bị hỏng (loadJson đã tự sao lưu bản gốc).
  useEffect(() => {
    if (storageWarning) flashStatus(storageWarning);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  useEffect(() => {
    if (!TAURI_READY) return;
    void callTauri("set_workspace_always_on_top", { alwaysOnTop: settings.alwaysOnTop });
  }, [settings.alwaysOnTop]);

  useEffect(() => {
    if (!TAURI_READY) return;
    void callTauri("set_max_live_webviews", { maxLive: settings.maxLiveWebviews });
  }, [settings.maxLiveWebviews]);

  /* ----------------------- Đồng bộ danh sách & lựa chọn -------------------- */
  const filteredAccounts = useMemo(() => {
    let list = accounts;
    if (sidebarFilter.kind === "platform") {
      list = accounts.filter((a) => a.platformId === sidebarFilter.id);
    } else if (sidebarFilter.kind === "group") {
      list = accounts.filter((a) => (a.groupIds || []).includes(sidebarFilter.id));
    }
    return [...list].sort((a, b) => {
      if (!!a.pinned !== !!b.pinned) return a.pinned ? -1 : 1;
      const oa = typeof a.order === "number" ? a.order : a.createdAt;
      const ob = typeof b.order === "number" ? b.order : b.createdAt;
      if (oa !== ob) return oa - ob;
      return a.createdAt - b.createdAt;
    });
  }, [accounts, sidebarFilter]);

  const selectedAccount = useMemo(
    () => accounts.find((a) => a.id === selectedAccountId) || null,
    [accounts, selectedAccountId]
  );

  // Đảm bảo tài khoản đang chọn luôn nằm trong danh sách đang lọc
  useEffect(() => {
    if (filteredAccounts.length === 0) return;
    if (!filteredAccounts.some((a) => a.id === selectedAccountId)) {
      setSelectedAccountId(filteredAccounts[0].id);
    }
  }, [filteredAccounts, selectedAccountId]);

  /* --------------------------- WebView theo tài khoản --------------------- */
  useEffect(() => {
    void callTauri("set_modal_overlay_mode", { open: anyModalOpen });
  }, [anyModalOpen]);

  useEffect(() => {
    if (anyModalOpen || !selectedAccount) return;
    const url = accountUrl(selectedAccount);
    if (!url) {
      flashStatus(
        `⚠️ "${selectedAccount.name}" chưa có địa chỉ website — bấm ✎ Sửa để thêm URL`
      );
      return;
    }
    void callTauri("switch_active_chat_account", {
      platform: selectedAccount.platformId,
      url,
      profileId: selectedAccount.id,
      profileName: selectedAccount.name,
      sidebarWidth: SIDEBAR_WIDTH_MAP[sidebarMode],
      webConfig: webConfigJson(selectedAccount),
    });
  }, [anyModalOpen, selectedAccount, sidebarMode, accountUrl, flashStatus, webConfigVersion]);

  /* --------------------- Lắng nghe sự kiện từ WebView --------------------- */
  const accountsRef = useRef(accounts);
  accountsRef.current = accounts;
  const muteRef = useRef(settings.muteSound);
  muteRef.current = settings.muteSound;
  const lastSoundAtRef = useRef(0);

  const pushToast = useCallback((toast: AlertToastItem) => {
    setAlertToasts((prev) => [toast, ...prev.filter((t) => t.profileId !== toast.profileId)].slice(0, 4));
    window.setTimeout(() => {
      setAlertToasts((prev) => prev.filter((t) => t.id !== toast.id));
    }, 12000);
  }, []);

  const handleAlert = useCallback(
    (evt: ProfileAlertEvent) => {
      const kind = (evt.kind || "").toLowerCase();
      const profileId = evt.profile_id || "";
      if (!profileId) return;

      if (kind === "switch_profile") {
        setSelectedAccountId(profileId);
        return;
      }

      const acc = accountsRef.current.find((a) => a.id === profileId);
      const notifyOn = acc ? acc.notificationsEnabled !== false : true;

      setProfileAlerts((prev) => {
        const cur: ProfileAlertState =
          prev[profileId] || { unreadCount: 0, hasIncomingCall: false };
        const unread = Math.max(0, Math.min(999, evt.unread_count || 0));

        if (kind === "unread") {
          return { ...prev, [profileId]: { ...cur, unreadCount: unread } };
        }
        if (kind === "message") {
          return {
            ...prev,
            [profileId]: {
              ...cur,
              unreadCount: Math.max(unread, Math.min(999, cur.unreadCount + 1)),
              lastTitle: evt.title || cur.lastTitle,
              lastBody: evt.body || cur.lastBody,
              lastAt: evt.timestamp || Date.now(),
            },
          };
        }
        if (kind === "call_start") {
          return {
            ...prev,
            [profileId]: {
              ...cur,
              unreadCount: Math.max(unread, cur.unreadCount),
              hasIncomingCall: true,
              caller: evt.caller || cur.caller,
              callKind: evt.call_kind || cur.callKind,
              lastAt: evt.timestamp || Date.now(),
            },
          };
        }
        if (kind === "call_end") {
          return { ...prev, [profileId]: { ...cur, hasIncomingCall: false } };
        }
        return prev;
      });

      if (!notifyOn) return;

      const isCall = kind === "call_start";
      if (kind === "message" || isCall) {
        pushToast({
          id: `${profileId}_${Date.now()}`,
          profileId,
          profileName: acc?.name || profileId,
          platformName: getPlatform(acc?.platformId || evt.platform).name,
          title: isCall ? "📞 Cuộc gọi đến" : evt.title || "Tin nhắn mới",
          body: isCall ? `${evt.caller || "Không rõ người gọi"} • ${evt.call_kind || "voice"}` : evt.body || "",
          isCall,
          timestamp: evt.timestamp || Date.now(),
        });

        if (!muteRef.current) {
          const now = Date.now();
          if (isCall || now - lastSoundAtRef.current > 2500) {
            lastSoundAtRef.current = now;
            playAlertTone(isCall);
          }
        }

        // Ghi vào trung tâm thông báo (danh sách đọc lại & trả lời nhanh)
        const entry: NotificationItem = {
          id: `${profileId}_${Date.now()}_${Math.random().toString(36).slice(2, 7)}`,
          profileId,
          title: isCall ? "📞 Cuộc gọi đến" : evt.title || "Tin nhắn mới",
          body: isCall
            ? `${evt.caller || "Không rõ người gọi"} • ${evt.call_kind || "voice"}`
            : evt.body || "",
          isCall,
          unread: Math.max(0, evt.unread_count || 0),
          at: evt.timestamp || Date.now(),
          conversation: evt.conversation || undefined,
        };
        setNotifications((prev) => {
          const conv = entry.conversation;
          if (conv) {
            const idx = prev.findIndex(
              (n) =>
                n.profileId === entry.profileId &&
                n.conversation === conv &&
                entry.at - n.at < 90000
            );
            if (idx >= 0) {
              const next = prev.slice();
              next[idx] = {
                ...next[idx],
                title: entry.title,
                body: entry.body,
                at: entry.at,
                unread: Math.max(next[idx].unread, entry.unread),
              };
              return next.slice(0, MAX_NOTIFICATIONS);
            }
          }
          return [entry, ...prev].slice(0, MAX_NOTIFICATIONS);
        });
      }
    },
    [getPlatform, pushToast]
  );

  const alertHandlerRef = useRef(handleAlert);
  alertHandlerRef.current = handleAlert;

  useEffect(() => {
    if (!TAURI_READY) return;
    let unlisten: (() => void) | undefined;
    let cancelled = false;

    listen<ProfileAlertEvent>("bch-profile-event", (event) => {
      alertHandlerRef.current(event.payload);
    })
      .then((fn) => {
        if (cancelled) fn();
        else unlisten = fn;
      })
      .catch(() => {});

    return () => {
      cancelled = true;
      if (unlisten) unlisten();
    };
  }, []);

  // Sự kiện từ khay hệ thống
  useEffect(() => {
    if (!TAURI_READY) return;
    let unlistenSelect: (() => void) | undefined;
    let unlistenNotif: (() => void) | undefined;
    let cancelled = false;

    listen<string>("bch-tray-select-profile", (event) => {
      const pid = event.payload;
      const acc = accounts.find((a) => a.id === pid);
      if (acc) selectAccount(acc);
    }).then((fn) => {
      if (cancelled) fn();
      else unlistenSelect = fn;
    }).catch(() => {});

    listen<void>("bch-open-notification-center", () => {
      openNotificationCenter();
    }).then((fn) => {
      if (cancelled) fn();
      else unlistenNotif = fn;
    }).catch(() => {});

    return () => {
      cancelled = true;
      if (unlistenSelect) unlistenSelect();
      if (unlistenNotif) unlistenNotif();
    };
  }, [accounts]);

  /* --------------------------- Bridge v2 (pane → app) --------------------- */
  useEffect(() => {
    if (!TAURI_READY) return;
    let unlisten: (() => void) | undefined;
    let cancelled = false;

    listen<{ profile_id: string; platform: string; op: string; ok: boolean }>(
      "bch-bridge-message",
      (event) => {
        const payload = event.payload;
        if (payload?.op !== "ping") return;
        setBridgeStatus(
          payload.ok
            ? `✅ Bridge v2 hoạt động — ${payload.platform} • ${payload.profile_id}`
            : "⚠️ Bridge có phản hồi nhưng báo lỗi."
        );
      }
    )
      .then((fn) => {
        if (cancelled) fn();
        else unlisten = fn;
      })
      .catch(() => {});

    return () => {
      cancelled = true;
      if (unlisten) unlisten();
    };
  }, []);

  /* ------------------------------- Keyboard ------------------------------- */
  useEffect(() => {
    const onKeyDown = (e: KeyboardEvent) => {
      if (e.key === "Escape" && IS_NOTIF_WINDOW) {
        e.preventDefault();
        closeInboxPanel();
        return;
      }
      if (e.key === "Escape" && anyModalOpen) {
        e.preventDefault();
        closeAllModals();
      }
    };
    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, [anyModalOpen, closeAllModals, closeInboxPanel]);

  /* ------------------------------- Actions -------------------------------- */
  const selectAccount = useCallback(
    (acc: AccountProfile) => {
      closeAllModals();
      setSelectedAccountId(acc.id);
      setAlertToasts((prev) => prev.filter((t) => t.profileId !== acc.id));
    },
    [closeAllModals]
  );

  // Phím tắt chuyển nhanh tài khoản: Ctrl+1…9 mở tài khoản theo thứ tự đang hiển thị,
  // Ctrl+Tab / Ctrl+Shift+Tab chuyển kế tiếp / quay lại.
  useEffect(() => {
    if (!TAURI_READY) return;

    const onKeyDown = (event: KeyboardEvent) => {
      const el = event.target as HTMLElement | null;
      const typing =
        !!el &&
        (el.tagName === "INPUT" ||
          el.tagName === "TEXTAREA" ||
          el.tagName === "SELECT" ||
          el.isContentEditable);
      if (typing) return;
      if (!(event.ctrlKey || event.metaKey) || event.altKey) return;

      if (event.key === "Tab") {
        if (filteredAccounts.length < 2) return;
        event.preventDefault();
        const current = filteredAccounts.findIndex((a) => a.id === selectedAccountId);
        const step = event.shiftKey ? -1 : 1;
        const next =
          filteredAccounts[(current + step + filteredAccounts.length) % filteredAccounts.length];
        if (next) selectAccount(next);
        return;
      }

      if (event.shiftKey || !/^[1-9]$/.test(event.key)) return;
      const acc = filteredAccounts[Number(event.key) - 1];
      if (!acc) return;
      event.preventDefault();
      selectAccount(acc);
    };

    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, [filteredAccounts, selectedAccountId, selectAccount]);

  const openAddAccount = useCallback(
    (platformId?: string) => {
      setNewAccName("");
      setNewAccPlatformId(
        platformId || (sidebarFilter.kind === "platform" ? sidebarFilter.id : "zalo")
      );
      setNewAccCustomUrl("");
      setNewAccNotify(true);
      setNewAccAvatar(undefined);
      setNewAccGroupIds([]);
      closeAllModals();
      setShowAddAccount(true);
    },
    [closeAllModals, sidebarFilter]
  );

  const openEditAccount = useCallback(
    (acc: AccountProfile) => {
      setEditingAccount(acc);
      setEditName(acc.name);
      setEditCustomUrl(acc.customUrl || "");
      setEditNotify(acc.notificationsEnabled !== false);
      setEditAvatar(acc.avatarDataUrl);
      setEditGroupIds(acc.groupIds || []);
      setEditProxy(normalizeProxy(acc.web?.proxy));
      setEditTab("general");
      setProxyResult(null);
      setBridgeStatus("");
      closeAllModals();
      setShowEditAccount(true);
    },
    [closeAllModals]
  );

  const testProfileProxy = async () => {
    if (!TAURI_READY) {
      setProxyResult({
        ok: false,
        ip: "",
        latency_ms: 0,
        via: "",
        message: "Chỉ kiểm tra được khi chạy app Tauri (không phải trình duyệt).",
      });
      return;
    }
    setProxyTesting(true);
    setProxyResult(null);
    const res = await callTauri<ProxyTestResult>("test_profile_proxy", {
      webConfig: JSON.stringify({ proxy: normalizeProxy(editProxy) }),
    });
    setProxyTesting(false);
    if (!res) {
      setProxyResult({
        ok: false,
        ip: "",
        latency_ms: 0,
        via: "",
        message: "Không kiểm tra được — xem log ứng dụng để biết lỗi.",
      });
      return;
    }
    setProxyResult(res);
  };

  const pingBridge = async () => {
    if (!editingAccount) return;
    if (!TAURI_READY) {
      setBridgeStatus("Chỉ kiểm tra được khi chạy app Tauri.");
      return;
    }
    setBridgeStatus("⏳ Đang chờ khung chat phản hồi…");
    const res = await callTauri("bch_bridge_ping", {
      platform: editingAccount.platformId,
      profileId: editingAccount.id,
    });
    if (res === null) {
      setBridgeStatus(
        "⚠️ Chưa kiểm tra được: hãy mở tài khoản này (bấm vào tài khoản ở cột trái) rồi thử lại."
      );
      return;
    }
    window.setTimeout(() => {
      setBridgeStatus((cur) =>
        cur === "⏳ Đang chờ khung chat phản hồi…"
          ? "⚠️ Không có phản hồi: khung chat chưa mở hoặc trang chưa tải xong (bản bridge v2 cần WebView2 khởi động lại một lần)."
          : cur
      );
    }, 4500);
  };

  const createAccount = (e: React.FormEvent) => {
    e.preventDefault();
    const name = newAccName.trim();
    if (!name) {
      alert("Vui lòng nhập tên tài khoản.");
      return;
    }
    const plat = getPlatform(newAccPlatformId);
    const custom = newAccCustomUrl.trim();
    if (!custom && !plat.url) {
      alert("Nền tảng này chưa có URL. Hãy nhập URL riêng cho tài khoản.");
      return;
    }

    const acc: AccountProfile = {
      id: makeAccountId(),
      name,
      platformId: plat.id,
      customUrl: custom ? normalizeUrl(custom) : undefined,
      avatarDataUrl: newAccAvatar,
      notificationsEnabled: newAccNotify,
      pinned: false,
      groupIds: newAccGroupIds,
      createdAt: Date.now(),
    };

    setAccounts((prev) => [...prev, acc]);
    if (custom || plat.url) {
      setSidebarFilter({ kind: "all" });
      setSelectedAccountId(acc.id);
    }
    setShowAddAccount(false);
    flashStatus(`Đã thêm tài khoản "${name}" — đang mở phiên đăng nhập riêng`);
  };

  const saveEditAccount = (e: React.FormEvent) => {
    e.preventDefault();
    if (!editingAccount) return;
    const name = editName.trim();
    if (!name) {
      alert("Vui lòng nhập tên tài khoản.");
      return;
    }
    const custom = editCustomUrl.trim();
    const nextProxy = normalizeProxy(editProxy);
    if (nextProxy.enabled && (!nextProxy.host || !nextProxy.port)) {
      alert("Proxy đang bật nhưng thiếu địa chỉ hoặc cổng.");
      return;
    }
    const wasSelected = selectedAccountId === editingAccount.id;
    const oldProxy = normalizeProxy(editingAccount.web?.proxy);
    const proxyChanged =
      wasSelected && JSON.stringify(oldProxy) !== JSON.stringify(nextProxy);

    setAccounts((prev) =>
      prev.map((a) =>
        a.id === editingAccount.id
          ? {
              ...a,
              name,
              customUrl: custom ? normalizeUrl(custom) : undefined,
              notificationsEnabled: editNotify,
              avatarDataUrl: editAvatar,
              groupIds: editGroupIds,
              web: { ...(a.web || { proxy: DEFAULT_PROXY }), proxy: nextProxy },
            }
          : a
      )
    );
    void callTauri("set_profile_notifications_enabled", {
      platform: editingAccount.platformId,
      profileId: editingAccount.id,
      enabled: editNotify,
    });
    setShowEditAccount(false);

    if (proxyChanged) {
      // Proxy chỉ áp dụng được lúc WebView khởi tạo → đóng rồi mở lại tài khoản.
      void (async () => {
        await callTauri("close_chat_window", {
          platform: editingAccount.platformId,
          profileId: editingAccount.id,
        });
        setWebConfigVersion((v) => v + 1);
      })();
      flashStatus(
        nextProxy.enabled
          ? `🌐 Đã bật proxy cho "${name}" — đang mở lại khung chat`
          : `Đã tắt proxy cho "${name}" — đang mở lại khung chat`
      );
      return;
    }
    flashStatus(`Đã cập nhật tài khoản "${name}"`);
  };

  const togglePin = (acc: AccountProfile) => {
    setAccounts((prev) =>
      prev.map((a) => (a.id === acc.id ? { ...a, pinned: !a.pinned } : a))
    );
  };

  const reorderProfiles = (fromId: string, toId: string) => {
    if (fromId === toId) return;
    const list = filteredAccounts;
    const fromIndex = list.findIndex((a) => a.id === fromId);
    const toIndex = list.findIndex((a) => a.id === toId);
    if (fromIndex < 0 || toIndex < 0) return;
    const reordered = [...list];
    const [moved] = reordered.splice(fromIndex, 1);
    reordered.splice(toIndex, 0, moved);
    // Reassign order values
    const orderMap = new Map<string, number>();
    reordered.forEach((a, i) => orderMap.set(a.id, i * 10));
    setAccounts((prev) =>
      prev.map((a) => {
        const o = orderMap.get(a.id);
        return o !== undefined ? { ...a, order: o } : a;
      })
    );
  };

  const toggleNotifications = (acc: AccountProfile) => {
    const enabled = acc.notificationsEnabled === false;
    setAccounts((prev) =>
      prev.map((a) => (a.id === acc.id ? { ...a, notificationsEnabled: enabled } : a))
    );
    void callTauri("set_profile_notifications_enabled", {
      platform: acc.platformId,
      profileId: acc.id,
      enabled,
    });
    flashStatus(
      enabled ? `🔔 Đã bật thông báo cho "${acc.name}"` : `🔕 Đã tắt thông báo cho "${acc.name}"`
    );
  };

  const openNotificationCenter = useCallback(() => {
    closeAllModals();
    setShowInbox(true);
  }, [closeAllModals]);

  const sendReply = async (item: NotificationItem, send: boolean) => {
    const acc = accounts.find((a) => a.id === item.profileId);
    if (!acc) {
      flashStatus("⚠️ Tài khoản của thông báo này không còn trong danh sách");
      return;
    }
    const text = replyText.trim();
    if (!text) return;

    setReplySending(true);
    const res = await callTauri<string>("send_chat_message", {
      platform: acc.platformId,
      profileId: acc.id,
      text,
      send,
    });
    setReplySending(false);

    if (res === null) {
      flashStatus(
        `⚠️ Không gửi được vào "${acc.name}": khung chat chưa mở. Bấm Mở tài khoản rồi thử lại.`
      );
      return;
    }
    setReplyTargetId(null);
    setReplyText("");
    flashStatus(
      send
        ? `📤 Đã gửi tin nhắn vào khung chat của "${acc.name}"`
        : `📝 Đã điền vào ô chat của "${acc.name}" — bấm Mở để kiểm tra rồi nhấn Enter trong khung chat`
    );
  };

  const reloadAccount = (acc: AccountProfile) => {
    void callTauri("reload_chat_account", {
      platform: acc.platformId,
      profileId: acc.id,
    });
    flashStatus(`🔄 Đã tải lại "${acc.name}"`);
  };

  const openInBrowser = (acc: AccountProfile) => {
    const url = accountUrl(acc);
    if (!url) return;
    void callTauri("open_system_browser", { url });
    flashStatus(`🌐 Đã mở "${acc.name}" bằng trình duyệt hệ thống`);
  };

  const confirmDeleteAccount = async () => {
    if (!deleteTarget) return;
    const target = deleteTarget;
    const remaining = accounts.filter((a) => a.id !== target.id);
    // Ghi "bia mộ" để bước khôi phục từ đĩa không dựng lại tài khoản vừa xoá.
    const deletedKey = `${target.platformId}/${target.id}`;
    const tombstones = loadJson<string[]>(LS_DELETED, []);
    if (!tombstones.includes(deletedKey)) {
      saveJson(LS_DELETED, [...tombstones, deletedKey]);
    }
    setAccounts(remaining);
    setProfileAlerts((prev) => {
      const next = { ...prev };
      delete next[target.id];
      return next;
    });
    setAlertToasts((prev) => prev.filter((t) => t.profileId !== target.id));
    setDeleteTarget(null);

    await callTauri("delete_chat_profile_completely", {
      platform: target.platformId,
      profileId: target.id,
    });

    if (selectedAccountId === target.id) {
      setSelectedAccountId(remaining[0]?.id || "");
    }
    flashStatus(`Đã xóa tài khoản "${target.name}" cùng dữ liệu đăng nhập của nó`);
  };

  const createPlatform = (e: React.FormEvent) => {
    e.preventDefault();
    const name = newPlatName.trim();
    const url = normalizeUrl(newPlatUrl);
    if (!name) {
      alert("Vui lòng nhập tên nền tảng.");
      return;
    }
    if (!url) {
      alert("Vui lòng nhập địa chỉ website.");
      return;
    }
    try {
      new URL(url);
    } catch {
      alert("Địa chỉ website không hợp lệ.");
      return;
    }
    const badge = (newPlatBadge.trim() || name.slice(0, 2)).toUpperCase().slice(0, 3);
    const plat: PlatformItem = {
      id: makePlatformId(),
      name,
      url,
      color: newPlatColor || "#6366F1",
      badge,
    };
    setPlatforms((prev) => [...prev, plat]);
    setSidebarFilter({ kind: "platform", id: plat.id });
    setShowAddPlatform(false);
    setNewPlatName("");
    setNewPlatUrl("");
    setNewPlatBadge("");
    flashStatus(`Đã thêm nền tảng "${name}"`);
  };

  const deletePlatform = (plat: PlatformItem) => {
    const used = accounts.filter((a) => a.platformId === plat.id).length;
    if (used > 0) {
      alert(`Nền tảng "${plat.name}" đang có ${used} tài khoản. Hãy xóa các tài khoản đó trước.`);
      return;
    }
    if (!window.confirm(`Xóa nền tảng "${plat.name}"?`)) return;
    setPlatforms((prev) => prev.filter((p) => p.id !== plat.id));
    if (sidebarFilter.kind === "platform" && sidebarFilter.id === plat.id)
      setSidebarFilter({ kind: "all" });
    flashStatus(`Đã xóa nền tảng "${plat.name}"`);
  };

  const createGroup = () => {
    const name = newGroupName.trim();
    if (!name) {
      alert("Vui lòng nhập tên nhóm.");
      return;
    }
    if (editingGroupId) {
      setGroups((prev) =>
        prev.map((g) =>
          g.id === editingGroupId ? { ...g, name, color: newGroupColor } : g
        )
      );
      flashStatus(`Đã cập nhật nhóm "${name}"`);
    } else {
      const g: UserGroup = {
        id: `g_${Date.now()}_${Math.random().toString(36).slice(2, 7)}`,
        name,
        color: newGroupColor,
        createdAt: Date.now(),
      };
      setGroups((prev) => [...prev, g]);
      flashStatus(`Đã tạo nhóm "${name}"`);
    }
    setNewGroupName("");
    setEditingGroupId(null);
  };

  const deleteGroup = (g: UserGroup) => {
    if (!window.confirm(`Xóa nhóm "${g.name}"? (Tài khoản sẽ không bị xóa)`)) return;
    setGroups((prev) => prev.filter((x) => x.id !== g.id));
    setAccounts((prev) =>
      prev.map((a) =>
        a.groupIds ? { ...a, groupIds: a.groupIds.filter((id) => id !== g.id) } : a
      )
    );
    if (sidebarFilter.kind === "group" && sidebarFilter.id === g.id) {
      setSidebarFilter({ kind: "all" });
    }
    flashStatus(`Đã xóa nhóm "${g.name}"`);
  };

  const toggleAccountGroup = (accId: string, groupId: string) => {
    setAccounts((prev) =>
      prev.map((a) => {
        if (a.id !== accId) return a;
        const cur = a.groupIds || [];
        const next = cur.includes(groupId)
          ? cur.filter((id) => id !== groupId)
          : [...cur, groupId];
        return { ...a, groupIds: next };
      })
    );
  };

  const handleAvatarPick = async (
    e: React.ChangeEvent<HTMLInputElement>,
    apply: (dataUrl: string) => void
  ) => {
    const file = e.target.files?.[0];
    e.target.value = "";
    if (!file) return;
    try {
      apply(await resizeImageToDataUrl(file));
    } catch (err) {
      alert(String(err));
    }
  };

  const exitApp = async () => {
    if (!window.confirm("Thoát Henry Chat Pro? Các phiên đăng nhập vẫn được giữ lại cho lần mở sau.")) {
      return;
    }
    await callTauri("confirm_and_exit_app");
  };

  const enableAllNotifications = () => {
    setAccounts((prev) => prev.map((a) => ({ ...a, notificationsEnabled: true })));
    accounts.forEach((acc) => {
      void callTauri("set_profile_notifications_enabled", {
        platform: acc.platformId,
        profileId: acc.id,
        enabled: true,
      });
    });
    flashStatus("🔔 Đã bật thông báo cho tất cả tài khoản");
  };

  /* ------------------------------- Derived -------------------------------- */
  const totalUnread = useMemo(
    () =>
      accounts.reduce((sum, acc) => {
        if (acc.notificationsEnabled === false) return sum;
        return sum + (profileAlerts[acc.id]?.unreadCount || 0);
      }, 0),
    [accounts, profileAlerts]
  );

  const incomingCalls = useMemo(
    () =>
      accounts
        .filter((a) => a.notificationsEnabled !== false && profileAlerts[a.id]?.hasIncomingCall)
        .map((a) => ({ account: a, alert: profileAlerts[a.id] })),
    [accounts, profileAlerts]
  );

  const modalLeft =
    (IS_NOTIF_WINDOW ? 0 : RAIL_WIDTH) + (sidebarMode === "collapsed" ? 68 : 310);

  const renderAvatar = (acc: AccountProfile, size: "card" | "preview") => {
    const plat = getPlatform(acc.platformId);
    const style = {
      backgroundColor: acc.avatarDataUrl ? "transparent" : plat.color,
    };
    // Ưu tiên icon nền tảng (badge) kèm chữ cái đầu của tên profile để vừa nhận biết
    // platform vừa phân biệt được các profile khi thu gọn.
    const badge = plat.badge.slice(0, 2);
    const fallback = `${badge}${profileInitials(acc.name).charAt(0)}`;
    if (size === "preview") {
      return (
        <div className="bch-avatar-preview" style={style}>
          {acc.avatarDataUrl ? (
            <img src={acc.avatarDataUrl} alt={acc.name} className="bch-acc-avatar-img" />
          ) : (
            fallback
          )}
        </div>
      );
    }
    return (
      <div className="bch-acc-avatar" style={style}>
        {acc.avatarDataUrl ? (
          <img src={acc.avatarDataUrl} alt={acc.name} className="bch-acc-avatar-img" />
        ) : (
          fallback
        )}
        <span
          className="bch-acc-dot"
          style={{ backgroundColor: selectedAccountId === acc.id ? "#10b981" : "#475569" }}
        />
      </div>
    );
  };

  /* -------------------------------- Render -------------------------------- */
  return (
    <div className={`bch-shell ${IS_NOTIF_WINDOW ? "bch-notif-window" : ""}`}>
      <nav className="bch-rail" aria-label="Điều hướng chính">
        <div className="bch-rail-brand" title="Henry Chat Pro">
          <img src={logoUrl} alt="Henry Chat Pro" />
        </div>
        <div className="bch-rail-group">
          <button
            type="button"
            className="bch-rail-btn active"
            title="Tất cả tài khoản"
            onClick={() => {
              setSidebarFilter({ kind: "all" });
              setSidebarMode("full");
            }}
          >
            💬
          </button>
          <button
            type="button"
            className="bch-rail-btn"
            title={`Trung tâm thông báo (${notifications.length})`}
            onClick={openNotificationCenter}
          >
            🔔
            {notifications.length > 0 && (
              <span className="bch-rail-badge">
                {notifications.length > 99 ? "99+" : notifications.length}
              </span>
            )}
          </button>
          <button
            type="button"
            className="bch-rail-btn"
            title="Gửi file / máy khác (LAN)"
            onClick={() => {
              closeAllModals();
              setShowLan(true);
            }}
          >
            📡
            {lanPeers.length > 0 && <span className="bch-rail-dot" />}
          </button>
        </div>
        <div className="bch-rail-bottom">
          <button
            type="button"
            className="bch-rail-btn"
            title={`Giao diện: ${settings.theme === "light" ? "Sáng" : settings.theme === "dark" ? "Tối" : "Theo hệ thống"}`}
            onClick={() =>
              setSettings((prev) => ({
                ...prev,
                theme:
                  prev.theme === "light" ? "dark" : prev.theme === "dark" ? "system" : "light",
              }))
            }
          >
            {settings.theme === "light" ? "☀️" : settings.theme === "dark" ? "🌙" : "🖥"}
          </button>
          <button
            type="button"
            className="bch-rail-btn"
            title="Cài đặt"
            onClick={() => {
              closeAllModals();
              setShowSettings(true);
            }}
          >
            ⚙️
          </button>
          <button
            type="button"
            className="bch-rail-btn danger"
            title="Thoát ứng dụng"
            onClick={exitApp}
          >
            ⏻
          </button>
        </div>
      </nav>
      <aside className={`bch-sidebar ${sidebarMode === "collapsed" ? "mode-collapsed" : ""}`}>
        {sidebarMode === "collapsed" ? (
          <>
            <div className="bch-collapsed-top">
              <button
                type="button"
                className="bch-hamburger-btn"
                onClick={() => setSidebarMode("full")}
                title="Mở rộng danh sách tài khoản"
              >
                <span className="bch-hamburger-line" />
                <span className="bch-hamburger-line" />
                <span className="bch-hamburger-line" />
              </button>
              <button
                type="button"
                className="bch-collapsed-add-btn"
                onClick={() => openAddAccount()}
                title="＋ Thêm tài khoản mới"
              >
                ＋
              </button>
            </div>

            <div style={{ padding: "4px 8px" }}>
              <select
                className="bch-select"
                style={{ width: "100%", fontSize: 11, padding: "4px 6px" }}
                value={
                  sidebarFilter.kind === "all"
                    ? "all"
                    : sidebarFilter.kind === "platform"
                    ? `p:${sidebarFilter.id}`
                    : `g:${sidebarFilter.id}`
                }
                onChange={(e) => {
                  const v = e.target.value;
                  if (v === "all") setSidebarFilter({ kind: "all" });
                  else if (v.startsWith("p:"))
                    setSidebarFilter({ kind: "platform", id: v.slice(2) });
                  else if (v.startsWith("g:"))
                    setSidebarFilter({ kind: "group", id: v.slice(2) });
                }}
                title="Chọn nhóm / nền tảng để lọc"
              >
                <option value="all">Tất cả</option>
                <optgroup label="Nền tảng">
                  {platforms.map((p) => (
                    <option key={p.id} value={`p:${p.id}`}>
                      {p.name}
                    </option>
                  ))}
                </optgroup>
                {groups.length > 0 && (
                  <optgroup label="Nhóm của bạn">
                    {groups.map((g) => (
                      <option key={g.id} value={`g:${g.id}`}>
                        {g.name}
                      </option>
                    ))}
                  </optgroup>
                )}
              </select>
            </div>

            <div className="bch-collapsed-icons-list">
              {filteredAccounts.map((acc) => {
                const plat = getPlatform(acc.platformId);
                const isSelected = selectedAccount?.id === acc.id;
                const notifyOn = acc.notificationsEnabled !== false;
                const alertState = profileAlerts[acc.id];
                const hasCall = notifyOn && Boolean(alertState?.hasIncomingCall);
                const unreadCnt = notifyOn ? alertState?.unreadCount || 0 : 0;
                return (
                  <div
                    key={acc.id}
                    className={`bch-collapsed-profile-item ${
                      draggedProfileId === acc.id ? "dragging" : ""
                    } ${dragOverProfileId === acc.id ? "drag-over" : ""}`}
                    draggable
                    onDragStart={(e) => {
                      setDraggedProfileId(acc.id);
                      e.dataTransfer.effectAllowed = "move";
                    }}
                    onDragOver={(e) => {
                      e.preventDefault();
                      if (draggedProfileId && draggedProfileId !== acc.id) {
                        setDragOverProfileId(acc.id);
                      }
                    }}
                    onDrop={(e) => {
                      e.preventDefault();
                      if (draggedProfileId && draggedProfileId !== acc.id) {
                        reorderProfiles(draggedProfileId, acc.id);
                      }
                      setDraggedProfileId(null);
                      setDragOverProfileId(null);
                    }}
                    onDragEnd={() => {
                      setDraggedProfileId(null);
                      setDragOverProfileId(null);
                    }}
                    title={`${acc.name} (${plat.name})${
                      hasCall ? `\n📞 Đang có cuộc gọi đến: ${alertState?.caller || ""}` : ""
                    }${unreadCnt > 0 ? `\n🔔 ${unreadCnt} tin chưa đọc` : ""}\nChuột trái: mở • Chuột phải: sửa`}
                    onClick={() => selectAccount(acc)}
                    onContextMenu={(e) => {
                      e.preventDefault();
                      openEditAccount(acc);
                    }}
                  >
                    {isSelected && <span className="bch-collapsed-active-pill" />}
                    <button
                      type="button"
                      className={`bch-collapsed-avatar-btn ${isSelected ? "active" : ""} ${
                        hasCall ? "incoming-call" : ""
                      }`}
                      style={{ backgroundColor: acc.avatarDataUrl ? "transparent" : plat.color }}
                    >
                      {acc.avatarDataUrl ? (
                        <img src={acc.avatarDataUrl} alt={acc.name} className="bch-acc-avatar-img" />
                      ) : (
                        `${plat.badge.slice(0, 2)}${profileInitials(acc.name).charAt(0)}`
                      )}
                      {hasCall ? (
                        <span className="bch-collapsed-call-badge" title="Cuộc gọi đến">
                          📞
                        </span>
                      ) : !notifyOn ? (
                        <span className="bch-collapsed-muted-badge" title="Đã tắt thông báo">
                          🔕
                        </span>
                      ) : unreadCnt > 0 ? (
                        <span className="bch-collapsed-unread-badge">
                          {unreadCnt > 99 ? "99+" : unreadCnt}
                        </span>
                      ) : null}
                    </button>
                  </div>
                );
              })}
            </div>

            <div className="bch-collapsed-footer">
              <button
                type="button"
                className="bch-collapsed-tool-btn"
                onClick={() => {
                  closeAllModals();
                  setShowLan(true);
                }}
                title="Gửi file / máy khác (LAN)"
              >
                📡
              </button>
              <button
                type="button"
                className="bch-collapsed-tool-btn"
                onClick={openNotificationCenter}
                title={`Trung tâm thông báo (${notifications.length})`}
              >
                🔔
              </button>
              <button
                type="button"
                className="bch-collapsed-tool-btn"
                onClick={() => {
                  closeAllModals();
                  setShowSettings(true);
                }}
                title="Cài đặt"
              >
                ⚙️
              </button>
              <button
                type="button"
                className="bch-collapsed-tool-btn danger"
                onClick={exitApp}
                title="Thoát ứng dụng"
              >
                ⏻
              </button>
            </div>
          </>
        ) : (
          <>
            <div className="bch-brand-header">
              <div className="bch-brand-info">
                <button
                  type="button"
                  className="bch-hamburger-btn"
                  onClick={() => setSidebarMode("collapsed")}
                  title="Thu gọn danh sách (chỉ hiện icon)"
                >
                  <span className="bch-hamburger-line" />
                  <span className="bch-hamburger-line" />
                  <span className="bch-hamburger-line" />
                </button>
                <img src={logoUrl} alt="Henry Chat Pro" className="bch-brand-logo" />
                <div className="bch-brand-text">
                  <h1 className="bch-brand-title">
                    <span className="bch-brand-name">Tài khoản</span>
                    {totalUnread > 0 && (
                      <span
                        className="bch-total-unread-pill"
                        title={`Tổng ${totalUnread} tin chưa đọc`}
                      >
                        {totalUnread > 99 ? "99+" : totalUnread}
                      </span>
                    )}
                  </h1>
                  <div className="bch-brand-sub">
                    {incomingCalls.length > 0
                      ? `📞 ${incomingCalls.length} cuộc gọi đến`
                      : `${accounts.length} tài khoản • ${platforms.length} nền tảng`}
                  </div>
                </div>
              </div>

              <button
                type="button"
                className="bch-brand-bell-btn"
                title={`Trung tâm thông báo (${notifications.length})`}
                aria-label="Trung tâm thông báo"
                onClick={openNotificationCenter}
              >
                🔔
                {notifications.length > 0 && (
                  <span className="bch-brand-bell-count">
                    {notifications.length > 99 ? "99+" : notifications.length}
                  </span>
                )}
              </button>

              <button
                type="button"
                className="bch-brand-add-btn"
                title="Thêm tài khoản mới"
                aria-label="Thêm tài khoản mới"
                onClick={() => openAddAccount()}
              >
                +
              </button>
            </div>

            {(incomingCalls.length > 0 || alertToasts.length > 0) && (
              <div className="bch-notify-panel">
                {incomingCalls.map(({ account, alert }) => (
                  <div className="bch-call-banner" key={`call_${account.id}`}>
                    <div className="bch-call-banner-top">
                      <span className="bch-call-pulse-dot" />
                      <strong style={{ fontSize: 11.5, color: "#a7f3d0" }}>
                        {alert?.callKind === "video" ? "📹 Cuộc gọi video đến" : "📞 Cuộc gọi đến"}
                      </strong>
                    </div>
                    <div className="bch-toast-body">
                      {alert?.caller || "Không rõ người gọi"} • {account.name}
                    </div>
                    <button
                      type="button"
                      className="bch-btn bch-btn-success"
                      style={{ padding: "4px 8px", fontSize: 11 }}
                      onClick={() => selectAccount(account)}
                    >
                      Mở tài khoản
                    </button>
                  </div>
                ))}

                {alertToasts.map((toast) => (
                  <div
                    className="bch-toast-row"
                    key={toast.id}
                    onClick={() => {
                      const acc = accounts.find((a) => a.id === toast.profileId);
                      if (acc) selectAccount(acc);
                    }}
                  >
                    <div style={{ minWidth: 0, flex: 1 }}>
                      <div className="bch-toast-head">
                        <span>
                          {toast.isCall ? "📞" : "💬"} {toast.profileName}
                        </span>
                        <span style={{ color: "var(--bch-muted)", fontWeight: 500 }}>
                          {toast.platformName}
                        </span>
                      </div>
                      <div className="bch-toast-body">
                        <strong style={{ color: "var(--bch-text)" }}>{toast.title}</strong>
                        {toast.body ? ` — ${toast.body}` : ""}
                      </div>
                    </div>
                    <button
                      type="button"
                      className="bch-toast-close"
                      title="Bỏ qua"
                      onClick={(e) => {
                        e.stopPropagation();
                        setAlertToasts((prev) => prev.filter((t) => t.id !== toast.id));
                      }}
                    >
                      ✕
                    </button>
                  </div>
                ))}
              </div>
            )}

            <div className="bch-plat-section">
              <div className="bch-section-top">
                <span className="bch-section-label">
                  Nền tảng / Trang web ({platforms.length})
                </span>
                <button
                  type="button"
                  className="bch-link-btn"
                  onClick={() => {
                    closeAllModals();
                    setShowAddPlatform(true);
                  }}
                >
                  + Thêm trang web
                </button>
              </div>

              <div className="bch-plat-chips">
                <button
                  type="button"
                  className={`bch-chip ${sidebarFilter.kind === "all" ? "active" : ""}`}
                  onClick={() => setSidebarFilter({ kind: "all" })}
                >
                  Tất cả ({accounts.length})
                </button>
                {platforms.map((plat) => (
                  <button
                    type="button"
                    key={plat.id}
                    className={`bch-chip ${
                      sidebarFilter.kind === "platform" && sidebarFilter.id === plat.id ? "active" : ""
                    }`}
                    onClick={() => setSidebarFilter({ kind: "platform", id: plat.id })}
                    title={`${plat.name} — ${plat.url}`}
                  >
                    <span
                      style={{
                        width: 8,
                        height: 8,
                        borderRadius: 999,
                        backgroundColor: plat.color,
                        display: "inline-block",
                      }}
                    />
                    {plat.name} ({accounts.filter((a) => a.platformId === plat.id).length})
                  </button>
                ))}
              </div>

            <div className="bch-plat-section" style={{ marginTop: 6 }}>
              <div className="bch-section-top">
                <span className="bch-section-label">
                  Nhóm của bạn ({groups.length})
                </span>
                <button
                  type="button"
                  className="bch-link-btn"
                  onClick={() => {
                    closeAllModals();
                    setNewGroupName("");
                    setNewGroupColor("#6366F1");
                    setEditingGroupId(null);
                    setShowGroupManager(true);
                  }}
                >
                  + Quản lý nhóm
                </button>
              </div>
              <div className="bch-plat-chips">
                {groups.length === 0 && (
                  <span style={{ fontSize: 11.5, color: "var(--bch-muted)" }}>
                    Chưa có nhóm. Bấm “+ Quản lý nhóm” để tạo.
                  </span>
                )}
                {groups.map((g) => (
                  <button
                    type="button"
                    key={g.id}
                    className={`bch-chip ${
                      sidebarFilter.kind === "group" && sidebarFilter.id === g.id ? "active" : ""
                    }`}
                    onClick={() => setSidebarFilter({ kind: "group", id: g.id })}
                    title={g.name}
                  >
                    <span
                      style={{
                        width: 8,
                        height: 8,
                        borderRadius: 999,
                        backgroundColor: g.color,
                        display: "inline-block",
                      }}
                    />
                    {g.name} ({accounts.filter((a) => (a.groupIds || []).includes(g.id)).length})
                  </button>
                ))}
              </div>
            </div>
            </div>

            <div className="bch-accounts-container">
              {filteredAccounts.length === 0 && (
                <div className="bch-alert-box bch-alert-info">
                  Chưa có tài khoản nào trong mục này. Bấm <strong>＋ Tài khoản</strong> để thêm.
                </div>
              )}

              {filteredAccounts.map((acc, index) => {
                const plat = getPlatform(acc.platformId);
                const isSelected = selectedAccount?.id === acc.id;
                const notifyOn = acc.notificationsEnabled !== false;
                const alertState = profileAlerts[acc.id];
                const hasCall = notifyOn && Boolean(alertState?.hasIncomingCall);
                const unreadCnt = notifyOn ? alertState?.unreadCount || 0 : 0;
                const snippet = alertState?.lastBody || alertState?.lastTitle;
                const shortcutHint = index < 9 ? `\nCtrl+${index + 1}: mở nhanh` : "";

                return (
                  <div
                    className={`bch-acc-card ${isSelected ? "active" : ""} ${
                      hasCall ? "incoming-call" : ""
                    } ${draggedProfileId === acc.id ? "dragging" : ""} ${
                      dragOverProfileId === acc.id ? "drag-over" : ""
                    }`}
                    key={acc.id}
                    draggable
                    onDragStart={(e) => {
                      setDraggedProfileId(acc.id);
                      e.dataTransfer.effectAllowed = "move";
                    }}
                    onDragOver={(e) => {
                      e.preventDefault();
                      if (draggedProfileId && draggedProfileId !== acc.id) {
                        setDragOverProfileId(acc.id);
                      }
                    }}
                    onDrop={(e) => {
                      e.preventDefault();
                      if (draggedProfileId && draggedProfileId !== acc.id) {
                        reorderProfiles(draggedProfileId, acc.id);
                      }
                      setDraggedProfileId(null);
                      setDragOverProfileId(null);
                    }}
                    onDragEnd={() => {
                      setDraggedProfileId(null);
                      setDragOverProfileId(null);
                    }}
                  >
                    <div
                      className="bch-acc-header"
                      onClick={() => selectAccount(acc)}
                      onContextMenu={(e) => {
                        e.preventDefault();
                        openEditAccount(acc);
                      }}
                      title={`${acc.name} • ${plat.name}\nChuột trái: mở khung chat\nChuột phải: chỉnh sửa${shortcutHint}`}
                    >
                      {renderAvatar(acc, "card")}
                      <div className="bch-acc-meta">
                        <div className="bch-acc-name">
                          <span style={{ overflow: "hidden", textOverflow: "ellipsis" }}>
                            {acc.pinned ? "📌 " : ""}
                            {acc.name}
                          </span>
                          {hasCall ? (
                            <span className="bch-badge-call">📞</span>
                          ) : !notifyOn ? (
                            <span className="bch-badge-muted">🔕</span>
                          ) : unreadCnt > 0 ? (
                            <span className="bch-badge-unread">
                              {unreadCnt > 99 ? "99+" : unreadCnt}
                            </span>
                          ) : null}
                        </div>
                        <div className="bch-acc-sub">
                          {plat.name}
                          {hasCall
                            ? ` • 📞 ${alertState?.caller || "đang gọi"}`
                            : unreadCnt > 0
                            ? ` • ${unreadCnt} tin chưa đọc`
                            : isSelected
                            ? " • Đang mở"
                            : ""}
                        </div>
                      </div>
                    </div>

                    {snippet && (
                      <div
                        className="bch-acc-alert-snippet"
                        onClick={() => selectAccount(acc)}
                        title={snippet}
                      >
                        {alertState?.lastTitle ? `${alertState.lastTitle} — ` : ""}
                        {snippet}
                      </div>
                    )}

                    <div className="bch-acc-actions compact">
                      <button
                        type="button"
                        className={`bch-action-btn icon-only ${acc.pinned ? "pinned" : ""}`}
                        onClick={() => togglePin(acc)}
                        title="Ghim tài khoản lên đầu danh sách"
                        data-tooltip={acc.pinned ? "Bỏ ghim" : "Ghim"}
                      >
                        {acc.pinned ? "📍" : "📌"}
                      </button>
                      <button
                        type="button"
                        className={`bch-action-btn icon-only ${notifyOn ? "notify-on" : "notify-off"}`}
                        onClick={() => toggleNotifications(acc)}
                        title="Bật/tắt thông báo & cảnh báo cuộc gọi"
                        data-tooltip={notifyOn ? "Tắt thông báo" : "Bật thông báo"}
                      >
                        {notifyOn ? "🔔" : "🔕"}
                      </button>
                      <button
                        type="button"
                        className="bch-action-btn icon-only"
                        onClick={() => reloadAccount(acc)}
                        title="Tải lại trang trong khung chat"
                        data-tooltip="Tải lại"
                      >
                        ↻
                      </button>
                      <button
                        type="button"
                        className="bch-action-btn icon-only"
                        onClick={() => openEditAccount(acc)}
                        title="Đổi tên, ảnh đại diện, URL riêng"
                        data-tooltip="Sửa"
                      >
                        ✎
                      </button>
                      <button
                        type="button"
                        className="bch-action-btn icon-only"
                        onClick={() => openInBrowser(acc)}
                        title="Mở website bằng trình duyệt hệ thống"
                        data-tooltip="Mở web"
                      >
                        🌐
                      </button>
                      <button
                        type="button"
                        className="bch-action-btn icon-only danger"
                        onClick={() => {
                          closeAllModals();
                          setDeleteTarget(acc);
                        }}
                        title="Xóa tài khoản và dữ liệu đăng nhập"
                        data-tooltip="Xóa"
                      >
                        🗑
                      </button>
                    </div>
                  </div>
                );
              })}
            </div>

            <div className="bch-sidebar-footer">
              <div className="bch-status-banner">{statusMessage}</div>
              <div className="bch-footer-row">
                <button
                  type="button"
                  className="bch-btn"
                  onClick={() => {
                    closeAllModals();
                    setShowAddPlatform(true);
                  }}
                >
                  ＋ Trang web
                </button>
                <button
                  type="button"
                  className="bch-btn"
                  onClick={() => {
                    closeAllModals();
                    setShowLan(true);
                  }}
                >
                  📡 Gửi file
                </button>
              </div>
            </div>
          </>
        )}
      </aside>

      <main className="bch-chat-viewport">
        {!selectedAccount ? (
          <div className="bch-stage-card">
            <div style={{ fontSize: 34, marginBottom: 8 }}>🗂️</div>
            <h2 style={{ margin: "0 0 8px", fontSize: 17, color: "var(--bch-text)" }}>
              Workspace đa tài khoản
            </h2>
            <p style={{ margin: "0 0 14px", fontSize: 12.5, color: "var(--bch-muted)", lineHeight: 1.6 }}>
              Mỗi tài khoản chạy trong một phiên WebView riêng, giữ đăng nhập độc lập.
              {accounts.length === 0
                ? " Hãy thêm tài khoản đầu tiên để bắt đầu."
                : " Chọn một tài khoản ở cột bên trái để mở khung chat."}
            </p>
            <button
              type="button"
              className="bch-btn bch-btn-primary"
              onClick={() => openAddAccount()}
            >
              ＋ Thêm tài khoản
            </button>
          </div>
        ) : (
          <div style={{ fontSize: 11.5, color: "var(--bch-muted)", textAlign: "center" }}>
            Khung chat của “{selectedAccount.name}” đang chạy trong cửa sổ WebView2 phía trên.
            <br />
            {TAURI_READY
              ? "Nếu khung chat không hiện, hãy bấm ↻ Tải lại hoặc chọn lại tài khoản."
              : "Bản xem trước trên trình duyệt không mở được WebView2."}
          </div>
        )}
      </main>

      {/* ------------------------------ MODALS ------------------------------ */}
      {showAddAccount && (
        <div
          className="bch-modal-overlay"
          style={{ left: modalLeft }}
          onClick={(e) => {
            if (e.target === e.currentTarget) setShowAddAccount(false);
          }}
        >
          <div className="bch-modal-card" style={{ maxWidth: 520 }}>
            <div className="bch-modal-header">
              <div>
                <h2 style={{ margin: 0, fontSize: 16, color: "var(--bch-text)" }}>Thêm tài khoản</h2>
                <p style={{ margin: "4px 0 0", fontSize: 11.5, color: "var(--bch-muted)" }}>
                  Tài khoản mới có cookie &amp; localStorage riêng, không ảnh hưởng tài khoản khác.
                </p>
              </div>
              <button type="button" className="bch-toast-close" onClick={() => setShowAddAccount(false)}>
                ✕
              </button>
            </div>

            <form onSubmit={createAccount}>
              <div className="bch-form-group">
                <label className="bch-label">Tên tài khoản</label>
                <input
                  className="bch-input"
                  value={newAccName}
                  onChange={(e) => setNewAccName(e.target.value)}
                  placeholder="Ví dụ: Zalo Công Ty, Facebook Cá Nhân..."
                  autoFocus
                />
              </div>

              <div className="bch-form-group">
                <label className="bch-label">Nền tảng</label>
                <select
                  className="bch-select"
                  value={newAccPlatformId}
                  onChange={(e) => setNewAccPlatformId(e.target.value)}
                >
                  {platforms.map((p) => (
                    <option key={p.id} value={p.id}>
                      {p.name}
                    </option>
                  ))}
                  <option value={CUSTOM_URL_OPTION_VALUE}>— URL tùy chỉnh —</option>
                </select>
              </div>

              {groups.length > 0 && (
                <div className="bch-form-group">
                  <label className="bch-label">Nhóm người dùng (có thể chọn nhiều)</label>
                  <div style={{ display: "flex", flexWrap: "wrap", gap: 6 }}>
                    {groups.map((g) => (
                      <label
                        key={g.id}
                        style={{
                          display: "inline-flex",
                          alignItems: "center",
                          gap: 4,
                          fontSize: 12,
                          padding: "4px 8px",
                          borderRadius: 6,
                          border: "1px solid #334155",
                          cursor: "pointer",
                          background: newAccGroupIds.includes(g.id) ? g.color : "transparent",
                          color: newAccGroupIds.includes(g.id) ? "#fff" : "var(--bch-text-soft)",
                        }}
                      >
                        <input
                          type="checkbox"
                          checked={newAccGroupIds.includes(g.id)}
                          onChange={() =>
                            setNewAccGroupIds((prev) =>
                              prev.includes(g.id)
                                ? prev.filter((id) => id !== g.id)
                                : [...prev, g.id]
                            )
                          }
                          style={{ display: "none" }}
                        />
                        {g.name}
                      </label>
                    ))}
                  </div>
                </div>
              )}

              <div className="bch-form-group">
                <label className="bch-label">
                  URL riêng {newAccPlatformId === CUSTOM_URL_OPTION_VALUE ? "" : "(không bắt buộc)"}
                </label>
                <input
                  className="bch-input"
                  value={newAccCustomUrl}
                  onChange={(e) => setNewAccCustomUrl(e.target.value)}
                  placeholder={
                    newAccPlatformId === CUSTOM_URL_OPTION_VALUE
                      ? "https://example.com"
                      : getPlatform(newAccPlatformId).url
                  }
                />
              </div>

              <div className="bch-form-group">
                <label className="bch-label">Ảnh đại diện</label>
                <div className="bch-avatar-picker">
                  {renderAvatar(
                    {
                      id: "preview",
                      name: newAccName || "?",
                      platformId: newAccPlatformId,
                      avatarDataUrl: newAccAvatar,
                      createdAt: 0,
                    },
                    "preview"
                  )}
                  <div style={{ flex: 1 }}>
                    <input
                      type="file"
                      accept="image/*"
                      onChange={(e) =>
                        handleAvatarPick(e, (dataUrl) => setNewAccAvatar(dataUrl))
                      }
                      style={{ fontSize: 11, color: "var(--bch-muted)" }}
                    />
                    {newAccAvatar && (
                      <button
                        type="button"
                        className="bch-link-btn"
                        onClick={() => setNewAccAvatar(undefined)}
                      >
                        Bỏ ảnh
                      </button>
                    )}
                  </div>
                </div>
              </div>

              <label
                style={{
                  display: "flex",
                  alignItems: "center",
                  gap: 8,
                  fontSize: 12.5,
                  color: "var(--bch-text-soft)",
                  marginBottom: 14,
                }}
              >
                <input
                  type="checkbox"
                  checked={newAccNotify}
                  onChange={(e) => setNewAccNotify(e.target.checked)}
                />
                Bật thông báo &amp; cảnh báo cuộc gọi cho tài khoản này
              </label>

              <div style={{ display: "flex", gap: 8, justifyContent: "flex-end" }}>
                <button type="button" className="bch-btn" onClick={() => setShowAddAccount(false)}>
                  Hủy
                </button>
                <button type="submit" className="bch-btn bch-btn-primary">
                  Thêm &amp; mở tài khoản
                </button>
              </div>
            </form>
          </div>
        </div>
      )}

      {showEditAccount && editingAccount && (
        <div
          className="bch-modal-overlay"
          style={{ left: modalLeft }}
          onClick={(e) => {
            if (e.target === e.currentTarget) setShowEditAccount(false);
          }}
        >
          <div className="bch-modal-card" style={{ maxWidth: 520 }}>
            <div className="bch-modal-header">
              <div>
                <h2 style={{ margin: 0, fontSize: 16, color: "var(--bch-text)" }}>Chỉnh sửa tài khoản</h2>
                <p style={{ margin: "4px 0 0", fontSize: 11.5, color: "var(--bch-muted)" }}>
                  {getPlatform(editingAccount.platformId).name} • mã profile: {editingAccount.id}
                </p>
              </div>
              <button type="button" className="bch-toast-close" onClick={() => setShowEditAccount(false)}>
                ✕
              </button>
            </div>

            <form onSubmit={saveEditAccount}>
              <div className="bch-tabs">
                <button
                  type="button"
                  className={`bch-tab ${editTab === "general" ? "is-active" : ""}`}
                  onClick={() => setEditTab("general")}
                >
                  Chung
                </button>
                <button
                  type="button"
                  className={`bch-tab ${editTab === "network" ? "is-active" : ""}`}
                  onClick={() => setEditTab("network")}
                >
                  Mạng &amp; Proxy{normalizeProxy(editProxy).enabled ? " 🌐" : ""}
                </button>
              </div>

              {editTab === "general" && (
                <>
                  <div className="bch-form-group">
                    <label className="bch-label">Tên tài khoản</label>
                    <input
                      className="bch-input"
                      value={editName}
                      onChange={(e) => setEditName(e.target.value)}
                      autoFocus
                    />
                  </div>

                  <div className="bch-form-group">
                    <label className="bch-label">
                      URL riêng (để trống sẽ dùng URL của nền tảng)
                    </label>
                    <input
                      className="bch-input"
                      value={editCustomUrl}
                      onChange={(e) => setEditCustomUrl(e.target.value)}
                      placeholder={getPlatform(editingAccount.platformId).url}
                    />
                  </div>

                  <div className="bch-form-group">
                    <label className="bch-label">Ảnh đại diện</label>
                    <div className="bch-avatar-picker">
                      {renderAvatar(
                        { ...editingAccount, name: editName, avatarDataUrl: editAvatar },
                        "preview"
                      )}
                      <div style={{ flex: 1 }}>
                        <input
                          type="file"
                          accept="image/*"
                          onChange={(e) => handleAvatarPick(e, (dataUrl) => setEditAvatar(dataUrl))}
                          style={{ fontSize: 11, color: "var(--bch-muted)" }}
                        />
                        {editAvatar && (
                          <button
                            type="button"
                            className="bch-link-btn"
                            onClick={() => setEditAvatar(undefined)}
                          >
                            Bỏ ảnh
                          </button>
                        )}
                      </div>
                    </div>
                  </div>
                </>
              )}

              {editTab === "network" && (
                <div className="bch-net-panel">
                  <label className="bch-check-row">
                    <input
                      type="checkbox"
                      checked={editProxy.enabled}
                      onChange={(e) =>
                        setEditProxy((p) => ({ ...p, enabled: e.target.checked }))
                      }
                    />
                    Dùng proxy riêng cho tài khoản này
                  </label>

                  <div className="bch-net-grid">
                    <select
                      className="bch-select"
                      value={editProxy.kind}
                      onChange={(e) =>
                        setEditProxy((p) => ({
                          ...p,
                          kind: e.target.value === "socks5" ? "socks5" : "http",
                        }))
                      }
                    >
                      <option value="http">HTTP</option>
                      <option value="socks5">SOCKS5</option>
                    </select>
                    <input
                      className="bch-input"
                      placeholder="proxy.example.com"
                      value={editProxy.host}
                      onChange={(e) => setEditProxy((p) => ({ ...p, host: e.target.value }))}
                      autoComplete="off"
                      spellCheck={false}
                    />
                    <input
                      className="bch-input"
                      inputMode="numeric"
                      placeholder="8080"
                      value={editProxy.port || ""}
                      onChange={(e) =>
                        setEditProxy((p) => ({
                          ...p,
                          port: Number(e.target.value.replace(/\D+/g, "")) || 0,
                        }))
                      }
                    />
                  </div>

                  <div className="bch-net-grid-2">
                    <input
                      className="bch-input"
                      placeholder="Tên đăng nhập proxy (nếu có)"
                      value={editProxy.username}
                      onChange={(e) => setEditProxy((p) => ({ ...p, username: e.target.value }))}
                      autoComplete="off"
                      spellCheck={false}
                    />
                    <input
                      className="bch-input"
                      type="password"
                      placeholder="Mật khẩu proxy"
                      value={editProxy.password}
                      onChange={(e) => setEditProxy((p) => ({ ...p, password: e.target.value }))}
                      autoComplete="new-password"
                    />
                  </div>

                  <input
                    className="bch-input"
                    placeholder="Bỏ qua proxy cho: *.local,192.168.* (không bắt buộc)"
                    value={editProxy.bypass}
                    onChange={(e) => setEditProxy((p) => ({ ...p, bypass: e.target.value }))}
                    spellCheck={false}
                  />

                  <div className="bch-net-actions">
                    <button
                      type="button"
                      className="bch-btn"
                      onClick={testProfileProxy}
                      disabled={proxyTesting}
                    >
                      {proxyTesting ? "⏳ Đang kiểm tra…" : "🧪 Kiểm tra proxy"}
                    </button>
                    <button type="button" className="bch-btn" onClick={pingBridge}>
                      🔌 Kiểm tra bridge
                    </button>
                  </div>

                  {proxyResult && (
                    <p
                      className={`bch-net-result ${proxyResult.ok ? "is-ok" : "is-err"}`}
                    >
                      {proxyResult.ok ? "✅" : "⛔"} {proxyResult.message}
                      {proxyResult.via && (
                        <span className="bch-net-meta">
                          {proxyResult.via} • {proxyResult.latency_ms} ms
                        </span>
                      )}
                    </p>
                  )}
                  {bridgeStatus && <p className="bch-net-result">{bridgeStatus}</p>}

                  <p className="bch-hint">
                    Proxy được áp dụng lúc khung chat khởi tạo, nên bấm “Lưu thay đổi” app
                    sẽ tự đóng và mở lại tài khoản này. IP thật được chặn rò rỉ qua
                    WebRTC/UDP và QUIC. Proxy có mật khẩu được chuyển tiếp qua relay nội bộ
                    127.0.0.1 — mật khẩu không lộ ra command line.
                  </p>
                </div>
              )}

              {groups.length > 0 && (
                <div className="bch-form-group">
                  <label className="bch-label">Nhóm người dùng (có thể chọn nhiều)</label>
                  <div style={{ display: "flex", flexWrap: "wrap", gap: 6 }}>
                    {groups.map((g) => (
                      <label
                        key={g.id}
                        style={{
                          display: "inline-flex",
                          alignItems: "center",
                          gap: 4,
                          fontSize: 12,
                          padding: "4px 8px",
                          borderRadius: 6,
                          border: "1px solid #334155",
                          cursor: "pointer",
                          background: editGroupIds.includes(g.id) ? g.color : "transparent",
                          color: editGroupIds.includes(g.id) ? "#fff" : "var(--bch-text-soft)",
                        }}
                      >
                        <input
                          type="checkbox"
                          checked={editGroupIds.includes(g.id)}
                          onChange={() =>
                            setEditGroupIds((prev) =>
                              prev.includes(g.id)
                                ? prev.filter((id) => id !== g.id)
                                : [...prev, g.id]
                            )
                          }
                          style={{ display: "none" }}
                        />
                        {g.name}
                      </label>
                    ))}
                  </div>
                </div>
              )}

              <label
                style={{
                  display: "flex",
                  alignItems: "center",
                  gap: 8,
                  fontSize: 12.5,
                  color: "var(--bch-text-soft)",
                  marginBottom: 14,
                }}
              >
                <input
                  type="checkbox"
                  checked={editNotify}
                  onChange={(e) => setEditNotify(e.target.checked)}
                />
                Bật thông báo &amp; cảnh báo cuộc gọi
              </label>

              <div style={{ display: "flex", gap: 8, justifyContent: "space-between" }}>
                <button
                  type="button"
                  className="bch-btn bch-btn-danger"
                  onClick={() => {
                    setShowEditAccount(false);
                    setDeleteTarget(editingAccount);
                  }}
                >
                  🗑 Xóa tài khoản
                </button>
                <div style={{ display: "flex", gap: 8 }}>
                  <button type="button" className="bch-btn" onClick={() => setShowEditAccount(false)}>
                    Hủy
                  </button>
                  <button type="submit" className="bch-btn bch-btn-primary">
                    Lưu thay đổi
                  </button>
                </div>
              </div>
            </form>
          </div>
        </div>
      )}

      {showAddPlatform && (
        <div
          className="bch-modal-overlay"
          style={{ left: modalLeft }}
          onClick={(e) => {
            if (e.target === e.currentTarget) setShowAddPlatform(false);
          }}
        >
          <div className="bch-modal-card" style={{ maxWidth: 520 }}>
            <div className="bch-modal-header">
              <div>
                <h2 style={{ margin: 0, fontSize: 16, color: "var(--bch-text)" }}>Thêm nền tảng / trang web</h2>
                <p style={{ margin: "4px 0 0", fontSize: 11.5, color: "var(--bch-muted)" }}>
                  Bất kỳ website nào cho phép đăng nhập trên trình duyệt.
                </p>
              </div>
              <button type="button" className="bch-toast-close" onClick={() => setShowAddPlatform(false)}>
                ✕
              </button>
            </div>

            <form onSubmit={createPlatform}>
              <div className="bch-form-group">
                <label className="bch-label">Tên nền tảng</label>
                <input
                  className="bch-input"
                  value={newPlatName}
                  onChange={(e) => setNewPlatName(e.target.value)}
                  placeholder="Ví dụ: Facebook, Discord, Shopee..."
                  autoFocus
                />
              </div>
              <div className="bch-form-group">
                <label className="bch-label">Địa chỉ website</label>
                <input
                  className="bch-input"
                  value={newPlatUrl}
                  onChange={(e) => setNewPlatUrl(e.target.value)}
                  placeholder="https://example.com"
                />
              </div>
              <div style={{ display: "flex", gap: 10 }}>
                <div className="bch-form-group" style={{ flex: 1 }}>
                  <label className="bch-label">Màu nhận diện</label>
                  <input
                    className="bch-input"
                    type="color"
                    value={newPlatColor}
                    onChange={(e) => setNewPlatColor(e.target.value)}
                    style={{ padding: 4, height: 36 }}
                  />
                </div>
                <div className="bch-form-group" style={{ flex: 1 }}>
                  <label className="bch-label">Nhãn (2–3 ký tự)</label>
                  <input
                    className="bch-input"
                    value={newPlatBadge}
                    onChange={(e) => setNewPlatBadge(e.target.value)}
                    placeholder="FB"
                    maxLength={3}
                  />
                </div>
              </div>

              {platforms.some((p) => !p.isBuiltIn) && (
                <div className="bch-section-card">
                  <div className="bch-section-label" style={{ marginBottom: 8 }}>
                    Nền tảng tự thêm
                  </div>
                  {platforms
                    .filter((p) => !p.isBuiltIn)
                    .map((p) => (
                      <div
                        key={p.id}
                        style={{
                          display: "flex",
                          alignItems: "center",
                          justifyContent: "space-between",
                          gap: 8,
                          fontSize: 12,
                          color: "var(--bch-text-soft)",
                          padding: "4px 0",
                        }}
                      >
                        <span style={{ overflow: "hidden", textOverflow: "ellipsis" }}>
                          <span
                            style={{
                              display: "inline-block",
                              width: 8,
                              height: 8,
                              borderRadius: 999,
                              backgroundColor: p.color,
                              marginRight: 6,
                            }}
                          />
                          {p.name} — {p.url}
                        </span>
                        <button
                          type="button"
                          className="bch-link-btn"
                          onClick={() => deletePlatform(p)}
                        >
                          Xóa
                        </button>
                      </div>
                    ))}
                </div>
              )}

              <div style={{ display: "flex", gap: 8, justifyContent: "flex-end" }}>
                <button type="button" className="bch-btn" onClick={() => setShowAddPlatform(false)}>
                  Hủy
                </button>
                <button type="submit" className="bch-btn bch-btn-primary">
                  Thêm nền tảng
                </button>
              </div>
            </form>
          </div>
        </div>
      )}

      {showLan && (
        <div
          className="bch-modal-overlay"
          style={{ left: modalLeft }}
          onClick={(e) => {
            if (e.target === e.currentTarget) setShowLan(false);
          }}
        >
          <div className="bch-modal-card" style={{ maxWidth: 620 }}>
            <div className="bch-modal-header">
              <div>
                <h2 style={{ margin: 0, fontSize: 16 }}>📡 Gửi file & máy khác</h2>
                <p style={{ margin: "4px 0 0", fontSize: 11.5, color: "var(--bch-muted)" }}>
                  Cùng mạng Wi-Fi/LAN • {lanPeers.length} thiết bị online
                </p>
              </div>
              <button type="button" className="bch-toast-close" onClick={() => setShowLan(false)}>
                ✕
              </button>
            </div>

            <div className="bch-lan-body">
              <div className="bch-lan-row">
                <input
                  className="bch-input"
                  value={lanDeviceName}
                  onChange={(e) => setLanDeviceName(e.target.value)}
                  placeholder="Tên thiết bị này"
                />
                <button type="button" className="bch-btn" onClick={() => void saveLanDeviceName()}>
                  Lưu tên
                </button>
              </div>

              <select
                className="bch-select"
                value={lanPeerId}
                onChange={(e) => setLanPeerId(e.target.value)}
              >
                <option value="">— Chọn thiết bị nhận —</option>
                {lanPeers.map((p) => (
                  <option key={p.id} value={p.id}>
                    {p.name} • {p.ip}
                  </option>
                ))}
              </select>

              <div className="bch-lan-row">
                <input
                  className="bch-input"
                  value={lanText}
                  onChange={(e) => setLanText(e.target.value)}
                  placeholder="Nhập tin nhắn cần gửi..."
                  onKeyDown={(e) => {
                    if (e.key === "Enter" && !e.shiftKey) {
                      e.preventDefault();
                      void sendLanText();
                    }
                  }}
                />
                <button
                  type="button"
                  className="bch-btn bch-btn-primary"
                  disabled={lanSending || !lanText.trim() || !lanPeerId}
                  onClick={() => void sendLanText()}
                >
                  Gửi
                </button>
              </div>

              <label className="bch-lan-file">
                📎 Gửi file (tối đa 80 MB)
                <input
                  type="file"
                  style={{ display: "none" }}
                  onChange={(e) => {
                    const f = e.target.files?.[0];
                    if (f) sendLanFile(f);
                    e.currentTarget.value = "";
                  }}
                />
              </label>

              <div className="bch-lan-feed">
                {lanFeed.length === 0 && (
                  <div className="bch-inbox-empty">
                    Chưa có dữ liệu. Chọn một thiết bị trong danh sách rồi gửi tin nhắn hoặc file.
                  </div>
                )}
                {lanFeed.map((item) => (
                  <div key={item.id} className={`bch-lan-item ${item.outgoing ? "out" : ""}`}>
                    <div className="bch-lan-item-top">
                      <strong>{item.from}</strong>
                      {item.to ? <span className="bch-lan-to">→ {item.to}</span> : null}
                      <span className="bch-lan-time">
                        {new Date(item.at).toLocaleTimeString("vi-VN")}
                      </span>
                    </div>
                    {item.text ? <div className="bch-lan-text">{item.text}</div> : null}
                    {item.fileName ? (
                      <div className="bch-lan-text">
                        📄 {item.fileName}
                        {item.size ? ` • ${(item.size / 1024).toFixed(0)} KB` : ""}
                      </div>
                    ) : null}
                  </div>
                ))}
              </div>
            </div>
          </div>
        </div>
      )}

      {showSettings && (
        <div
          className="bch-modal-overlay"
          style={{ left: modalLeft }}
          onClick={(e) => {
            if (e.target === e.currentTarget) setShowSettings(false);
          }}
        >
          <div className="bch-modal-card" style={{ maxWidth: 560 }}>
            <div className="bch-modal-header">
              <div>
                <h2 style={{ margin: 0, fontSize: 16, color: "var(--bch-text)" }}>Cài đặt</h2>
                <p style={{ margin: "4px 0 0", fontSize: 11.5, color: "var(--bch-muted)" }}>
                  Henry Chat Pro v3.0.0 • {accounts.length} tài khoản • {platforms.length} nền tảng
                </p>
              </div>
              <button type="button" className="bch-toast-close" onClick={() => setShowSettings(false)}>
                ✕
              </button>
            </div>

            <div className="bch-section-card">
              <div className="bch-section-label" style={{ marginBottom: 9 }}>
                Giao diện
              </div>
              <div className="bch-footer-row" style={{ gridTemplateColumns: "1fr 1fr 1fr", marginBottom: 10 }}>
                <button
                  type="button"
                  className={`bch-btn ${settings.theme === "light" ? "bch-btn-primary" : ""}`}
                  onClick={() => setSettings((prev) => ({ ...prev, theme: "light" }))}
                >
                  ☀️ Sáng
                </button>
                <button
                  type="button"
                  className={`bch-btn ${settings.theme === "dark" ? "bch-btn-primary" : ""}`}
                  onClick={() => setSettings((prev) => ({ ...prev, theme: "dark" }))}
                >
                  🌙 Tối
                </button>
                <button
                  type="button"
                  className={`bch-btn ${settings.theme === "system" ? "bch-btn-primary" : ""}`}
                  onClick={() => setSettings((prev) => ({ ...prev, theme: "system" }))}
                >
                  🖥 Hệ thống
                </button>
              </div>
              <div className="bch-footer-row" style={{ gridTemplateColumns: "1fr 1fr" }}>
                <button
                  type="button"
                  className={`bch-btn ${sidebarMode === "full" ? "bch-btn-primary" : ""}`}
                  onClick={() => setSidebarMode("full")}
                >
                  Danh sách đầy đủ (310px)
                </button>
                <button
                  type="button"
                  className={`bch-btn ${sidebarMode === "collapsed" ? "bch-btn-primary" : ""}`}
                  onClick={() => setSidebarMode("collapsed")}
                >
                  Chỉ icon (68px)
                </button>
              </div>
              <label
                style={{
                  display: "flex",
                  alignItems: "center",
                  gap: 8,
                  fontSize: 12.5,
                  color: "var(--bch-text-soft)",
                  marginTop: 12,
                }}
              >
                <input
                  type="checkbox"
                  checked={settings.alwaysOnTop}
                  onChange={(e) =>
                    setSettings((prev) => ({ ...prev, alwaysOnTop: e.target.checked }))
                  }
                />
                Luôn hiển thị trên cùng (ghim cửa sổ)
              </label>
              <label
                style={{
                  display: "flex",
                  alignItems: "center",
                  gap: 8,
                  fontSize: 12.5,
                  color: "var(--bch-text-soft)",
                  marginTop: 8,
                }}
              >
                <input
                  type="checkbox"
                  checked={settings.minimizeToTray}
                  onChange={(e) =>
                    setSettings((prev) => ({ ...prev, minimizeToTray: e.target.checked }))
                  }
                />
                Thu xuống khay hệ thống khi bấm đóng (thoát thật qua menu khay)
              </label>
              <div style={{ marginTop: 12, borderTop: "1px solid #334155", paddingTop: 10 }}>
                <div style={{ fontSize: 12.5, color: "var(--bch-text-soft)", marginBottom: 6, fontWeight: 600 }}>
                  Riêng tư (chặn tín hiệu đã đọc / đang gõ)
                </div>
                <label style={{ display: "flex", alignItems: "center", gap: 8, fontSize: 12.5, color: "var(--bch-text-soft)", marginTop: 4 }}>
                  <input
                    type="checkbox"
                    checked={settings.privacyBlockReadReceipts}
                    onChange={(e) =>
                      setSettings((prev) => ({ ...prev, privacyBlockReadReceipts: e.target.checked }))
                    }
                  />
                  Không gửi "đã đọc" (read receipt)
                </label>
                <label style={{ display: "flex", alignItems: "center", gap: 8, fontSize: 12.5, color: "var(--bch-text-soft)", marginTop: 4 }}>
                  <input
                    type="checkbox"
                    checked={settings.privacyBlockTyping}
                    onChange={(e) =>
                      setSettings((prev) => ({ ...prev, privacyBlockTyping: e.target.checked }))
                    }
                  />
                  Không gửi "đang gõ" (typing indicator)
                </label>
                <p style={{ margin: "6px 0 0", fontSize: 11, color: "var(--bch-muted)" }}>
                  Áp dụng cho mọi profile. Có thể không hiệu quả trên một số nền tảng (Telegram MTProto).
                </p>
              </div>
              <p style={{ margin: "10px 0 0", fontSize: 11, color: "var(--bch-muted)", lineHeight: 1.5 }}>
                ⌨️ Phím tắt: <strong>Ctrl + 1…9</strong> mở tài khoản theo thứ tự đang hiển thị •{" "}
                <strong>Ctrl + Tab</strong> / <strong>Ctrl + Shift + Tab</strong> chuyển kế tiếp hoặc
                quay lại.
              </p>
            </div>

            <div className="bch-section-card">
              <div className="bch-section-label" style={{ marginBottom: 9 }}>
                Thông báo
              </div>
              <label
                style={{
                  display: "flex",
                  alignItems: "center",
                  gap: 8,
                  fontSize: 12.5,
                  color: "var(--bch-text-soft)",
                }}
              >
                <input
                  type="checkbox"
                  checked={settings.muteSound}
                  onChange={(e) => setSettings((prev) => ({ ...prev, muteSound: e.target.checked }))}
                />
                Tắt âm thanh thông báo của ứng dụng
              </label>
              <p style={{ margin: "8px 0 10px", fontSize: 11.5, color: "var(--bch-muted)", lineHeight: 1.5 }}>
                Mỗi tài khoản có công tắc thông báo riêng (nút 🔔 trên thẻ tài khoản). Ứng dụng chỉ đọc
                số tin chưa đọc và cuộc gọi đến — không lưu nội dung tin nhắn.
              </p>
              <button type="button" className="bch-btn bch-btn-success" onClick={enableAllNotifications}>
                🔔 Bật thông báo cho tất cả tài khoản
              </button>
            </div>

            <div className="bch-section-card">
              <div className="bch-section-label" style={{ marginBottom: 9 }}>
                Hiệu năng &amp; dữ liệu
              </div>
              <p style={{ margin: "0 0 10px", fontSize: 11.5, color: "var(--bch-muted)", lineHeight: 1.5 }}>
                Phiên đăng nhập của từng tài khoản nằm trong thư mục{" "}
                <code>chat_profiles/&lt;nền tảng&gt;/&lt;mã tài khoản&gt;</code>.
              </p>
              <div className="bch-form-group">
                <label className="bch-label">Số khung chat giữ chạy đồng thời</label>
                <select
                  className="bch-select"
                  value={settings.maxLiveWebviews}
                  onChange={(e) =>
                    setSettings((prev) => ({
                      ...prev,
                      maxLiveWebviews: Number(e.target.value),
                    }))
                  }
                >
                  <option value={1}>1 — tiết kiệm RAM tối đa</option>
                  <option value={2}>2</option>
                  <option value={3}>3 — cân bằng (mặc định)</option>
                  <option value={4}>4</option>
                  <option value={5}>5</option>
                  <option value={0}>Không giới hạn (tốn RAM nhất)</option>
                </select>
                <p style={{ margin: "6px 0 0", fontSize: 11, color: "var(--bch-muted)", lineHeight: 1.45 }}>
                  Khi mở nhiều tài khoản hơn mức này, khung chat của tài khoản ít dùng nhất sẽ
                  tự đóng để giải phóng RAM. Phiên đăng nhập vẫn được giữ trên ổ đĩa nên mở
                  lại không phải đăng nhập lại; đổi lại, tài khoản đã đóng sẽ không báo tin
                  nhắn mới cho tới khi bạn mở lại.
                </p>
              </div>
              <div className="bch-footer-row">
                <button
                  type="button"
                  className="bch-btn"
                  onClick={async () => {
                    const msg = await callTauri<string>("optimize_ram_now");
                    flashStatus(msg || "Không chạy được tối ưu RAM");
                  }}
                >
                  🧠 Giải phóng RAM
                </button>
                <button
                  type="button"
                  className="bch-btn"
                  onClick={async () => {
                    const dir = await callTauri<string>("open_app_data_folder");
                    flashStatus(dir ? `📂 Đã mở ${dir}` : "Không mở được thư mục dữ liệu");
                  }}
                >
                  📂 Mở thư mục dữ liệu
                </button>
              </div>
            </div>

            <div className="bch-alert-box bch-alert-info">
              Đã loại bỏ toàn bộ tính năng sao lưu / bơm lịch sử Zalo vào web chat: mỗi tài khoản
              hoạt động như một trình duyệt riêng, dữ liệu tin nhắn do chính website quản lý.
            </div>

            <div style={{ display: "flex", justifyContent: "space-between", gap: 8 }}>
              <button type="button" className="bch-btn bch-btn-danger" onClick={exitApp}>
                ⏻ Thoát ứng dụng
              </button>
              <button type="button" className="bch-btn bch-btn-primary" onClick={() => setShowSettings(false)}>
                Đóng
              </button>
            </div>
          </div>
        </div>
      )}

      {showGroupManager && (
        <div
          className="bch-modal-overlay"
          style={{ left: modalLeft }}
          onClick={(e) => {
            if (e.target === e.currentTarget) setShowGroupManager(false);
          }}
        >
          <div className="bch-modal-card" style={{ maxWidth: 520 }}>
            <div className="bch-modal-header">
              <div>
                <h2 style={{ margin: 0, fontSize: 16, color: "var(--bch-text)" }}>Quản lý nhóm người dùng</h2>
                <p style={{ margin: "4px 0 0", fontSize: 11.5, color: "var(--bch-muted)" }}>
                  Tạo nhóm riêng để gom tài khoản theo ý bạn (không phụ thuộc trang web).
                </p>
              </div>
              <button type="button" className="bch-toast-close" onClick={() => setShowGroupManager(false)}>
                ✕
              </button>
            </div>

            <div className="bch-form-group">
              <label className="bch-label">{editingGroupId ? "Sửa nhóm" : "Tạo nhóm mới"}</label>
              <div style={{ display: "flex", gap: 8 }}>
                <input
                  className="bch-input"
                  value={newGroupName}
                  onChange={(e) => setNewGroupName(e.target.value)}
                  placeholder="Tên nhóm, ví dụ: Công việc, Cá nhân..."
                  autoFocus
                  style={{ flex: 1 }}
                />
                <input
                  className="bch-input"
                  type="color"
                  value={newGroupColor}
                  onChange={(e) => setNewGroupColor(e.target.value)}
                  style={{ width: 56, padding: 4 }}
                />
                <button type="button" className="bch-btn bch-btn-primary" onClick={createGroup}>
                  {editingGroupId ? "Lưu" : "Thêm"}
                </button>
                {editingGroupId && (
                  <button
                    type="button"
                    className="bch-btn"
                    onClick={() => {
                      setEditingGroupId(null);
                      setNewGroupName("");
                      setNewGroupColor("#6366F1");
                    }}
                  >
                    Hủy
                  </button>
                )}
              </div>
            </div>

            <div className="bch-accounts-container" style={{ maxHeight: 260 }}>
              {groups.length === 0 && (
                <div className="bch-alert-box bch-alert-info">Chưa có nhóm nào.</div>
              )}
              {groups.map((g) => (
                <div
                  key={g.id}
                  style={{
                    display: "flex",
                    alignItems: "center",
                    justifyContent: "space-between",
                    gap: 8,
                    padding: "6px 8px",
                    borderBottom: "1px solid #1e293b",
                  }}
                >
                  <span style={{ display: "inline-flex", alignItems: "center", gap: 8, color: "var(--bch-text-soft)", fontSize: 12.5 }}>
                    <span
                      style={{
                        width: 10,
                        height: 10,
                        borderRadius: 999,
                        backgroundColor: g.color,
                        display: "inline-block",
                      }}
                    />
                    {g.name}
                    <span style={{ color: "var(--bch-muted)", fontSize: 11 }}>
                      ({accounts.filter((a) => (a.groupIds || []).includes(g.id)).length} tài khoản)
                    </span>
                  </span>
                  <div style={{ display: "flex", gap: 6 }}>
                    <button
                      type="button"
                      className="bch-btn"
                      style={{ padding: "2px 8px", fontSize: 11 }}
                      onClick={() => {
                        setEditingGroupId(g.id);
                        setNewGroupName(g.name);
                        setNewGroupColor(g.color);
                      }}
                    >
                      ✎
                    </button>
                    <button
                      type="button"
                      className="bch-btn bch-btn-danger"
                      style={{ padding: "2px 8px", fontSize: 11 }}
                      onClick={() => deleteGroup(g)}
                    >
                      🗑
                    </button>
                  </div>
                </div>
              ))}
            </div>
          </div>
        </div>
      )}

      {showInbox && (
        <div
          className="bch-inbox-overlay"
          style={{ left: modalLeft }}
          onClick={(e) => {
            if (!IS_NOTIF_WINDOW && e.target === e.currentTarget) closeInboxPanel();
          }}
        >
          <div className="bch-inbox-card">
            <div className="bch-inbox-head" data-tauri-drag-region>
              <div>
                <h2 className="bch-inbox-heading">Trung tâm thông báo</h2>
                <p className="bch-inbox-sub">
                  {notifications.length} thông báo • tin mới được lấy từ tiêu đề khung chat của
                  từng tài khoản
                </p>
              </div>
              <div style={{ display: "flex", gap: 6, alignItems: "center" }}>
                {notifications.length > 0 && (
                  <button
                    type="button"
                    className="bch-btn"
                    onClick={() => setNotifications([])}
                    title="Xoá toàn bộ danh sách thông báo"
                  >
                    🗑 Xoá hết
                  </button>
                )}
                <button
                  type="button"
                  className="bch-toast-close"
                  onClick={closeInboxPanel}
                  aria-label="Đóng"
                  title={IS_NOTIF_WINDOW ? "Ẩn bảng thông báo" : "Đóng"}
                >
                  ✕
                </button>
              </div>
            </div>

            <div className="bch-inbox-list">
              {notifications.length === 0 && (
                <div className="bch-inbox-empty">
                  Chưa có thông báo nào. Khi một tài khoản nhận tin nhắn mới (hoặc có cuộc gọi
                  đến), thông báo sẽ xuất hiện ở đây để bạn đọc và trả lời ngay.
                </div>
              )}

              {notifications.map((item) => {
                const acc = accounts.find((a) => a.id === item.profileId);
                const plat = acc ? getPlatform(acc.platformId) : null;
                const replying = replyTargetId === item.id;
                return (
                  <div
                    key={item.id}
                    className={`bch-inbox-item ${item.isCall ? "is-call" : ""}`}
                  >
                    <div className="bch-inbox-item-top">
                      <span
                        className="bch-inbox-badge"
                        style={{ backgroundColor: plat?.color || "#475569" }}
                      >
                        {plat?.badge || "?"}
                      </span>
                      <div className="bch-inbox-meta">
                        <div className="bch-inbox-name">
                          {acc?.name || item.profileId}
                          {plat ? ` • ${plat.name}` : ""}
                        </div>
                        <div className="bch-inbox-time">
                          {new Date(item.at).toLocaleString("vi-VN")}
                          {item.unread > 0 ? ` • ${item.unread} tin chưa đọc` : ""}
                        </div>
                      </div>
                      <button
                        type="button"
                        className="bch-link-btn"
                        onClick={() => {
                          if (IS_NOTIF_WINDOW) {
                            void callTauri("open_profile_from_notification", {
                              profileId: item.profileId,
                            });
                          } else if (acc) {
                            selectAccount(acc);
                          }
                        }}
                      >
                        Mở
                      </button>
                      <button
                        type="button"
                        className="bch-inbox-del"
                        onClick={() =>
                          setNotifications((prev) => prev.filter((n) => n.id !== item.id))
                        }
                        title="Xoá thông báo này"
                        aria-label="Xoá thông báo này"
                      >
                        ✕
                      </button>
                    </div>

                    <div className="bch-inbox-title">{item.title}</div>
                    {item.body && <div className="bch-inbox-body">{item.body}</div>}

                    {replying ? (
                      <div className="bch-inbox-reply">
                        <textarea
                          className="bch-input bch-inbox-textarea"
                          rows={2}
                          autoFocus
                          value={replyText}
                          placeholder={`Trả lời ${acc?.name || "tài khoản"}...`}
                          onChange={(e) => setReplyText(e.target.value)}
                        />
                        <div className="bch-inbox-reply-actions">
                          <button
                            type="button"
                            className="bch-btn bch-btn-primary"
                            disabled={replySending || !replyText.trim()}
                            onClick={() => void sendReply(item, true)}
                          >
                            {replySending ? "Đang gửi..." : "📤 Gửi"}
                          </button>
                          <button
                            type="button"
                            className="bch-btn"
                            disabled={replySending || !replyText.trim()}
                            onClick={() => void sendReply(item, false)}
                            title="Chỉ điền vào ô chat để bạn xem lại trước khi gửi"
                          >
                            📝 Chỉ điền
                          </button>
                          <button
                            type="button"
                            className="bch-btn"
                            onClick={() => {
                              setReplyTargetId(null);
                              setReplyText("");
                            }}
                          >
                            Huỷ
                          </button>
                        </div>
                      </div>
                    ) : (
                      <div className="bch-inbox-actions">
                        <button
                          type="button"
                          className="bch-btn"
                          onClick={() => {
                            setReplyTargetId(item.id);
                            setReplyText("");
                          }}
                        >
                          ↩ Trả lời
                        </button>
                      </div>
                    )}
                  </div>
                );
              })}
            </div>
          </div>
        </div>
      )}

      {deleteTarget && (
        <div
          className="bch-modal-overlay"
          style={{ left: modalLeft }}
          onClick={(e) => {
            if (e.target === e.currentTarget) setDeleteTarget(null);
          }}
        >
          <div className="bch-modal-card" style={{ maxWidth: 460 }}>
            <div className="bch-modal-header">
              <h2 style={{ margin: 0, fontSize: 16, color: "var(--bch-text)" }}>Xóa tài khoản?</h2>
              <button type="button" className="bch-toast-close" onClick={() => setDeleteTarget(null)}>
                ✕
              </button>
            </div>
            <div className="bch-alert-box bch-alert-warning">
              Tài khoản <strong>{deleteTarget.name}</strong> sẽ bị xóa khỏi workspace và toàn bộ dữ
              liệu đăng nhập (cookie, cache) của profile này trên ổ cứng sẽ bị xóa. Hành động không
              thể hoàn tác.
            </div>
            <div style={{ display: "flex", gap: 8, justifyContent: "flex-end" }}>
              <button type="button" className="bch-btn" onClick={() => setDeleteTarget(null)}>
                Hủy
              </button>
              <button type="button" className="bch-btn bch-btn-danger" onClick={confirmDeleteAccount}>
                🗑 Xóa vĩnh viễn
              </button>
            </div>
          </div>
        </div>
      )}
    </div>
  );
}
