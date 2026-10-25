// Prevents additional console window on Windows in release
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use pc_guardian_lib::AppState;
use std::sync::atomic::Ordering;
use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, TrayIconBuilder, TrayIconEvent};
use tauri::{Emitter, Manager, WindowEvent};

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
            let warmup_client = state.ollama.clone();
            let warmup_model = state.ollama_model.clone();
            let warmup_db = state.db.clone();
            app.manage(state);

            // 应用启动后在后台加载默认模型。不会阻塞窗口出现，后续首条指令可直接推理。
            //
            // 但显存只有 8 GiB：qwen3:8b 驻留要吃 5.4 GiB，而 Jarvis 的 omni 模型
            // 要 7.2 GiB，两者不可能同时待在卡上。要让 Jarvis 拿到显存，就把
            // `llm.warmup_on_start` 设成 false 跳过这次预热（配合把
            // `llm.keep_alive` 调小，避免它用一次之后又常驻回去）。
            tauri::async_runtime::spawn(async move {
                let enabled = warmup_db
                    .lock()
                    .await
                    .get_setting("llm.warmup_on_start")
                    .ok()
                    .flatten()
                    .map(|value| value != "false" && value != "0")
                    .unwrap_or(true);
                if !enabled {
                    eprintln!("[Ollama] 已按要求跳过预热（llm.warmup_on_start=false），显存留给 Jarvis");
                    return;
                }
                match warmup_client.warm_up(&warmup_model).await {
                    Ok(()) => eprintln!("[Ollama] 模型预热完成: {}", warmup_model),
                    Err(e) => eprintln!("[Ollama] 模型预热跳过: {}", e),
                }
            });

            // V18: 启动 Proactive Engine（后台监控）
            {
                let state = app.state::<AppState>();
                let engine = state.proactive_engine.clone();
                tauri::async_runtime::spawn(async move {
                    engine.start();
                });
            }

            // 启动常驻调度器（定时作业）
            {
                let state = app.state::<AppState>();
                let scheduler = state.scheduler.clone();
                tauri::async_runtime::spawn(async move {
                    scheduler.start();
                });
            }

            // 启动多端网关（设备配对 + 带鉴权的本地 HTTP 服务）
            {
                let state = app.state::<AppState>();
                let gateway = state.gateway.clone();
                tauri::async_runtime::spawn(async move {
                    gateway.start();
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
        .on_menu_event(|app, event| match event.id().as_ref() {
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
            // 模型职责路由与排他推理运行时
            pc_guardian_lib::commands::get_model_routing,
            pc_guardian_lib::commands::get_inference_runtime,
            pc_guardian_lib::commands::start_webpage_generation,
            pc_guardian_lib::commands::publish_webpage,
            pc_guardian_lib::commands::open_external,
            pc_guardian_lib::commands::list_webpage_versions,
            pc_guardian_lib::commands::restore_webpage_version,
            pc_guardian_lib::commands::webpage_chain_id,
            // 窗口管理命令
            pc_guardian_lib::commands::show_main_window,
            pc_guardian_lib::commands::close_mini_window,
            pc_guardian_lib::commands::is_mini_window,
            pc_guardian_lib::commands::toggle_mini_window,
            // 系统监控命令
            pc_guardian_lib::commands::get_system_metrics,
            // 设置管理命令
            pc_guardian_lib::commands::update_setting,
            pc_guardian_lib::commands::check_autostart_status,
            // 审计日志命令
            pc_guardian_lib::commands::get_audit_logs,
            // 能力契约 / 工具回执命令
            pc_guardian_lib::commands::get_tool_receipts,
            pc_guardian_lib::commands::get_capability_coverage,
            pc_guardian_lib::commands::get_task_truth,
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
            // 常驻调度命令
            pc_guardian_lib::commands::list_scheduled_jobs,
            pc_guardian_lib::commands::create_scheduled_job,
            pc_guardian_lib::commands::delete_scheduled_job,
            pc_guardian_lib::commands::set_scheduled_job_enabled,
            pc_guardian_lib::commands::run_scheduled_job_now,
            pc_guardian_lib::commands::get_scheduled_job_runs,
            pc_guardian_lib::commands::get_scheduler_status,
            // 多端设备命令
            pc_guardian_lib::commands::list_paired_devices,
            pc_guardian_lib::commands::create_pairing_code,
            pc_guardian_lib::commands::revoke_paired_device,
            pc_guardian_lib::commands::get_gateway_status,
            // 模型网关状态
            pc_guardian_lib::commands::get_model_gateway_status,
            // V20: 技能管理命令
            pc_guardian_lib::commands::get_skills,
            pc_guardian_lib::commands::get_skill_details,
            pc_guardian_lib::commands::toggle_skill,
            pc_guardian_lib::commands::install_skill,
            pc_guardian_lib::commands::uninstall_skill,
            // 界面直呼能力（能力中心 / 媒体工坊 / 收件箱）
            pc_guardian_lib::commands::list_tool_contracts,
            pc_guardian_lib::commands::list_media_generations,
            pc_guardian_lib::commands::run_ui_tool,
            // AI Jarvis：外部感知进程的托管与桥接
            pc_guardian_lib::commands::jarvis_health,
            pc_guardian_lib::commands::jarvis_start,
            pc_guardian_lib::commands::jarvis_stop,
            pc_guardian_lib::commands::jarvis_chat,
            pc_guardian_lib::commands::jarvis_duplex,
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


/// 启动时注册全局快捷键：从设置读，注册，并把最终生效值写回库。
///
/// 写回的必要性：如果用户填的键被别的软件占用，这里会回退到备用键，
/// 界面上却仍显示他填的那个——下次启动又会去抢同一个失败的键。
/// 把实际生效值落库，界面看到的就和系统里的一致。
fn register_global_shortcut(
    app_handle: &tauri::AppHandle,
) -> Result<(), Box<dyn std::error::Error>> {
    let shortcut_str = {
        let state = app_handle.state::<AppState>();
        let guard = state.global_shortcut.blocking_lock();
        guard.clone()
    };

    let effective = pc_guardian_lib::shortcut::register_global_shortcut_at(
        app_handle,
        &shortcut_str,
        None,
    );

    // 回退链可能选中了别的键（用户填的被占用），把实际生效值写回两处：
    // 内存里的句柄供下次热切换注销，库里供界面显示。
    if effective != shortcut_str {
        {
            let state = app_handle.state::<AppState>();
            *state.global_shortcut.blocking_lock() = effective.clone();
        }
        if let Ok(db) = app_handle.state::<AppState>().db.try_lock() {
            let _ = db.update_setting("global_shortcut", &effective);
        }
    }

    Ok(())
}
