// Prevents additional console window on Windows in release
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use pc_guardian_lib::AppState;
use std::sync::atomic::Ordering;
use tauri::{Manager, Emitter, WindowEvent};
use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{TrayIconBuilder, TrayIconEvent, MouseButton};
use tauri_plugin_global_shortcut::GlobalShortcutExt;

fn main() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_notification::init())
        // V18: 重新启用全局快捷键，换用 Ctrl+Shift+Space
        .plugin(
            tauri_plugin_global_shortcut::Builder::new()
                .with_handler(|app, _shortcut, _event| {
                    let app_handle = app.clone();
                    tauri::async_runtime::spawn(async move {
                        if let Err(e) = app_handle.emit("global:toggle_mini", ()) {
                            eprintln!("[GlobalShortcut] emit failed: {}", e);
                        }
                        let _ = pc_guardian_lib::commands::toggle_mini_window_inner(&app_handle);
                    });
                })
                .build(),
        )
        .setup(|app| {
            // 初始化应用状态（数据库连接等）
            let state = AppState::new(app)?;
            app.manage(state);

            // V18: 启动 Proactive Engine（后台监控）
            {
                let state = app.state::<AppState>();
                let engine = state.proactive_engine.clone();
                tauri::async_runtime::spawn(async move {
                    engine.start();
                });
            }

            // V20: 初始化技能管理器（后台异步）
            {
                tauri::async_runtime::spawn(async move {
                    pc_guardian_lib::skills::init_global().await;
                    eprintln!("[Skills] 技能管理器初始化完成");
                });
            }

            // V18: 注册全局快捷键 Ctrl+Shift+Space
            let app_handle = app.handle().clone();
            if let Err(e) = register_global_shortcut(&app_handle) {
                eprintln!("[GlobalShortcut] 注册失败（不影响应用启动）: {}", e);
            }

            // V21: 创建系统托盘
            if let Err(e) = create_tray(app.handle()) {
                eprintln!("[Tray] 创建托盘失败（不影响应用启动）: {}", e);
            }

            Ok(())
        })
        // V21: 主窗口关闭按钮最小化到托盘
        .on_window_event(|window, event| {
            if let WindowEvent::CloseRequested { api, .. } = event {
                if window.label() == "main" {
                    let state = window.state::<AppState>();
                    if state.minimize_to_tray.load(Ordering::SeqCst) {
                        api.prevent_close();
                        let _ = window.hide();
                    }
                }
            }
        })
        // V21: 托盘菜单事件处理
        .on_menu_event(|app, event| {
            match event.id().as_ref() {
                "tray_show_main" => {
                    if let Some(window) = app.get_webview_window("main") {
                        let _ = window.show();
                        let _ = window.set_focus();
                    }
                }
                "tray_show_mini" => {
                    let _ = pc_guardian_lib::commands::toggle_mini_window_inner(app);
                }
                "tray_settings" => {
                    if let Some(window) = app.get_webview_window("main") {
                        let _ = window.show();
                        let _ = window.set_focus();
                        let _ = app.emit("navigate:settings", ());
                    }
                }
                _ => {}
            }
        })
        .invoke_handler(tauri::generate_handler![
            // 对话命令
            pc_guardian_lib::commands::create_conversation,
            pc_guardian_lib::commands::get_conversations,
            pc_guardian_lib::commands::get_messages,
            pc_guardian_lib::commands::delete_conversation,
            pc_guardian_lib::commands::send_message,
            // Phase 4: 权限确认
            pc_guardian_lib::commands::respond_permission,
            // Phase 4: 可信目录管理
            pc_guardian_lib::commands::get_allowed_directories,
            pc_guardian_lib::commands::add_allowed_directory,
            pc_guardian_lib::commands::remove_allowed_directory,
            // Phase 4: 信任工具管理
            pc_guardian_lib::commands::get_trusted_tools,
            pc_guardian_lib::commands::add_trusted_tool,
            pc_guardian_lib::commands::remove_trusted_tool,
            // Phase 4: 路径校验
            pc_guardian_lib::commands::validate_path_security,
            // Ollama 命令
            pc_guardian_lib::commands::check_ollama,
            pc_guardian_lib::commands::get_ollama_models,
            // 窗口管理命令
            pc_guardian_lib::commands::show_main_window,
            pc_guardian_lib::commands::close_mini_window,
            pc_guardian_lib::commands::is_mini_window,
            pc_guardian_lib::commands::toggle_mini_window,
            // 系统监控命令
            pc_guardian_lib::commands::get_system_metrics,
            // 设置管理命令
            pc_guardian_lib::commands::update_setting,
            // 审计日志命令
            pc_guardian_lib::commands::get_audit_logs,
            // 任务管理命令
            pc_guardian_lib::commands::get_tasks,
            pc_guardian_lib::commands::get_active_task,
            pc_guardian_lib::commands::cancel_task,
            // V17: 记忆管理命令
            pc_guardian_lib::commands::get_memories,
            pc_guardian_lib::commands::add_memory,
            pc_guardian_lib::commands::delete_memory,
            pc_guardian_lib::commands::search_memories,
            // V18: Proactive 主动助手命令
            pc_guardian_lib::commands::get_proactive_rules,
            pc_guardian_lib::commands::add_proactive_rule,
            pc_guardian_lib::commands::update_proactive_rule,
            pc_guardian_lib::commands::delete_proactive_rule,
            pc_guardian_lib::commands::toggle_proactive_rule,
            pc_guardian_lib::commands::get_proactive_status,
            // V20: 技能管理命令
            pc_guardian_lib::commands::get_skills,
            pc_guardian_lib::commands::get_skill_details,
            pc_guardian_lib::commands::toggle_skill,
            pc_guardian_lib::commands::install_skill,
            pc_guardian_lib::commands::uninstall_skill,
            // Phase 1 命令
            pc_guardian_lib::commands::ping,
            pc_guardian_lib::commands::get_settings,
            pc_guardian_lib::commands::get_system_status,
            // V10: GitHub 趋势
            pc_guardian_lib::commands::fetch_github_trending,
        ])
        .run(tauri::generate_context!())
        .expect("error while running PC Guardian");
}

