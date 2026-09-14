//! PC Guardian AI - 核心库
//!
//! 三层闸门架构：ToolRegistry → PermissionManager → ToolExecutor
//! LLM 永不直接接触系统 API

pub mod agent;
pub mod browser;
pub mod computer;
pub mod db;
pub mod error;
pub mod llm;
pub mod memory;
pub mod monitoring;
pub mod proactive;
pub mod security;
pub mod skills;
pub mod tools;
pub mod utils;

use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::collections::HashMap;
use tokio::sync::Mutex;

#[cfg(target_os = "windows")]
use windows::Win32::Foundation::*;
#[cfg(target_os = "windows")]
use windows::Win32::UI::WindowsAndMessaging::*;

use agent::AgentRuntime;
use db::Database;
use llm::OllamaClient;
use proactive::ProactiveEngine;
use tools::ToolRegistry;
use security::PermissionManager;
use crate::tools::executor::{PendingPermission, ToolExecutor};

/// 应用全局状态
///
/// 所有跨模块共享的资源都放在这里，通过 Tauri 的 state 管理。
pub struct AppState {
    pub db: Arc<Mutex<Database>>,
    pub tool_registry: Arc<ToolRegistry>,
    /// V21: 工具执行器（供 Proactive 等模块自动执行工具）
    pub tool_executor: Arc<ToolExecutor>,
    pub permission_manager: Arc<Mutex<PermissionManager>>,
    pub agent: Arc<Mutex<AgentRuntime>>,
    pub ollama: Arc<OllamaClient>,
    /// 全局快捷键配置（在初始化时预加载，避免运行时 block_on）
    pub global_shortcut: String,
    /// Phase 4: 待处理的权限确认请求（request_id → PendingPermission）
    pub pending_permissions: Arc<Mutex<HashMap<String, PendingPermission>>>,
    /// V18: Proactive 主动监控引擎
    pub proactive_engine: Arc<ProactiveEngine>,
    /// V21: 关闭按钮最小化到托盘（同步原子变量，供窗口事件回调读取）
    pub minimize_to_tray: Arc<AtomicBool>,
}

impl AppState {
    /// 初始化应用状态
    ///
    /// 使用配置默认值，实际配置在运行时从数据库读取。
    pub fn new(app: &tauri::App) -> Result<Self, Box<dyn std::error::Error>> {
        // 获取数据目录
        let app_dir = dirs::data_dir()
            .unwrap_or_else(|| std::path::PathBuf::from("."))
            .join("pc-guardian");
        std::fs::create_dir_all(&app_dir)?;

        let db_path = app_dir.join("pc-guardian.db");
        let db = Database::new(&db_path)?;
        db.init_schema()?;

        // V18: 确保 Proactive 预设规则存在
        match crate::proactive::ensure_preset_rules(db.connection()) {
            Ok(n) if n > 0 => eprintln!("[Proactive] 插入了 {} 条预设规则", n),
            _ => {}
        }

        // V17: 设置全局数据库路径（供 MemoryEngine 工具层使用）
        crate::memory::set_db_path(db_path.clone());

        // 启动自愈：清理所有崩溃遗留的非终态任务，防止重启后卡在 executing
        {
            let task_mgr = crate::agent::task::TaskManager::new(db.connection());
            match task_mgr.cleanup_stale_tasks() {
                Ok(n) if n > 0 => eprintln!("[Startup] 清理了 {} 个遗留任务", n),
                _ => {}
            }
        }

        // 预加载全局快捷键配置（必须在 db 被 move 进 Arc 之前）
        let mut global_shortcut = db
            .get_setting("global_shortcut")
            .ok()
            .flatten()
            .unwrap_or_else(|| "Ctrl+Shift+Space".to_string());

        // V20修复：自动修复被常见程序占用的旧快捷键配置
        // Alt+Space 常被 Watt Toolkit/Steam++ 等工具占用，自动升级为 Ctrl+Shift+Space
        let occupied = ["Alt+Space", "ALT+SPACE", "alt+space", "Alt+Space "];
        if occupied.contains(&global_shortcut.as_str()) {
            eprintln!(
                "[Shortcut] 检测到旧快捷键 '{}' 可能被占用，自动升级为 Ctrl+Shift+Space",
                global_shortcut
            );
            global_shortcut = "Ctrl+Shift+Space".to_string();
            let _ = db.update_setting("global_shortcut", &global_shortcut);
        }

        // 预加载 MEDIUM 风险确认配置
        let confirm_medium_risk = db
            .get_setting("security.confirm_medium_risk")
            .ok()
            .flatten()
            .map(|v| v == "true")
            .unwrap_or(true);

        // V21: 预加载"关闭按钮最小化到托盘"配置（默认开启）
        let minimize_to_tray_val = db
            .get_setting("tray.minimize_on_close")
            .ok()
            .flatten()
            .map(|v| v == "true")
            .unwrap_or(true);
        let minimize_to_tray = Arc::new(AtomicBool::new(minimize_to_tray_val));

        let db_arc = Arc::new(Mutex::new(db));

        // 默认配置
        let ollama_url = "http://localhost:11434".to_string();
        let ollama_model = "qwen3:8b".to_string();
        let max_iterations = 10usize;
        let temperature = 0.7f32;
        let timeout_ms = 60000u64;

        let mut tool_registry = ToolRegistry::new(db_arc.clone(), app_dir.clone());
        // V20: 动态注册技能工具（内置 + 本地）
        crate::skills::register_skill_tools(&mut tool_registry);
        let tool_registry = Arc::new(tool_registry);

        // Phase 4: 从数据库加载 PermissionManager（含可信目录和信任工具）
        let permission_manager = {
            let db_guard = db_arc.blocking_lock();
            let mut pm = PermissionManager::with_db(db_guard.connection());
            pm.update_config(confirm_medium_risk);
            Arc::new(Mutex::new(pm))
        };

        let ollama = Arc::new(OllamaClient::new(ollama_url, timeout_ms));

        let pending_permissions = Arc::new(Mutex::new(HashMap::<
            String,
            PendingPermission,
        >::new()));

        // V21: 创建全局 ToolExecutor（供 Agent 和 Proactive 共享）
        let tool_executor = Arc::new(ToolExecutor::new(
            permission_manager.clone(),
            tool_registry.clone(),
            db_arc.clone(),
        ));
        tool_executor.set_app_handle(app.handle().clone());
        tool_executor.set_pending_permissions(pending_permissions.clone());

        let agent = AgentRuntime::new(
            ollama.clone(),
            tool_registry.clone(),
            permission_manager.clone(),
            db_arc.clone(),
            ollama_model,
            temperature,
            max_iterations,
        );

        // Phase 4: 设置 ToolExecutor 的 app_handle 和 pending_permissions
        let app_handle = app.handle().clone();
        agent.set_app_handle(app_handle);
        agent.set_pending_permissions(pending_permissions.clone());

        // V18: 创建 Proactive Engine（在 main.rs setup 中启动）
        let proactive_engine = Arc::new(ProactiveEngine::new(app.handle().clone()));

        Ok(Self {
            db: db_arc,
            tool_registry,
            tool_executor,
            permission_manager,
            agent: Arc::new(Mutex::new(agent)),
            ollama,
            global_shortcut,
            pending_permissions,
            proactive_engine,
            minimize_to_tray,
        })
    }
}

