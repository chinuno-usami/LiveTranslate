//! 全局快捷键：动作枚举、状态项、与配置字段的映射。
//! 实际 register/unregister 由 `tray` 执行，避免与 Tauri AppHandle 循环依赖。

use crate::config::HotkeyConfig;
use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HotkeyAction {
    Show,
    Start,
    Stop,
    ToggleCapture,
    ToggleClickThrough,
    OpenSettings,
    OpenHistory,
    ToggleTranslate,
    ToggleSteamvrLock,
    Quit,
}

impl HotkeyAction {
    pub const ALL: [Self; 10] = [
        Self::Show,
        Self::Start,
        Self::Stop,
        Self::ToggleCapture,
        Self::ToggleClickThrough,
        Self::OpenSettings,
        Self::OpenHistory,
        Self::ToggleTranslate,
        Self::ToggleSteamvrLock,
        Self::Quit,
    ];

    pub fn id(self) -> &'static str {
        match self {
            Self::Show => "show",
            Self::Start => "start",
            Self::Stop => "stop",
            Self::ToggleCapture => "toggle_capture",
            Self::ToggleClickThrough => "toggle_click_through",
            Self::OpenSettings => "open_settings",
            Self::OpenHistory => "open_history",
            Self::ToggleTranslate => "toggle_translate",
            Self::ToggleSteamvrLock => "toggle_steamvr_lock",
            Self::Quit => "quit",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Show => "显示浮窗",
            Self::Start => "开始采集",
            Self::Stop => "停止采集",
            Self::ToggleCapture => "切换采集",
            Self::ToggleClickThrough => "切换点击穿透",
            Self::OpenSettings => "打开设置",
            Self::OpenHistory => "打开历史",
            Self::ToggleTranslate => "切换翻译",
            Self::ToggleSteamvrLock => "切换 VR 位置锁定",
            Self::Quit => "退出",
        }
    }

    pub fn accelerator(self, config: &HotkeyConfig) -> &str {
        match self {
            Self::Show => &config.show,
            Self::Start => &config.start,
            Self::Stop => &config.stop,
            Self::ToggleCapture => &config.toggle_capture,
            Self::ToggleClickThrough => &config.toggle_click_through,
            Self::OpenSettings => &config.open_settings,
            Self::OpenHistory => &config.open_history,
            Self::ToggleTranslate => &config.toggle_translate,
            Self::ToggleSteamvrLock => &config.toggle_steamvr_lock,
            Self::Quit => &config.quit,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct HotkeyStatusItem {
    pub action: String,
    pub label: String,
    pub accelerator: String,
    pub registered: bool,
    pub error: Option<String>,
}

impl HotkeyStatusItem {
    pub fn from_action(action: HotkeyAction, config: &HotkeyConfig) -> Self {
        let accelerator = action.accelerator(config).trim().to_string();
        Self {
            action: action.id().to_string(),
            label: action.label().to_string(),
            accelerator,
            registered: false,
            error: None,
        }
    }

    pub fn skipped_empty(action: HotkeyAction) -> Self {
        Self {
            action: action.id().to_string(),
            label: action.label().to_string(),
            accelerator: String::new(),
            registered: false,
            error: None,
        }
    }
}

/// 根据当前配置生成「尚未注册」的状态快照（暂停或启动前）。
pub fn pending_status(config: &HotkeyConfig) -> Vec<HotkeyStatusItem> {
    HotkeyAction::ALL
        .iter()
        .copied()
        .map(|action| {
            let acc = action.accelerator(config).trim();
            if acc.is_empty() {
                HotkeyStatusItem::skipped_empty(action)
            } else {
                HotkeyStatusItem::from_action(action, config)
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::HotkeyConfig;

    #[test]
    fn pending_status_skips_empty_accelerators() {
        let cfg = HotkeyConfig {
            open_history: String::new(),
            toggle_capture: String::new(),
            ..Default::default()
        };
        let items = pending_status(&cfg);
        assert_eq!(items.len(), HotkeyAction::ALL.len());
        let history = items.iter().find(|i| i.action == "open_history").unwrap();
        assert!(history.accelerator.is_empty());
        assert!(!history.registered);
        let show = items.iter().find(|i| i.action == "show").unwrap();
        assert_eq!(show.accelerator, "CmdOrCtrl+Alt+S");
        assert!(!show.registered);
    }

    #[test]
    fn action_ids_match_config_entries() {
        let cfg = HotkeyConfig::default();
        let entry_ids: Vec<&str> = cfg.entries().iter().map(|(id, _, _)| *id).collect();
        let action_ids: Vec<&str> = HotkeyAction::ALL.iter().map(|a| a.id()).collect();
        assert_eq!(entry_ids, action_ids);
    }
}
