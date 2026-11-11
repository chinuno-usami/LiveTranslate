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
    let config_dir = CustomMenuItem::new("config_dir".to_string(), "打开配置目录");
    let quit = CustomMenuItem::new("quit".to_string(), "退出");

    let tray_menu = SystemTrayMenu::new()
        .add_item(show)
        .add_item(start)
        .add_item(stop)
        .add_native_item(SystemTrayMenuItem::Separator)
        .add_item(through)
        .add_item(settings)
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

pub fn register_global_shortcuts(app: &AppHandle) -> tauri::Result<()> {
    let mut manager = app.global_shortcut_manager();

    // 逐个注册并各自记日志：一个组合被其它程序占用时，
    // 不能让排在它后面的快捷键（尤其是退出）一起失效。
    let mut register = |accelerator: &str, handler: Box<dyn Fn() + Send + Sync>| {
        if let Err(e) = manager.register(accelerator, handler) {
            tracing::warn!("快捷键 {accelerator} 注册失败（可能被其它程序占用）: {e}");
        }
    };

    let app_handle = app.clone();
    register(
        "CmdOrCtrl+Alt+S",
        Box::new(move || show_main_window(&app_handle)),
    );

    let app_handle = app.clone();
    register(
        "CmdOrCtrl+Alt+R",
        Box::new(move || crate::start_capture_bg(&app_handle)),
    );

    let app_handle = app.clone();
    register(
        "CmdOrCtrl+Alt+E",
        Box::new(move || crate::stop_capture_bg(&app_handle)),
    );

    // 切换点击穿透（穿透开启后这是关闭它的快捷键）
    let app_handle = app.clone();
    register(
        "CmdOrCtrl+Alt+T",
        Box::new(move || crate::toggle_click_through(&app_handle)),
    );

    let app_handle = app.clone();
    register(
        "CmdOrCtrl+Alt+Comma",
        Box::new(move || crate::open_settings_window(&app_handle, None)),
    );

    let app_handle = app.clone();
    register(
        "CmdOrCtrl+Alt+Q",
        Box::new(move || crate::quit_app(&app_handle)),
    );

    tracing::info!("Global shortcuts registration finished");
    Ok(())
}