// ============================================================
// Windows API: 浮窗拖动（WM_NCHITTEST 返回 HTCAPTION）
// ============================================================

#[cfg(target_os = "windows")]
static mut PREV_MINI_WINDOW_PROC: isize = 0;

/// 浮窗窗口过程：拦截 WM_NCHITTEST，返回 HTCAPTION 让 Windows 认为整个窗口都是标题栏
#[cfg(target_os = "windows")]
unsafe extern "system" fn mini_window_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if msg == WM_NCHITTEST {
        // 告诉 Windows 鼠标在标题栏上，从而允许拖动整个窗口
        return LRESULT(HTCAPTION as isize);
    }
    // 其他消息交给原窗口过程处理
    if PREV_MINI_WINDOW_PROC != 0 {
        let prev_proc: WNDPROC = std::mem::transmute(PREV_MINI_WINDOW_PROC);
        return CallWindowProcW(prev_proc, hwnd, msg, wparam, lparam);
    }
    DefWindowProcW(hwnd, msg, wparam, lparam)
}

/// 子类化浮窗窗口过程，启用整个窗口拖动
#[cfg(target_os = "windows")]
pub fn subclass_mini_window(hwnd: isize) {
    unsafe {
        let hwnd = HWND(hwnd);
        let prev_proc = SetWindowLongPtrW(hwnd, GWLP_WNDPROC, mini_window_proc as *const () as isize);
        PREV_MINI_WINDOW_PROC = prev_proc;
        eprintln!("[MiniWindow] 窗口子类化成功，启用整个窗口拖动");
    }
}

/// Tauri Command 注册模块
///
/// 所有暴露给前端的 IPC 命令都在这里定义。
pub mod commands {
    use rusqlite::params;
    use serde::{Deserialize, Serialize};
    use std::sync::atomic::Ordering;
    use tauri::{Emitter, Manager, State};

    use crate::AppState;
    use crate::db::conversation::{Conversation, ConversationManager, ConversationMessage};
    use crate::db::{AllowedDirectory, TrustedTool};
    use crate::agent::task::{TaskManager, Task};
    use crate::llm::OllamaModel;
    use crate::security::{PermissionDecision, PermissionResponse, PathPolicyResult, AccessType};

    // ============================================================
    // 对话相关命令
    // ============================================================

