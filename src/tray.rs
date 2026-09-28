use tauri::{
    AppHandle, CustomMenuItem, GlobalShortcutManager, Manager, SystemTray, SystemTrayEvent,
    SystemTrayMenu, SystemTrayMenuItem,
};

pub fn create_system_tray() -> SystemTray {
    let show = CustomMenuItem::new("show".to_string(), "显示");
    let start = CustomMenuItem::new("start".to_string(), "开始");
    let stop = CustomMenuItem::new("stop".to_string(), "停止");
    let through = CustomMenuItem::new("through".to_string(), "切换点击穿透");
    let config_dir = CustomMenuItem::new("config_dir".to_string(), "打开配置目录");
    let quit = CustomMenuItem::new("quit".to_string(), "退出");

    let tray_menu = SystemTrayMenu::new()
        .add_item(show)
        .add_item(start)
        .add_item(stop)
        .add_native_item(SystemTrayMenuItem::Separator)
        .add_item(through)
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
                if let Some(window) = app.get_window("main") {
                    let _ = window.emit("tray://command", "start");
                }
            }
            "stop" => {
                if let Some(window) = app.get_window("main") {
                    let _ = window.emit("tray://command", "stop");
                }
            }
            "through" => {
                // 关闭点击穿透的唯一可靠入口：穿透开启后窗口不再接收鼠标事件
                crate::toggle_click_through(app);
            }
            "config_dir" => {
                if let Err(e) = crate::open_config_dir() {
                    tracing::error!("Failed to open config dir: {}", e);
                }
            }
            "quit" => {
                app.exit(0);
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
        let _ = window.show();
        let _ = window.set_focus();
    }
}

pub fn register_global_shortcuts(app: &AppHandle) -> tauri::Result<()> {
    let mut manager = app.global_shortcut_manager();

    let app_handle = app.clone();
    manager.register("CmdOrCtrl+Alt+S", move || {
        show_main_window(&app_handle);
    })?;

    let app_handle = app.clone();
    manager.register("CmdOrCtrl+Alt+R", move || {
        if let Some(window) = app_handle.get_window("main") {
            let _ = window.emit("shortcut://command", "start");
        }
    })?;

    let app_handle = app.clone();
    manager.register("CmdOrCtrl+Alt+E", move || {
        if let Some(window) = app_handle.get_window("main") {
            let _ = window.emit("shortcut://command", "stop");
        }
    })?;

    // 切换点击穿透（穿透开启后这是关闭它的快捷键）
    let app_handle = app.clone();
    manager.register("CmdOrCtrl+Alt+T", move || {
        crate::toggle_click_through(&app_handle);
    })?;

    let app_handle = app.clone();
    manager.register("CmdOrCtrl+Alt+Q", move || {
        app_handle.exit(0);
    })?;

    tracing::info!("Global shortcuts registered successfully");
    Ok(())
}
