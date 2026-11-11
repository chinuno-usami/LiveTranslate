//! OSC → VRChat Chatbox 输出。
//!
//! 纯函数部分（组包 / 截断 / 限流判定）不依赖 feature，方便单测；
//! 真正的 UDP 发送在 `imp` 中，未启用 `osc` feature 时替换为 stub。
#![cfg_attr(not(feature = "osc"), allow(dead_code))]

use crate::config::OscConfig;
use crate::subtitle::DisplayLine;
use serde::Serialize;
use std::collections::VecDeque;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OscContent {
    Translated,
    Source,
    Both,
}

impl OscContent {
    fn parse(value: &str) -> Self {
        match value {
            "source" => OscContent::Source,
            "both" => OscContent::Both,
            _ => OscContent::Translated,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OscMode {
    Rolling,
    Latest,
}

impl OscMode {
    fn parse(value: &str) -> Self {
        if value == "latest" {
            OscMode::Latest
        } else {
            OscMode::Rolling
        }
    }
}

/// 组包参数（从配置归一化而来，便于单测）
#[derive(Debug, Clone)]
pub struct PackOptions {
    pub content: OscContent,
    pub mode: OscMode,
    pub max_lines: usize,
    pub max_chars: usize,
    pub separator: String,
}

impl PackOptions {
    pub fn from_config(cfg: &OscConfig) -> Self {
        Self {
            content: OscContent::parse(&cfg.content),
            mode: OscMode::parse(&cfg.mode),
            max_lines: cfg.max_lines.clamp(1, 9),
            max_chars: cfg.max_chars.clamp(8, 144),
            // 换行在部分客户端会被过滤，允许配置成 " | "
            separator: if cfg.separator.is_empty() {
                "\n".to_string()
            } else {
                cfg.separator.clone()
            },
        }
    }
}

/// 展平单条内容里的换行/连续空白（Chatbox 中单条内部不应有换行）
pub fn flatten(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// 规范化单条字幕：`[来源] 译文`（内容里不保留换行）
pub fn format_entry(line: &DisplayLine, content: OscContent) -> String {
    let source = flatten(&line.source);
    let translated = flatten(&line.translated);
    let body = match content {
        OscContent::Translated => {
            if translated.is_empty() {
                source
            } else {
                translated
            }
        }
        OscContent::Source => source,
        OscContent::Both => match (source.is_empty(), translated.is_empty()) {
            (true, true) => String::new(),
            (true, false) => translated,
            (false, true) => source,
            (false, false) => format!("{source} | {translated}"),
        },
    };
    if body.is_empty() {
        return String::new();
    }
    match &line.label {
        Some(label) if !label.trim().is_empty() => format!("[{label}] {body}"),
        _ => body,
    }
}

/// 按 Unicode 字符截断，超长时保留头部并加 `…`
pub fn truncate_chars(text: &str, max_chars: usize) -> String {
    if max_chars == 0 {
        return String::new();
    }
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    let mut out: String = text.chars().take(max_chars.saturating_sub(1)).collect();
    out.push('…');
    out
}

/// 把最近多条字幕合并成滚动字幕。
///
/// - `history` 按时间从旧到新
/// - 从最新往旧累加，超过字符预算时丢弃更早的条目
/// - 最新一条自身超长时只截断这一条
pub fn pack_rolling(history: &VecDeque<String>, opts: &PackOptions) -> Option<String> {
    if history.is_empty() {
        return None;
    }

    let mut selected: Vec<&String> = Vec::new();
    let mut total = 0usize;
    let separator_len = opts.separator.chars().count();

    for entry in history.iter().rev() {
        if entry.is_empty() {
            continue;
        }
        if selected.len() >= opts.max_lines {
            break;
        }
        let entry_len = entry.chars().count();
        let added = if selected.is_empty() {
            entry_len
        } else {
            entry_len + separator_len
        };
        if total + added > opts.max_chars {
            if selected.is_empty() {
                // 最新一条自身就超长：只截断这一条
                return Some(truncate_chars(entry, opts.max_chars));
            }
            // 放不下的更早消息直接丢弃
            break;
        }
        total += added;
        selected.push(entry);
    }

    if selected.is_empty() {
        return None;
    }
    selected.reverse();
    let joined = selected
        .iter()
        .map(|s| s.as_str())
        .collect::<Vec<_>>()
        .join(&opts.separator);
    Some(truncate_chars(&joined, opts.max_chars))
}

/// `latest` 模式：只发送最新一条
pub fn pack_latest(history: &VecDeque<String>, opts: &PackOptions) -> Option<String> {
    history
        .iter()
        .rev()
        .find(|entry| !entry.is_empty())
        .map(|entry| truncate_chars(entry, opts.max_chars))
}

pub fn pack(history: &VecDeque<String>, opts: &PackOptions) -> Option<String> {
    match opts.mode {
        OscMode::Rolling => pack_rolling(history, opts),
        OscMode::Latest => pack_latest(history, opts),
    }
}

/// 5 秒滑动窗口内是否还有发送配额
pub fn has_window_quota(sent_at: &VecDeque<std::time::Instant>, max_per_window: usize) -> bool {
    let cutoff = std::time::Instant::now() - std::time::Duration::from_secs(5);
    sent_at.iter().filter(|t| **t >= cutoff).count() < max_per_window
}

#[derive(Debug, Clone, Serialize)]
pub struct OscStatus {
    pub enabled: bool,
    pub available: bool,
    pub last_sent_ms: u64,
    pub message: String,
}

#[allow(clippy::derivable_impls)]
impl Default for OscStatus {
    fn default() -> Self {
        Self {
            enabled: false,
            available: cfg!(feature = "osc"),
            last_sent_ms: 0,
            message: String::new(),
        }
    }
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(feature = "osc")]
mod imp {
    use super::*;
    use rosc::{OscMessage, OscPacket, OscType};
    use std::net::{SocketAddr, UdpSocket};
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
    use std::sync::{Arc, Mutex};
    use std::thread::JoinHandle;
    use std::time::{Duration, Instant};

    struct Shared {
        config: Mutex<OscConfig>,
        /// 最新一份完整字幕快照（覆盖旧值，不做队列）
        pending: Mutex<Option<Vec<DisplayLine>>>,
        status: Mutex<OscStatus>,
        stop: AtomicBool,
        /// 最近一次字幕活动时间（epoch ms），用于 typing 指示
        activity_ms: AtomicU64,
        /// 配置变更后请求清空去重状态（否则停止再开启时相同文本永不发送）
        reset_dedup: AtomicBool,
        /// 请求发送一次空文本（Chatbox 是有状态的，停止后要主动清屏）
        pending_clear: AtomicBool,
    }

    impl Shared {
        fn config(&self) -> OscConfig {
            self.config
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone()
        }

        fn set_status_message(&self, message: String) {
            let mut status = self.status.lock().unwrap_or_else(|e| e.into_inner());
            status.message = message;
        }
    }

    pub struct OscSink {
        shared: Arc<Shared>,
        handle: Mutex<Option<JoinHandle<()>>>,
    }

    impl OscSink {
        pub fn new(config: &OscConfig) -> Self {
            let shared = Arc::new(Shared {
                config: Mutex::new(config.clone()),
                pending: Mutex::new(None),
                status: Mutex::new(OscStatus {
                    enabled: config.enabled,
                    available: true,
                    last_sent_ms: 0,
                    message: if config.enabled {
                        "等待字幕".to_string()
                    } else {
                        "未启用".to_string()
                    },
                }),
                stop: AtomicBool::new(false),
                activity_ms: AtomicU64::new(0),
                reset_dedup: AtomicBool::new(false),
                pending_clear: AtomicBool::new(false),
            });

            let thread_shared = shared.clone();
            match std::thread::Builder::new()
                .name("osc-output".to_string())
                .spawn(move || run(thread_shared))
            {
                Ok(handle) => Self {
                    shared,
                    handle: Mutex::new(Some(handle)),
                },
                Err(e) => {
                    shared.set_status_message(format!("无法启动 OSC 线程: {e}"));
                    Self {
                        shared,
                        handle: Mutex::new(None),
                    }
                }
            }
        }

        pub fn apply_config(&self, config: &OscConfig) {
            let previous = self.shared.config();
            {
                let mut current = self.shared.config.lock().unwrap_or_else(|e| e.into_inner());
                *current = config.clone();
            }
            if previous.target != config.target
                || previous.content != config.content
                || previous.separator != config.separator
                || previous.mode != config.mode
                || previous.enabled != config.enabled
            {
                self.shared.reset_dedup.store(true, Ordering::SeqCst);
            }
            if previous.enabled && !config.enabled {
                self.shared.pending_clear.store(true, Ordering::SeqCst);
            }
            let mut status = self.shared.status.lock().unwrap_or_else(|e| e.into_inner());
            status.enabled = config.enabled;
            if !config.enabled {
                status.message = "未启用".to_string();
            }
        }

        /// 传入完整字幕快照；内部覆盖旧快照，限流期间只保留最新结果
        pub fn publish(&self, snapshot: &[DisplayLine]) {
            let mut pending = self
                .shared
                .pending
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            *pending = Some(snapshot.to_vec());
            drop(pending);
            self.shared.activity_ms.store(now_ms(), Ordering::SeqCst);
        }

        pub fn set_active(&self, active: bool) {
            if !active {
                self.shared.activity_ms.store(0, Ordering::SeqCst);
                self.shared.reset_dedup.store(true, Ordering::SeqCst);
                if let Ok(mut pending) = self.shared.pending.lock() {
                    *pending = Some(Vec::new());
                }
                // 空快照组包后是 None（什么都不发），Chatbox 会把最后一句留在画面里，
                // 所以额外标记一次「发空串」。
                self.shared.pending_clear.store(true, Ordering::SeqCst);
            }
        }

        pub fn status(&self) -> OscStatus {
            self.shared
                .status
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone()
        }

        pub fn shutdown(&self) {
            self.shared.stop.store(true, Ordering::SeqCst);
            if let Ok(mut handle) = self.handle.lock() {
                if let Some(handle) = handle.take() {
                    let _ = handle.join();
                }
            }
        }
    }

    /// 把 `host:port` 解析一次缓存起来，避免每个 UDP 包都做一次 DNS 查询
    fn resolve_target(target: &str) -> Result<SocketAddr, String> {
        use std::net::ToSocketAddrs;
        let trimmed = target.trim();
        if trimmed.is_empty() {
            return Err("目标地址为空".to_string());
        }
        trimmed
            .to_socket_addrs()
            .map_err(|e| format!("解析 {trimmed} 失败: {e}"))?
            .next()
            .ok_or_else(|| format!("解析 {trimmed} 得不到地址"))
    }

    /// 取缓存的目标地址；配置里的字符串变了才重解析
    fn cached_target(
        cache: &mut Option<(String, SocketAddr)>,
        configured: &str,
    ) -> Result<SocketAddr, String> {
        if let Some((source, addr)) = cache.as_ref() {
            if source == configured {
                return Ok(*addr);
            }
        }
        let addr = resolve_target(configured)?;
        *cache = Some((configured.to_string(), addr));
        Ok(addr)
    }

    fn run(shared: Arc<Shared>) {
        let socket = match UdpSocket::bind("0.0.0.0:0") {
            Ok(socket) => socket,
            Err(e) => {
                shared.set_status_message(format!("无法创建 UDP socket: {e}"));
                return;
            }
        };

        let mut history: VecDeque<String> = VecDeque::new();
        let mut pending_text: Option<String> = None;
        let mut latest_meta: Option<(String, String)> = None;
        let mut last_sent_text: Option<String> = None;
        let mut last_sent_at: Option<Instant> = None;
        let mut sent_at: VecDeque<Instant> = VecDeque::new();
        let mut typing = false;
        let mut last_typing_sent: Option<Instant> = None;
        // 解析好的目标地址：只在「配置里的字符串变了」时重解析，避免每包 DNS。
        // 只判断 `is_none()` 会让 output.osc.target 的改动永远不生效。
        let mut target: Option<(String, SocketAddr)> = None;
        let mut custom_target: Option<(String, SocketAddr)> = None;

        loop {
            if shared.stop.load(Ordering::SeqCst) {
                let config = shared.config();
                let addr = target.map(|(_, addr)| addr).or_else(|| {
                    if config.enabled {
                        resolve_target(&config.target).ok()
                    } else {
                        None
                    }
                });
                if let Some(addr) = addr {
                    if typing {
                        let _ = send_typing(&socket, addr, false);
                    }
                    let had_visible = last_sent_text
                        .as_deref()
                        .is_some_and(|text| !text.is_empty());
                    if shared.pending_clear.swap(false, Ordering::SeqCst) || had_visible {
                        let _ = send_chatbox(&socket, addr, "");
                    }
                }
                break;
            }

            let config = shared.config();

            if !config.enabled {
                let need_clear = shared.pending_clear.swap(false, Ordering::SeqCst);
                let had_visible = last_sent_text
                    .as_deref()
                    .is_some_and(|text| !text.is_empty());
                if shared.reset_dedup.swap(false, Ordering::SeqCst) {
                    last_sent_text = None;
                    pending_text = None;
                    history.clear();
                }
                if typing || need_clear || had_visible {
                    let addr = target
                        .as_ref()
                        .map(|(_, addr)| *addr)
                        .or_else(|| resolve_target(&config.target).ok());
                    if let Some(addr) = addr {
                        if typing {
                            let _ = send_typing(&socket, addr, false);
                        }
                        if need_clear || had_visible {
                            let _ = send_chatbox(&socket, addr, "");
                        }
                    }
                    typing = false;
                }
                std::thread::sleep(Duration::from_millis(100));
                continue;
            }

            if shared.reset_dedup.swap(false, Ordering::SeqCst) {
                last_sent_text = None;
                pending_text = None;
                history.clear();
            }

            let opts = PackOptions::from_config(&config);

            // 锁只覆盖 take()：后面的格式化/组包不持有锁
            let snapshot = shared
                .pending
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .take();
            if let Some(snapshot) = snapshot {
                {
                    // 重新构建历史（快照已经按浮窗 max_lines 裁剪）
                    history.clear();
                    for line in &snapshot {
                        let entry = format_entry(line, opts.content);
                        if entry.is_empty() {
                            continue;
                        }
                        history.push_back(entry);
                    }
                    while history.len() > opts.max_lines {
                        history.pop_front();
                    }
                    if let Some(last) = snapshot.last() {
                        latest_meta = Some((
                            last.source_id.clone(),
                            last.label.clone().unwrap_or_default(),
                        ));
                    }

                    match pack(&history, &opts) {
                        Some(text) if Some(&text) != last_sent_text.as_ref() => {
                            pending_text = Some(text);
                        }
                        _ => pending_text = None,
                    }
                }
            }

            // 停止采集后主动清屏：pack() 对空历史返回 None，只能显式发一个空串
            if shared.pending_clear.swap(false, Ordering::SeqCst) {
                last_sent_text = None;
                pending_text = Some(String::new());
            }

            // typing：有字幕活动后置 true，静音 3 秒后置 false
            let activity = shared.activity_ms.load(Ordering::SeqCst);
            let want_typing =
                config.send_typing && activity > 0 && now_ms().saturating_sub(activity) < 3_000;
            if want_typing != typing {
                let throttle_ok = last_typing_sent
                    .map(|t| t.elapsed() >= Duration::from_millis(500))
                    .unwrap_or(true);
                if throttle_ok {
                    match cached_target(&mut target, &config.target) {
                        Ok(addr) => {
                            if send_typing(&socket, addr, want_typing).is_ok() {
                                typing = want_typing;
                                last_typing_sent = Some(Instant::now());
                            }
                        }
                        Err(e) => shared.set_status_message(format!("OSC 目标无效: {e}")),
                    }
                }
            }

            // 限流：最小间隔 + 5 秒窗口配额
            let interval_ok = last_sent_at
                .map(|t| t.elapsed() >= Duration::from_millis(config.min_interval_ms.max(200)))
                .unwrap_or(true);
            let cutoff = Instant::now() - Duration::from_secs(5);
            sent_at.retain(|t| *t >= cutoff);
            let window_ok = has_window_quota(&sent_at, config.max_per_window.max(1));

            if let (Some(text), true, true) = (pending_text.clone(), interval_ok, window_ok) {
                let addr = match cached_target(&mut target, &config.target) {
                    Ok(addr) => Some(addr),
                    Err(e) => {
                        shared.set_status_message(format!("OSC 目标无效: {e}"));
                        None
                    }
                };
                let send_result = match addr {
                    Some(addr) => send_chatbox(&socket, addr, &text),
                    None => Err("目标地址无效".to_string()),
                };
                match send_result {
                    Ok(()) => {
                        last_sent_text = Some(text.clone());
                        pending_text = None;
                        last_sent_at = Some(Instant::now());
                        sent_at.push_back(Instant::now());
                        if config.debug_log {
                            tracing::info!("OSC chatbox => {:?}", text);
                        }
                        {
                            let mut status =
                                shared.status.lock().unwrap_or_else(|e| e.into_inner());
                            status.enabled = true;
                            status.last_sent_ms = now_ms();
                            status.message = format!("已发送 {} 字符", text.chars().count());
                        }
                        let custom = config.custom_address.trim();
                        if !custom.is_empty() {
                            match cached_target(&mut custom_target, custom) {
                                Ok(addr) => {
                                    if let Some((source_id, label)) = &latest_meta {
                                        if let Err(e) =
                                            send_custom(&socket, addr, &text, source_id, label)
                                        {
                                            shared.set_status_message(format!(
                                                "自定义地址发送失败: {e}"
                                            ));
                                        }
                                    }
                                }
                                // 以前这里是 `Ok(addr)` 模式匹配，失败就静默跳过
                                Err(e) => shared.set_status_message(format!("自定义地址无效: {e}")),
                            }
                        }
                    }
                    Err(e) => {
                        // 也记一次时间戳：否则失败会每个 100ms tick 重试一次（含 DNS 解析）
                        last_sent_at = Some(Instant::now());
                        shared.set_status_message(format!("OSC 发送失败: {e}"));
                    }
                }
            }

            std::thread::sleep(Duration::from_millis(100));
        }
    }

    fn packet_bytes(packet: OscPacket) -> Result<Vec<u8>, String> {
        rosc::encoder::encode(&packet).map_err(|e| format!("OSC 编码失败: {e}"))
    }

    fn send_chatbox(socket: &UdpSocket, target: SocketAddr, text: &str) -> Result<(), String> {
        let packet = OscPacket::Message(OscMessage {
            addr: "/chatbox/input".to_string(),
            args: vec![
                OscType::String(text.to_string()),
                OscType::Bool(true),
                OscType::Bool(false),
            ],
        });
        let bytes = packet_bytes(packet)?;
        socket
            .send_to(&bytes, target)
            .map(|_| ())
            .map_err(|e| e.to_string())
    }

    fn send_typing(socket: &UdpSocket, target: SocketAddr, typing: bool) -> Result<(), String> {
        let packet = OscPacket::Message(OscMessage {
            addr: "/chatbox/typing".to_string(),
            args: vec![OscType::Bool(typing)],
        });
        let bytes = packet_bytes(packet)?;
        socket
            .send_to(&bytes, target)
            .map(|_| ())
            .map_err(|e| e.to_string())
    }

    fn send_custom(
        socket: &UdpSocket,
        target: SocketAddr,
        text: &str,
        source_id: &str,
        label: &str,
    ) -> Result<(), String> {
        let packet = OscPacket::Message(OscMessage {
            addr: "/livetranslate/subtitle".to_string(),
            args: vec![
                OscType::String(text.to_string()),
                OscType::String(source_id.to_string()),
                OscType::String(label.to_string()),
            ],
        });
        let bytes = packet_bytes(packet)?;
        socket
            .send_to(&bytes, target)
            .map(|_| ())
            .map_err(|e| e.to_string())
    }
}

#[cfg(not(feature = "osc"))]
mod imp {
    use super::*;

    pub struct OscSink {
        status: Mutex<OscStatus>,
    }

    use std::sync::Mutex;

    impl OscSink {
        pub fn new(config: &OscConfig) -> Self {
            Self {
                status: Mutex::new(OscStatus {
                    enabled: config.enabled,
                    available: false,
                    last_sent_ms: 0,
                    message: "当前构建未包含 OSC 支持（请下载 Full 版）".to_string(),
                }),
            }
        }

        pub fn apply_config(&self, config: &OscConfig) {
            if let Ok(mut status) = self.status.lock() {
                status.enabled = config.enabled;
            }
        }

        pub fn publish(&self, _snapshot: &[DisplayLine]) {}

        pub fn set_active(&self, _active: bool) {}

        pub fn status(&self) -> OscStatus {
            self.status.lock().map(|s| s.clone()).unwrap_or_default()
        }

        pub fn shutdown(&self) {}
    }
}

pub use imp::OscSink;

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn opts(mode: OscMode, max_lines: usize, max_chars: usize) -> PackOptions {
        PackOptions {
            content: OscContent::Translated,
            mode,
            max_lines,
            max_chars,
            separator: "\n".to_string(),
        }
    }

    fn line(label: Option<&str>, source: &str, translated: &str) -> DisplayLine {
        DisplayLine {
            source_id: "mic".into(),
            label: label.map(str::to_string),
            source: source.into(),
            translated: translated.into(),
            lines: Vec::new(),
            text: String::new(),
        }
    }

    #[test]
    fn format_entry_three_contents() {
        let entry = line(Some("麦克风"), "hello", "你好");
        assert_eq!(
            format_entry(&entry, OscContent::Translated),
            "[麦克风] 你好"
        );
        assert_eq!(format_entry(&entry, OscContent::Source), "[麦克风] hello");
        assert_eq!(
            format_entry(&entry, OscContent::Both),
            "[麦克风] hello | 你好"
        );
    }

    #[test]
    fn format_entry_falls_back_to_source() {
        let entry = line(None, "hello", "");
        assert_eq!(format_entry(&entry, OscContent::Translated), "hello");
    }

    #[test]
    fn format_entry_flattens_newlines() {
        let entry = line(None, "hello\n\nworld", "你\n好");
        assert_eq!(
            format_entry(&entry, OscContent::Both),
            "hello world | 你 好"
        );
    }

    #[test]
    fn rolling_keeps_fitting_lines() {
        let history: VecDeque<String> = ["a".repeat(10), "b".repeat(10), "c".repeat(10)].into();
        let result = pack_rolling(&history, &opts(OscMode::Rolling, 3, 100)).unwrap();
        assert_eq!(
            result,
            format!("{}\n{}\n{}", "a".repeat(10), "b".repeat(10), "c".repeat(10))
        );
    }

    #[test]
    fn rolling_drops_oldest_when_over_budget() {
        // 3 行，每行 10 字符 + 2 个分隔符 = 32 > 25（只容得下最新两条 21）
        let history: VecDeque<String> = ["a".repeat(10), "b".repeat(10), "c".repeat(10)].into();
        let result = pack_rolling(&history, &opts(OscMode::Rolling, 3, 25)).unwrap();
        assert_eq!(result, format!("{}\n{}", "b".repeat(10), "c".repeat(10)));
        assert!(!result.contains(&"a".repeat(10)));
    }

    #[test]
    fn rolling_respects_max_lines() {
        let history: VecDeque<String> =
            ["1", "2", "3", "4"].iter().map(|s| s.to_string()).collect();
        let result = pack_rolling(&history, &opts(OscMode::Rolling, 2, 144)).unwrap();
        assert_eq!(result, "3\n4");
    }

    #[test]
    fn rolling_truncates_single_overlong_line() {
        let history: VecDeque<String> = ["x".repeat(50)].into();
        let result = pack_rolling(&history, &opts(OscMode::Rolling, 4, 10)).unwrap();
        assert_eq!(result.chars().count(), 10);
        assert!(result.ends_with('…'));
    }

    #[test]
    fn unicode_truncation_is_safe() {
        let history: VecDeque<String> = ["你好世界🎉🎉🎉".to_string()].into();
        let result = pack_rolling(&history, &opts(OscMode::Rolling, 1, 5)).unwrap();
        assert_eq!(result.chars().count(), 5);
    }

    #[test]
    fn latest_mode_only_sends_last() {
        let history: VecDeque<String> = ["1", "2", "3"].iter().map(|s| s.to_string()).collect();
        let result = pack_latest(&history, &opts(OscMode::Latest, 4, 144)).unwrap();
        assert_eq!(result, "3");
    }

    #[test]
    fn separator_counts_toward_budget() {
        let mut o = opts(OscMode::Rolling, 4, 10);
        o.separator = " | ".to_string();
        let history: VecDeque<String> = ["12345".to_string(), "67890".to_string()].into();
        // 5 + 3 + 5 = 13 > 10，只能保留最新一条
        assert_eq!(pack_rolling(&history, &o).unwrap(), "67890");
    }

    #[test]
    fn window_quota_is_enforced() {
        let mut sent = VecDeque::new();
        assert!(has_window_quota(&sent, 2));
        sent.push_back(std::time::Instant::now());
        assert!(has_window_quota(&sent, 2));
        sent.push_back(std::time::Instant::now());
        assert!(!has_window_quota(&sent, 2));
        sent.push_back(std::time::Instant::now() - Duration::from_secs(6));
        assert!(!has_window_quota(&sent, 2));
    }
}