    /// 创建新对话
    #[tauri::command]
    pub async fn create_conversation(
        state: State<'_, AppState>,
        title: Option<String>,
    ) -> Result<Conversation, String> {
        let db = state.db.lock().await;
        let mgr = ConversationManager::new(db.connection());
        mgr.create(title).map_err(|e| e.to_string())
    }

    /// 获取所有对话
    #[tauri::command]
    pub async fn get_conversations(
        state: State<'_, AppState>,
    ) -> Result<Vec<Conversation>, String> {
        let db = state.db.lock().await;
        let mgr = ConversationManager::new(db.connection());
        mgr.list_all().map_err(|e| e.to_string())
    }

    /// 获取对话消息历史
    #[tauri::command]
    pub async fn get_messages(
        state: State<'_, AppState>,
        conversation_id: String,
    ) -> Result<Vec<ConversationMessage>, String> {
        let db = state.db.lock().await;
        let mgr = ConversationManager::new(db.connection());
        mgr.get_messages(&conversation_id).map_err(|e| e.to_string())
    }

    /// 删除对话
    #[tauri::command]
    pub async fn delete_conversation(
        state: State<'_, AppState>,
        conversation_id: String,
    ) -> Result<(), String> {
        let db = state.db.lock().await;
        let mgr = ConversationManager::new(db.connection());
        mgr.delete(&conversation_id).map_err(|e| e.to_string())
    }

    /// 发送消息（触发 Agent Loop）
    ///
    /// 在后台启动 Agent Loop，通过 Tauri Event 推送流式结果。
    /// 前端需要监听 "chat:stream" 事件来接收结果。
    #[tauri::command]
    pub async fn send_message(
        window: tauri::Window,
        state: State<'_, AppState>,
        conversation_id: String,
        content: String,
    ) -> Result<(), String> {
        let app_handle = window.app_handle().clone();
        let agent = state.agent.lock().await;
        let rx = agent
            .send_message(&conversation_id, &content, &state.db)
            .await
            .map_err(|e| e.to_string())?;
        drop(agent);

        // 后台任务：将事件通过 Tauri Event 推送给前端
        let sid = conversation_id.clone();
        tokio::spawn(async move {
            let mut rx = rx;
            while let Some(event) = rx.recv().await {
                let (event_name, raw_payload) = match &event {
                    crate::agent::ChatEvent::Text(_) => ("chat:text", serde_json::to_string(&event).unwrap_or_default()),
                    crate::agent::ChatEvent::Thinking => ("chat:thinking", serde_json::to_string(&event).unwrap_or_default()),
                    crate::agent::ChatEvent::ToolCall { .. } => ("chat:tool_call", serde_json::to_string(&event).unwrap_or_default()),
                    crate::agent::ChatEvent::ToolResult { .. } => ("chat:tool_result", serde_json::to_string(&event).unwrap_or_default()),
                    crate::agent::ChatEvent::TaskUpdate(ref ev) => ("agent:task_update", serde_json::to_string(ev).unwrap_or_default()),
                    crate::agent::ChatEvent::ActionStatus(ref ev) => ("agent:action_status", serde_json::to_string(ev).unwrap_or_default()),
                    crate::agent::ChatEvent::Recovery(ref ev) => ("agent:recovery", serde_json::to_string(ev).unwrap_or_default()),
                    crate::agent::ChatEvent::Done => ("chat:done", serde_json::to_string(&event).unwrap_or_default()),
                    crate::agent::ChatEvent::Error(_) => ("chat:error", serde_json::to_string(&event).unwrap_or_default()),
                };

                // V21: 注入 session_id，前端按当前对话过滤事件
                let payload = if let Ok(mut json) = serde_json::from_str::<serde_json::Value>(&raw_payload) {
                    if let Some(obj) = json.as_object_mut() {
                        obj.insert("session_id".to_string(), serde_json::Value::String(sid.clone()));
                    }
                    serde_json::to_string(&json).unwrap_or(raw_payload)
                } else {
                    raw_payload
                };

                let _ = app_handle.emit(event_name, payload);

                if matches!(event, crate::agent::ChatEvent::Done | crate::agent::ChatEvent::Error(_)) {
                    break;
                }
            }
        });

        Ok(())
    }

    // ============================================================
    // Phase 4: 权限确认响应
    // ============================================================

    /// 响应权限确认请求
    #[tauri::command]
    pub async fn respond_permission(
        state: State<'_, AppState>,
        response: PermissionResponse,
    ) -> Result<(), String> {
        let mut pending = state.pending_permissions.lock().await;
        if let Some(pending_perm) = pending.remove(&response.request_id) {
            if response.decision == PermissionDecision::AlwaysAllow {
                let db = state.db.lock().await;
                if let Err(e) = db.add_trusted_tool(
                    &pending_perm.tool_name,
                    pending_perm.path.as_deref(),
                    &pending_perm.risk_level,
                ) {
                    eprintln!("写入 trusted_tools 失败: {}", e);
                }
                let mut pm = state.permission_manager.lock().await;
                pm.reload_trusted_data(db.connection());
            }
            let _ = pending_perm.sender.send(response.decision);
            Ok(())
        } else {
            Err(format!("权限请求 {} 不存在或已超时", response.request_id))
        }
    }

