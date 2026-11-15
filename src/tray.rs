use crate::config::HotkeyConfig;
use crate::hotkeys::{HotkeyAction, HotkeyStatusItem};
use tauri::{
    AppHandle, CustomMenuItem, GlobalShortcutManager, Manager, SystemTray, SystemTrayEvent,
    SystemTrayMenu, SystemTrayMenuItem,
};

pub fn create_system_tray() -> SystemTray {
    let show = CustomMenuItem::new("show".to_string(), "显示");
    let start = CustomMenuItem::new("start".to_string(), "开始");
    let stop = CustomMenuItem::new("stop".to_string(), "停止");
    let through = CustomMenuItem::new("through".to_string(), "切换点击穿透");
    let settings = CustomMenuItem::new("settings".to_string(), "设置…");
    let history = CustomMenuItem::new("history".to_string(), "翻译历史…");
    let config_dir = CustomMenuItem::new("config_dir".to_string(), "打开配置目录");
    let quit = CustomMenuItem::new("quit".to_string(), "退出");

    let tray_menu = SystemTrayMenu::new()
        .add_item(show)
        .add_item(start)
        .add_item(stop)
        .add_native_item(SystemTrayMenuItem::Separator)
        .add_item(through)
        .add_item(settings)
        .add_item(history)
        .add_item(config_dir)
        .add_native_item(SystemTrayMenuItem::Separator)
        .add_item(quit);

    SystemTray::new().with_menu(tray_menu)
}

pub fn handle_system_tray_event(app: &AppHandle, event: SystemTrayEvent) {
    match event {
        SystemTrayEvent::MenuItemClick { id, .. } => match id.as_str() {
            "show" => {
                show_main_window(app);
            }
            "start" => {
                crate::start_capture_bg(app);
            }
            "stop" => {
                crate::stop_capture_bg(app);
            }
            "through" => {
                // 关闭点击穿透的唯一可靠入口：穿透开启后窗口不再接收鼠标事件
                crate::toggle_click_through(app);
            }
            "settings" => {
                crate::open_settings_window(app, None);
            }
            "history" => {
                crate::open_history_window(app);
            }
            "config_dir" => {
                if let Err(e) = crate::open_config_dir(app) {
                    tracing::error!("Failed to open config dir: {}", e);
                }
            }
            "quit" => {
                crate::quit_app(app);
            }
            _ => {}
        },
        SystemTrayEvent::LeftClick { .. } => {
            if let Some(window) = app.get_window("main") {
                let is_visible = window.is_visible().unwrap_or(false);
                if is_visible {
                    let _ = window.hide();
                } else {
                    show_main_window(app);
                }
            }
        }
        SystemTrayEvent::DoubleClick { .. } => {
            show_main_window(app);
        }
        _ => {}
    }
}

fn show_main_window(app: &AppHandle) {
    if let Some(window) = app.get_window("main") {
        let _ = window.unminimize();
        let _ = window.show();
        let _ = window.set_focus();
    }
}

/// 暂停全部全局快捷键（录制前调用，含 quit）。
/// `unregister_all` 失败只 warn，不中断。
pub fn pause_global_hotkeys(app: &AppHandle) {
    let mut manager = app.global_shortcut_manager();
    if let Err(e) = manager.unregister_all() {
        tracing::warn!("unregister_all 失败（继续暂停）: {e}");
    }
    tracing::info!("Global shortcuts paused for hotkey recording");
}

/// 按配置注册全局快捷键。单项失败不阻断其余项。
pub fn apply_hotkeys(app: &AppHandle, config: &HotkeyConfig) -> Vec<HotkeyStatusItem> {
    let mut manager = app.global_shortcut_manager();
    if let Err(e) = manager.unregister_all() {
        tracing::warn!("unregister_all 失败（继续重注册）: {e}");
    }

    let mut status = Vec::with_capacity(HotkeyAction::ALL.len());
    for action in HotkeyAction::ALL {
        let accelerator = action.accelerator(config).trim().to_string();
        if accelerator.is_empty() {
            status.push(HotkeyStatusItem::skipped_empty(action));
            continue;
        }

        let app_handle = app.clone();
        let handler: Box<dyn Fn() + Send + Sync> = match action {
            HotkeyAction::Show => Box::new(move || show_main_window(&app_handle)),
            HotkeyAction::Start => Box::new(move || crate::start_capture_bg(&app_handle)),
            HotkeyAction::Stop => Box::new(move || crate::stop_capture_bg(&app_handle)),
            HotkeyAction::ToggleCapture => Box::new(move || crate::toggle_capture_bg(&app_handle)),
            HotkeyAction::ToggleClickThrough => {
                Box::new(move || crate::toggle_click_through(&app_handle))
            }
            HotkeyAction::OpenSettings => {
                Box::new(move || crate::open_settings_window(&app_handle, None))
            }
            HotkeyAction::OpenHistory => Box::new(move || crate::open_history_window(&app_handle)),
            HotkeyAction::ToggleTranslate => {
                Box::new(move || crate::toggle_translate_bg(&app_handle))
            }
            HotkeyAction::ToggleSteamvrLock => {
                Box::new(move || crate::toggle_steamvr_lock_bg(&app_handle))
            }
            HotkeyAction::Quit => Box::new(move || crate::quit_app(&app_handle)),
        };

        match manager.register(&accelerator, handler) {
            Ok(()) => status.push(HotkeyStatusItem {
                action: action.id().to_string(),
                label: action.label().to_string(),
                accelerator,
                registered: true,
                error: None,
            }),
            Err(e) => {
                let msg = e.to_string();
                tracing::warn!("快捷键 {accelerator} ({}) 注册失败: {msg}", action.id());
                status.push(HotkeyStatusItem {
                    action: action.id().to_string(),
                    label: action.label().to_string(),
                    accelerator,
                    registered: false,
                    error: Some(msg),
                });
            }
        }
    }

    tracing::info!("Global shortcuts registration finished");
    status
}