/// V21: 创建系统托盘图标和菜单
fn create_tray(app: &tauri::AppHandle) -> Result<(), Box<dyn std::error::Error>> {
    // 使用应用默认图标
    let icon = app.default_window_icon().cloned();

    // 构建菜单项
    let show_main = MenuItem::with_id(app, "tray_show_main", "显示主窗口", true, None::<&str>)?;
    let show_mini = MenuItem::with_id(app, "tray_show_mini", "显示浮窗", true, None::<&str>)?;
    let settings = MenuItem::with_id(app, "tray_settings", "设置", true, None::<&str>)?;
    let separator = PredefinedMenuItem::separator(app)?;
    let quit = PredefinedMenuItem::quit(app, Some("退出"))?;

    let menu = Menu::new(app)?;
    menu.append(&show_main)?;
    menu.append(&show_mini)?;
    menu.append(&settings)?;
    menu.append(&separator)?;
    menu.append(&quit)?;

    // 创建托盘图标
    let mut builder = TrayIconBuilder::with_id("pc-guardian-tray")
        .tooltip("PC Guardian AI")
        .menu(&menu)
        .show_menu_on_left_click(false);

    if let Some(img) = icon {
        builder = builder.icon(img);
    }

    builder
        .on_tray_icon_event(|tray, event| {
            // 左键点击切换主窗口显示/隐藏
            if let TrayIconEvent::Click { button, .. } = event {
                if button == MouseButton::Left {
                    let app = tray.app_handle();
                    if let Some(window) = app.get_webview_window("main") {
                        match window.is_visible() {
                            Ok(true) => {
                                let _ = window.hide();
                            }
                            _ => {
                                let _ = window.show();
                                let _ = window.set_focus();
                            }
                        }
                    }
                }
            }
        })
        .build(app)?;

    eprintln!("[Tray] 系统托盘创建成功");
    Ok(())
}

/// 注册全局快捷键 Ctrl+Shift+Space
fn register_global_shortcut(app_handle: &tauri::AppHandle) -> Result<(), Box<dyn std::error::Error>> {
    let shortcut_str = {
        let state = app_handle.state::<AppState>();
        state.global_shortcut.clone()
    };

    let shortcut = match shortcut_str.parse::<tauri_plugin_global_shortcut::Shortcut>() {
        Ok(s) => s,
        Err(e) => {
            eprintln!(
                "[GlobalShortcut] 解析 '{}' 失败: {}，使用默认 Ctrl+Shift+Space",
                shortcut_str, e
            );
            "Ctrl+Shift+Space".parse()?
        }
    };

    match app_handle.global_shortcut().register(shortcut) {
        Ok(_) => {
            eprintln!("[GlobalShortcut] 已注册: {}", shortcut_str);
            return Ok(());
        }
        Err(e) => {
            eprintln!(
                "[GlobalShortcut] 注册 '{}' 失败: {}，尝试备用快捷键",
                shortcut_str, e
            );
        }
    }

    let fallbacks = ["Ctrl+Alt+Space", "Ctrl+Shift+A", "Alt+Shift+Space"];
    for fb in &fallbacks {
        if let Ok(fb_shortcut) = fb.parse::<tauri_plugin_global_shortcut::Shortcut>() {
            if app_handle.global_shortcut().register(fb_shortcut).is_ok() {
                eprintln!("[GlobalShortcut] 已回退到备用快捷键: {}", fb);
                return Ok(());
            }
        }
    }

    eprintln!("[GlobalShortcut] 所有快捷键均注册失败，全局快捷键功能不可用");
    Ok(())
}