    // ============================================================
    // Phase 4: 可信目录管理
    // ============================================================

    /// 获取所有允许目录
    #[tauri::command]
    pub async fn get_allowed_directories(
        state: State<'_, AppState>,
    ) -> Result<Vec<AllowedDirectory>, String> {
        let db = state.db.lock().await;
        db.get_allowed_directories().map_err(|e| e.to_string())
    }

    /// 添加允许目录
    #[tauri::command]
    pub async fn add_allowed_directory(
        state: State<'_, AppState>,
        path: String,
        label: String,
        access_level: String,
    ) -> Result<i64, String> {
        let db = state.db.lock().await;
        let result = db.add_allowed_directory(&path, &label, &access_level).map_err(|e| e.to_string())?;
        let mut pm = state.permission_manager.lock().await;
        pm.reload_trusted_data(db.connection());
        Ok(result)
    }

    /// 删除允许目录
    #[tauri::command]
    pub async fn remove_allowed_directory(
        state: State<'_, AppState>,
        path: String,
    ) -> Result<(), String> {
        let db = state.db.lock().await;
        db.remove_allowed_directory(&path).map_err(|e| e.to_string())?;
        let mut pm = state.permission_manager.lock().await;
        pm.reload_trusted_data(db.connection());
        Ok(())
    }

    // ============================================================
    // Phase 4: 信任工具管理
    // ============================================================

    /// 获取所有信任工具
    #[tauri::command]
    pub async fn get_trusted_tools(
        state: State<'_, AppState>,
    ) -> Result<Vec<TrustedTool>, String> {
        let db = state.db.lock().await;
        db.get_trusted_tools().map_err(|e| e.to_string())
    }

    /// 添加信任工具
    #[tauri::command]
    pub async fn add_trusted_tool(
        state: State<'_, AppState>,
        tool_name: String,
        path_pattern: Option<String>,
        risk_level: String,
    ) -> Result<(), String> {
        let db = state.db.lock().await;
        db.add_trusted_tool(&tool_name, path_pattern.as_deref(), &risk_level).map_err(|e| e.to_string())?;
        let mut pm = state.permission_manager.lock().await;
        pm.reload_trusted_data(db.connection());
        Ok(())
    }

    /// 删除信任工具
    #[tauri::command]
    pub async fn remove_trusted_tool(
        state: State<'_, AppState>,
        tool_name: String,
        path_pattern: Option<String>,
    ) -> Result<(), String> {
        let db = state.db.lock().await;
        db.remove_trusted_tool(&tool_name, path_pattern.as_deref()).map_err(|e| e.to_string())?;
        let mut pm = state.permission_manager.lock().await;
        pm.reload_trusted_data(db.connection());
        Ok(())
    }

    // ============================================================
    // Phase 4: 路径校验（供前端预览）
    // ============================================================

    /// 校验路径安全
    #[tauri::command]
    pub async fn validate_path_security(
        state: State<'_, AppState>,
        path: String,
        access_type: String,
    ) -> Result<PathValidationResult, String> {
        let pm = state.permission_manager.lock().await;
        let access = match access_type.as_str() {
            "read" => AccessType::Read,
            "write" => AccessType::Write,
            "delete" => AccessType::Delete,
            _ => AccessType::Read,
        };
        let result = pm.validate_path(&path, access);
        let (allowed, confirmation_required, reason, code) = match result {
            PathPolicyResult::Allow => (true, false, None, None),
            PathPolicyResult::ConfirmationRequired => (false, true, None, None),
            PathPolicyResult::Deny { reason, code } => (false, false, Some(reason), Some(code)),
        };
        Ok(PathValidationResult {
            allowed,
            confirmation_required,
            reason,
            code,
        })
    }

    // ============================================================
    // Ollama 相关命令
    // ============================================================

    /// 检查 Ollama 是否可用
    #[tauri::command]
    pub async fn check_ollama(state: State<'_, AppState>) -> Result<bool, String> {
        Ok(state.ollama.is_available().await)
    }

    /// 获取本地 Ollama 模型列表
    #[tauri::command]
    pub async fn get_ollama_models(
        state: State<'_, AppState>,
    ) -> Result<Vec<OllamaModel>, String> {
        state
            .ollama
            .list_models()
            .await
            .map_err(|e| e.to_string())
    }

    // ============================================================
    // 原有命令（Phase 1）
    // ============================================================

    /// Ping 命令
    #[tauri::command]
    pub async fn ping(message: Option<String>) -> Result<PingResponse, String> {
        Ok(PingResponse {
            success: true,
            message: message.unwrap_or_else(|| "hello".to_string()),
            timestamp: chrono::Utc::now().timestamp(),
        })
    }

    /// 获取配置项
    #[tauri::command]
    pub async fn get_settings(
        state: State<'_, AppState>,
    ) -> Result<Vec<SettingItem>, String> {
        let db = state.db.lock().await;
        db.get_all_settings().map_err(|e| e.to_string())
    }

    /// 获取系统状态
    #[tauri::command]
    pub async fn get_system_status() -> Result<SystemStatus, String> {
        Ok(SystemStatus {
            status: "ok",
            version: env!("CARGO_PKG_VERSION"),
        })
    }

    // ============================================================
    // V10: GitHub 趋势
    // ============================================================

    /// 拉取 GitHub 趋势仓库并保存快照（供前端 GitHub 页面使用）
    #[tauri::command]
    pub async fn fetch_github_trending(
        state: State<'_, AppState>,
        range: Option<String>,
        language: Option<String>,
        limit: Option<u32>,
    ) -> Result<serde_json::Value, String> {
        let range = range
            .filter(|r| !r.is_empty())
            .unwrap_or_else(|| "daily".to_string());
        let limit = limit.unwrap_or(20) as usize;
        let repos = crate::tools::executors::github::fetch_github_repos(
            &range,
            language.as_deref(),
            None,
            limit,
        )
        .await
        .map_err(|e| e.to_string())?;

        let app_dir = dirs::data_dir()
            .unwrap_or_else(|| std::path::PathBuf::from("."))
            .join("pc-guardian");
        {
            let db = state.db.lock().await;
            crate::tools::executors::github::save_github_snapshot(
                &db,
                &app_dir,
                &range,
                language.as_deref(),
                &repos,
            )
            .map_err(|e| e.to_string())?;
        }
        Ok(serde_json::json!({ "repos": repos, "count": repos.len(), "range": range }))
    }

    // ============================================================
    // 窗口管理命令
    // ============================================================

    /// 显示/聚焦主窗口
    #[tauri::command]
    pub async fn show_main_window(app_handle: tauri::AppHandle) -> Result<(), String> {
        if let Some(window) = app_handle.get_webview_window("main") {
            window.show().map_err(|e| e.to_string())?;
            window.set_focus().map_err(|e| e.to_string())?;
        }
        Ok(())
    }

    /// 关闭迷你窗口
    #[tauri::command]
    pub async fn close_mini_window(window: tauri::Window) -> Result<(), String> {
        if window.label() == "mini" {
            window.close().map_err(|e| e.to_string())?;
        }
        Ok(())
    }

    /// 判断当前窗口是否为迷你窗口
    #[tauri::command]
    pub async fn is_mini_window(window: tauri::Window) -> Result<bool, String> {
        Ok(window.label() == "mini")
    }

    /// 切换迷你浮窗的显示/隐藏（无边框透明窗口）
    #[tauri::command]
    pub async fn toggle_mini_window(app_handle: tauri::AppHandle) -> Result<(), String> {
        toggle_mini_window_inner(&app_handle).map_err(|e| e.to_string())
    }

    /// 切换迷你浮窗的内部实现（供全局快捷键 handler 等非命令代码调用）
    pub fn toggle_mini_window_inner(app_handle: &tauri::AppHandle) -> Result<(), Box<dyn std::error::Error>> {
        if let Some(window) = app_handle.get_webview_window("mini") {
            match window.is_visible() {
                Ok(true) => { let _ = window.hide(); }
                _ => { let _ = window.show(); let _ = window.set_focus(); }
            }
        } else {
            let window = tauri::WebviewWindowBuilder::new(
                app_handle,
                "mini",
                tauri::WebviewUrl::App("index.html".into()),
            )
            .title("PC Guardian")
            .decorations(false)
            .transparent(true)
            .shadow(false)
            .always_on_top(true)
            .inner_size(240.0, 280.0)
            .center()
            .skip_taskbar(true)
            .build()?;

            // Windows: 子类化窗口过程，WM_NCHITTEST 返回 HTCAPTION 启用整个窗口拖动
            #[cfg(target_os = "windows")]
            {
                if let Ok(hwnd) = window.hwnd() {
                    crate::subclass_mini_window(hwnd.0 as isize);
                }
            }
        }
        Ok(())
    }

    // ============================================================
    // 系统监控命令
    // ============================================================

    /// 获取系统指标快照
    #[tauri::command]
    pub async fn get_system_metrics() -> Result<crate::monitoring::SystemMetrics, String> {
        Ok(crate::monitoring::get_system_metrics())
    }

    // ============================================================
    // 设置管理命令
    // ============================================================

    /// 更新单个配置项
    #[tauri::command]
    pub async fn update_setting(
        state: State<'_, AppState>,
        key: String,
        value: String,
    ) -> Result<(), String> {
        // V21: 同步更新内存中的原子变量（避免窗口事件回调读旧值）
        if key == "tray.minimize_on_close" {
            state.minimize_to_tray.store(value == "true", Ordering::SeqCst);
        }
        let db = state.db.lock().await;
        db.update_setting(&key, &value).map_err(|e| e.to_string())
    }

    // ============================================================
    // 审计日志命令
    // ============================================================

    /// 获取审计日志列表
    #[tauri::command]
    pub async fn get_audit_logs(
        state: State<'_, AppState>,
        limit: Option<i64>,
    ) -> Result<Vec<AuditLogEntry>, String> {
        let db = state.db.lock().await;
        let conn = db.connection();
        let limit = limit.unwrap_or(100);

        let mut stmt = conn.prepare(
            "SELECT id, timestamp, session_id, tool_name, tool_risk_level, \
             permission_result, success, error_code, error_message, duration_ms \
             FROM audit_logs ORDER BY timestamp DESC LIMIT ?1"
        ).map_err(|e| e.to_string())?;

        let logs = stmt.query_map(params![limit], |row| {
            Ok(AuditLogEntry {
                id: row.get(0)?,
                timestamp: row.get(1)?,
                session_id: row.get(2)?,
                tool_name: row.get(3)?,
                tool_risk_level: row.get(4)?,
                permission_result: row.get(5)?,
                success: row.get::<_, i64>(6)? != 0,
                error_code: row.get(7)?,
                error_message: row.get(8)?,
                duration_ms: row.get(9)?,
            })
        }).map_err(|e| e.to_string())?;

        logs.collect::<Result<Vec<_>, _>>().map_err(|e| e.to_string())
    }

    /// 审计日志条目
    #[derive(Debug, Serialize)]
    pub struct AuditLogEntry {
        pub id: i64,
        pub timestamp: String,
        pub session_id: String,
        pub tool_name: String,
        pub tool_risk_level: String,
        pub permission_result: String,
        pub success: bool,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub error_code: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub error_message: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub duration_ms: Option<i64>,
    }

    #[derive(Debug, Serialize)]
    pub struct PingResponse {
        pub success: bool,
        pub message: String,
        pub timestamp: i64,
    }

    #[derive(Debug, Serialize, Deserialize)]
    pub struct SettingItem {
        pub key: String,
        pub value: String,
        pub value_type: String,
        pub description: Option<String>,
    }

    // ============================================================
    // 任务管理命令
    // ============================================================

    /// 获取会话的任务列表
    #[tauri::command]
    pub async fn get_tasks(
        state: State<'_, AppState>,
        conversation_id: String,
        limit: Option<i64>,
    ) -> Result<Vec<Task>, String> {
        let db = state.db.lock().await;
        let task_mgr = TaskManager::new(db.connection());
        task_mgr.list_by_session(&conversation_id, limit.unwrap_or(20))
            .map_err(|e| e.to_string())
    }

    /// 获取会话的当前活跃任务
    #[tauri::command]
    pub async fn get_active_task(
        state: State<'_, AppState>,
        conversation_id: String,
    ) -> Result<Option<Task>, String> {
        let db = state.db.lock().await;
        let task_mgr = TaskManager::new(db.connection());
        task_mgr.get_active_by_session(&conversation_id)
            .map_err(|e| e.to_string())
    }

    /// 取消会话的活跃任务
    #[tauri::command]
    pub async fn cancel_task(
        state: State<'_, AppState>,
        conversation_id: String,
    ) -> Result<(), String> {
        let db = state.db.lock().await;
        let task_mgr = TaskManager::new(db.connection());
        task_mgr.cancel_active_by_session(&conversation_id)
            .map_err(|e| e.to_string())
    }

    #[derive(Debug, Serialize)]
    pub struct SystemStatus {
        pub status: &'static str,
        pub version: &'static str,
    }

    /// Phase 4: 路径校验结果
    #[derive(Debug, Serialize)]
    pub struct PathValidationResult {
        pub allowed: bool,
        pub confirmation_required: bool,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub reason: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub code: Option<String>,
    }

    // ============================================================
    // V17: 记忆管理命令
    // ============================================================

    /// 记忆条目（前端展示用）
    #[derive(Debug, Serialize)]
    pub struct MemoryItemResponse {
        pub id: String,
        pub memory_type: String,
        pub content: String,
        pub importance: u8,
        pub access_count: u32,
        pub created_at: String,
        pub last_accessed_at: String,
        pub source: String,
    }

    /// 获取记忆列表（支持类型过滤）
    #[tauri::command]
    pub async fn get_memories(
        state: State<'_, AppState>,
        memory_type: Option<String>,
        limit: Option<i64>,
    ) -> Result<Vec<MemoryItemResponse>, String> {
        use crate::memory::{MemoryEngine, MemoryType};
        let db = state.db.lock().await;
        let mtype = memory_type.as_deref().map(MemoryType::from_str);
        let memories = MemoryEngine::list_by_type(db.connection(), mtype, limit.unwrap_or(50) as usize)
            .map_err(|e| e.to_string())?;
        Ok(memories.into_iter().map(|m| MemoryItemResponse {
            id: m.id,
            memory_type: m.memory_type.as_str().to_string(),
            content: m.content,
            importance: m.importance,
            access_count: m.access_count,
            created_at: m.created_at,
            last_accessed_at: m.last_accessed_at,
            source: m.source,
        }).collect())
    }

    /// 手动添加记忆
    #[tauri::command]
    pub async fn add_memory(
        state: State<'_, AppState>,
        content: String,
        memory_type: Option<String>,
        importance: Option<u8>,
    ) -> Result<String, String> {
        use crate::memory::{MemoryEngine, MemoryType};
        let db = state.db.lock().await;
        let mtype = memory_type.as_deref().map(MemoryType::from_str).unwrap_or(MemoryType::Knowledge);
        let imp = importance.unwrap_or(50);
        MemoryEngine::add_memory(db.connection(), mtype, &content, imp, "manual")
            .map_err(|e| e.to_string())
    }

    /// 删除记忆
    #[tauri::command]
    pub async fn delete_memory(
        state: State<'_, AppState>,
        memory_id: String,
    ) -> Result<bool, String> {
        use crate::memory::MemoryEngine;
        let db = state.db.lock().await;
        MemoryEngine::forget_by_id(db.connection(), &memory_id)
            .map_err(|e| e.to_string())
    }

    /// 搜索记忆
    #[tauri::command]
    pub async fn search_memories(
        state: State<'_, AppState>,
        query: String,
        memory_type: Option<String>,
        limit: Option<i64>,
    ) -> Result<Vec<MemoryItemResponse>, String> {
        use crate::memory::{MemoryEngine, MemoryType};
        let db = state.db.lock().await;
        let mtype = memory_type.as_deref().map(MemoryType::from_str);
        let memories = MemoryEngine::recall(db.connection(), &query, mtype, limit.unwrap_or(10) as usize)
            .map_err(|e| e.to_string())?;
        Ok(memories.into_iter().map(|m| MemoryItemResponse {
            id: m.id,
            memory_type: m.memory_type.as_str().to_string(),
            content: m.content,
            importance: m.importance,
            access_count: m.access_count,
            created_at: m.created_at,
            last_accessed_at: m.last_accessed_at,
            source: m.source,
        }).collect())
    }

    // ============================================================
    // V18: Proactive 主动助手命令
    // ============================================================

    /// 获取所有主动规则
    #[tauri::command]
    pub async fn get_proactive_rules(
        state: State<'_, AppState>,
    ) -> Result<Vec<crate::proactive::ProactiveRule>, String> {
        let db = state.db.lock().await;
        crate::proactive::get_rules(db.connection()).map_err(|e| e.to_string())
    }

    /// 添加新规则
    #[tauri::command]
    pub async fn add_proactive_rule(
        state: State<'_, AppState>,
        name: String,
        trigger_type: String,
        trigger_config: String,
        action_type: String,
        action_config: String,
    ) -> Result<String, String> {
        let id = format!("rule_{}", uuid::Uuid::new_v4().simple());
        let now = chrono::Utc::now().format("%Y-%m-%d %H:%M:%S").to_string();
        let rule = crate::proactive::ProactiveRule {
            id: id.clone(),
            name,
            trigger_type,
            trigger_config,
            action_type,
            action_config,
            enabled: true,
            last_triggered: None,
            trigger_count: 0,
            created_at: now,
        };
        let db = state.db.lock().await;
        crate::proactive::insert_rule(db.connection(), &rule).map_err(|e| e.to_string())?;
        Ok(id)
    }

    /// 更新规则
    #[tauri::command]
    pub async fn update_proactive_rule(
        state: State<'_, AppState>,
        id: String,
        name: String,
        trigger_type: String,
        trigger_config: String,
        action_type: String,
        action_config: String,
        enabled: bool,
    ) -> Result<(), String> {
        let db = state.db.lock().await;
        let existing = crate::proactive::get_rule_by_id(db.connection(), &id)?
            .ok_or_else(|| format!("规则 {} 不存在", id))?;
        let rule = crate::proactive::ProactiveRule {
            id,
            name,
            trigger_type,
            trigger_config,
            action_type,
            action_config,
            enabled,
            last_triggered: existing.last_triggered,
            trigger_count: existing.trigger_count,
            created_at: existing.created_at,
        };
        crate::proactive::update_rule(db.connection(), &rule).map_err(|e| e.to_string())
    }

    /// 删除规则
    #[tauri::command]
    pub async fn delete_proactive_rule(
        state: State<'_, AppState>,
        id: String,
    ) -> Result<(), String> {
        let db = state.db.lock().await;
        crate::proactive::delete_rule(db.connection(), &id).map_err(|e| e.to_string())
    }

    /// 启用/禁用规则
    #[tauri::command]
    pub async fn toggle_proactive_rule(
        state: State<'_, AppState>,
        id: String,
        enabled: bool,
    ) -> Result<(), String> {
        let db = state.db.lock().await;
        crate::proactive::toggle_rule(db.connection(), &id, enabled).map_err(|e| e.to_string())
    }

    /// 获取 Proactive Engine 运行状态
    #[tauri::command]
    pub async fn get_proactive_status(
        state: State<'_, AppState>,
    ) -> Result<crate::proactive::ProactiveStatus, String> {
        let engine_status = state.proactive_engine.get_status();
        let db = state.db.lock().await;
        let rules = crate::proactive::get_rules(db.connection()).unwrap_or_default();
        let enabled_count = rules.iter().filter(|r| r.enabled).count();
        Ok(crate::proactive::ProactiveStatus {
            running: engine_status.running,
            rules_total: rules.len(),
            rules_enabled: enabled_count,
            last_check: engine_status.last_check,
            checks_count: engine_status.checks_count,
            triggers_count: engine_status.triggers_count,
        })
    }

    // ============================================================
    // V20: 技能管理命令
    // ============================================================

    /// 技能信息（前端展示用）
    #[derive(Debug, Serialize)]
    pub struct SkillInfo {
        pub id: String,
        pub name: String,
        pub description: String,
        pub version: String,
        pub author: String,
        pub skill_type: String,
        pub enabled: bool,
        pub installed_at: String,
        pub permissions: Vec<String>,
        pub triggers: Vec<String>,
    }

    /// 获取所有已安装技能
    #[tauri::command]
    pub async fn get_skills(
        state: State<'_, AppState>,
    ) -> Result<Vec<SkillInfo>, String> {
        let _ = state; // 预留：未来从 state 获取
        let mgr = crate::skills::global();
        let skills = if let Some(m) = mgr {
            m.list_skills().await
        } else {
            // 如果全局管理器未初始化，直接加载
            let mgr = crate::skills::SkillManager::new().await;
            mgr.list_skills().await
        };

        Ok(skills.into_iter().map(|s| SkillInfo {
            id: s.id,
            name: s.name,
            description: s.description,
            version: s.version,
            author: s.author,
            skill_type: s.skill_type.as_str().to_string(),
            enabled: s.enabled,
            installed_at: s.installed_at,
            permissions: s.permissions,
            triggers: s.triggers,
        }).collect())
    }

    /// 获取技能详情
    #[tauri::command]
    pub async fn get_skill_details(
        state: State<'_, AppState>,
        skill_id: String,
    ) -> Result<SkillInfo, String> {
        let _ = state;
        let mgr = crate::skills::global();
        let skill = if let Some(m) = mgr {
            m.get_skill(&skill_id).await
        } else {
            None
        };

        skill.map(|s| SkillInfo {
            id: s.id,
            name: s.name,
            description: s.description,
            version: s.version,
            author: s.author,
            skill_type: s.skill_type.as_str().to_string(),
            enabled: s.enabled,
            installed_at: s.installed_at,
            permissions: s.permissions,
            triggers: s.triggers,
        }).ok_or_else(|| format!("技能不存在: {}", skill_id))
    }

    /// 启用/禁用技能
    #[tauri::command]
    pub async fn toggle_skill(
        state: State<'_, AppState>,
        skill_id: String,
        enabled: bool,
    ) -> Result<(), String> {
        let _ = state;
        if let Some(mgr) = crate::skills::global() {
            mgr.toggle_skill(&skill_id, enabled).await.map_err(|e| e.to_string())
        } else {
            Err("技能管理器未初始化".to_string())
        }
    }

    /// 安装技能（从目录）
    #[tauri::command]
    pub async fn install_skill(
        state: State<'_, AppState>,
        source_dir: String,
    ) -> Result<SkillInfo, String> {
        let _ = state;
        if let Some(mgr) = crate::skills::global() {
            let skill = mgr.install_skill(&source_dir).await.map_err(|e| e.to_string())?;
            Ok(SkillInfo {
                id: skill.id,
                name: skill.name,
                description: skill.description,
                version: skill.version,
                author: skill.author,
                skill_type: skill.skill_type.as_str().to_string(),
                enabled: skill.enabled,
                installed_at: skill.installed_at,
                permissions: skill.permissions,
                triggers: skill.triggers,
            })
        } else {
            Err("技能管理器未初始化".to_string())
        }
    }

    /// 卸载技能
    #[tauri::command]
    pub async fn uninstall_skill(
        state: State<'_, AppState>,
        skill_id: String,
    ) -> Result<(), String> {
        let _ = state;
        if let Some(mgr) = crate::skills::global() {
            mgr.uninstall_skill(&skill_id).await.map_err(|e| e.to_string())
        } else {
            Err("技能管理器未初始化".to_string())
        }
    }
}
