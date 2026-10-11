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
pub mod webgen;
pub mod webpublish;

use std::collections::HashMap;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use tokio::sync::Mutex;

#[cfg(target_os = "windows")]
use windows::Win32::Foundation::*;
#[cfg(target_os = "windows")]
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::tools::executor::{PendingPermission, ToolExecutor};
use agent::AgentRuntime;
use db::Database;
use llm::OllamaClient;
use proactive::ProactiveEngine;
use security::PermissionManager;
use tools::ToolRegistry;

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
    pub ollama_model: String,
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

        // 从数据库读取运行参数，使用户选择的小模型和低延迟配置真正生效。
        let ollama_url = db
            .get_setting("ollama.base_url")?
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| "http://localhost:11434".to_string());
        let ollama_model = db
            .get_setting("ollama.model")?
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| "qwen3:8b".to_string());
        let max_iterations = db
            .get_setting("agent.max_iterations")?
            .and_then(|value| value.parse::<usize>().ok())
            .map(|value| value.clamp(1, 50))
            .unwrap_or(10);
        let temperature = db
            .get_setting("agent.temperature")?
            .and_then(|value| value.parse::<f32>().ok())
            .map(|value| value.clamp(0.0, 2.0))
            .unwrap_or(0.7);
        let timeout_ms = db
            .get_setting("ollama.timeout_ms")?
            .and_then(|value| value.parse::<u64>().ok())
            .map(|value| value.clamp(5_000, 300_000))
            .unwrap_or(60_000);

        let db_arc = Arc::new(Mutex::new(db));

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

        let pending_permissions = Arc::new(Mutex::new(HashMap::<String, PendingPermission>::new()));

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
            ollama_model.clone(),
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
            ollama_model,
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
        let prev_proc =
            SetWindowLongPtrW(hwnd, GWLP_WNDPROC, mini_window_proc as *const () as isize);
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
    use tauri::{AppHandle, Emitter, Manager, State};

    use crate::agent::task::{Task, TaskManager};
    use crate::db::conversation::{Conversation, ConversationManager, ConversationMessage};
    use crate::db::{AllowedDirectory, TrustedTool};
    use crate::llm::{ChatChunk, OllamaMessage, OllamaModel};
    use crate::security::{AccessType, PathPolicyResult, PermissionDecision, PermissionResponse};
    use crate::webgen;
    use crate::AppState;

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
        mgr.get_messages(&conversation_id)
            .map_err(|e| e.to_string())
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
        model: Option<String>,
        request_id: Option<String>,
    ) -> Result<String, String> {
        let app_handle = window.app_handle().clone();
        let agent = state.agent.lock().await;
        let run = agent
            .send_message(
                &conversation_id,
                &content,
                model.as_deref(),
                request_id.as_deref(),
                &state.db,
            )
            .await
            .map_err(|e| e.to_string())?;
        drop(agent);
        let run_id = run.run_id;
        let mut rx = run.events;

        // 后台任务：将事件通过 Tauri Event 推送给前端
        let sid = conversation_id.clone();
        let event_run_id = run_id.clone();
        tokio::spawn(async move {
            while let Some(event) = rx.recv().await {
                let (event_name, raw_payload) = match &event {
                    crate::agent::ChatEvent::Text(_) => (
                        "chat:text",
                        serde_json::to_string(&event).unwrap_or_default(),
                    ),
                    crate::agent::ChatEvent::Thinking => (
                        "chat:thinking",
                        serde_json::to_string(&event).unwrap_or_default(),
                    ),
                    crate::agent::ChatEvent::ToolCall { .. } => (
                        "chat:tool_call",
                        serde_json::to_string(&event).unwrap_or_default(),
                    ),
                    crate::agent::ChatEvent::ToolResult { .. } => (
                        "chat:tool_result",
                        serde_json::to_string(&event).unwrap_or_default(),
                    ),
                    crate::agent::ChatEvent::TaskUpdate(ref ev) => (
                        "agent:task_update",
                        serde_json::to_string(ev).unwrap_or_default(),
                    ),
                    crate::agent::ChatEvent::ActionStatus(ref ev) => (
                        "agent:action_status",
                        serde_json::to_string(ev).unwrap_or_default(),
                    ),
                    crate::agent::ChatEvent::Recovery(ref ev) => (
                        "agent:recovery",
                        serde_json::to_string(ev).unwrap_or_default(),
                    ),
                    crate::agent::ChatEvent::Done => (
                        "chat:done",
                        serde_json::to_string(&event).unwrap_or_default(),
                    ),
                    crate::agent::ChatEvent::Error(_) => (
                        "chat:error",
                        serde_json::to_string(&event).unwrap_or_default(),
                    ),
                };

                // V21: 注入 session_id，前端按当前对话过滤事件
                let payload =
                    if let Ok(mut json) = serde_json::from_str::<serde_json::Value>(&raw_payload) {
                        if let Some(obj) = json.as_object_mut() {
                            obj.insert(
                                "session_id".to_string(),
                                serde_json::Value::String(sid.clone()),
                            );
                            obj.insert(
                                "run_id".to_string(),
                                serde_json::Value::String(event_run_id.clone()),
                            );
                        }
                        serde_json::to_string(&json).unwrap_or(raw_payload)
                    } else {
                        raw_payload
                    };

                let _ = app_handle.emit(event_name, payload);

                if matches!(
                    event,
                    crate::agent::ChatEvent::Done | crate::agent::ChatEvent::Error(_)
                ) {
                    break;
                }
            }
        });

        Ok(run_id)
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
        let result = db
            .add_allowed_directory(&path, &label, &access_level)
            .map_err(|e| e.to_string())?;
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
        db.remove_allowed_directory(&path)
            .map_err(|e| e.to_string())?;
        let mut pm = state.permission_manager.lock().await;
        pm.reload_trusted_data(db.connection());
        Ok(())
    }

    // ============================================================
    // Phase 4: 信任工具管理
    // ============================================================

    /// 获取所有信任工具
    #[tauri::command]
    pub async fn get_trusted_tools(state: State<'_, AppState>) -> Result<Vec<TrustedTool>, String> {
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
        db.add_trusted_tool(&tool_name, path_pattern.as_deref(), &risk_level)
            .map_err(|e| e.to_string())?;
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
        db.remove_trusted_tool(&tool_name, path_pattern.as_deref())
            .map_err(|e| e.to_string())?;
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
    pub async fn get_ollama_models(state: State<'_, AppState>) -> Result<Vec<OllamaModel>, String> {
        state.ollama.list_models().await.map_err(|e| e.to_string())
    }

    // ============================================================
    // 网页工坊
    //
    // 上下文窗口、文档清洗与标准格式规格化统一放在 crate::webgen 模块里，
    // 这里只保留事件定义和与前端交互相关的常量。
    // ============================================================

    /// 续写时回传已有 HTML 的尾部：整份源码会撑爆窗口，但只给一小段又不足以接上下文。
    const WEBGEN_CONTINUATION_TAIL_CHARS: usize = 900;
    /// 迭代修改时，现有页面源码能整份回传的上限。
    const WEBGEN_PREV_HTML_FULL_LIMIT: usize = 6_000;
    /// 超过上限时只回传现有页面的开头部分（足以传达 head 与视觉风格）。
    const WEBGEN_PREV_HTML_HEAD_CHARS: usize = 3_000;
    /// 单轮输出被封顶后，最多自动续写几次。
    /// 每轮约一万多字符，六轮足够覆盖一个信息量很大的页面。
    const MAX_CONTINUATION_ROUNDS: usize = 6;
    /// 续写轮次至少要新增这么多字符才算"有进展"，否则判定为模型卡住。
    const CONTINUATION_MIN_GAIN: usize = 40;
    /// 功能自检不通过时，最多定向修复几次。
    ///
    /// 修复只重写 `<script>` 块（几行到几十行），不碰 HTML/CSS，所以在标准窗口里就能跑完，
    /// 一轮约一分钟。留三轮，并且要求后一轮必须比前一轮的加权成本更低才继续 ——
    /// 没有进展就停，免得越改越差。
    ///
    /// 为什么是三轮而不是两轮：走"就地补写"时，补进去的函数自己还会调用页面里本来就没写的
    /// 小助手（实测补了 10 个函数，其中两个又引出 `hash` / `randomPick`），所以天然需要
    /// **两级收敛**：第一轮补齐主体，第二轮补齐它们依赖的助手。两轮会在还剩两个缺失函数时
    /// 就停下，页面照旧点不动。
    const MAX_FUNCTION_REPAIR_ROUNDS: usize = 3;

    #[derive(Clone, Serialize)]
    pub struct WebGenerationEvent {
        generation_id: String,
        event_type: String,
        message: String,
        html: Option<String>,
        delta: Option<String>,
        spec: Option<String>,
        digest: Option<String>,
    }

    fn emit_web_generation_event(
        app: &AppHandle,
        generation_id: &str,
        event_type: &str,
        message: impl Into<String>,
        html: Option<String>,
    ) {
        let _ = app.emit(
            "webpage-generation:event",
            WebGenerationEvent {
                generation_id: generation_id.to_string(),
                event_type: event_type.to_string(),
                message: message.into(),
                html,
                delta: None,
                spec: None,
                digest: None,
            },
        );
    }

    /// 把加工后的标准格式规格推给前端，让用户能看到这道工序到底产出了什么。
    fn emit_web_generation_spec(app: &AppHandle, generation_id: &str, spec: &str) {
        let _ = app.emit(
            "webpage-generation:event",
            WebGenerationEvent {
                generation_id: generation_id.to_string(),
                event_type: "spec".to_string(),
                message: format!("需求已加工为标准格式（{} 字）。", spec.chars().count()),
                html: None,
                delta: None,
                spec: Some(spec.to_string()),
                digest: None,
            },
        );
    }

    /// 把梳理稿推给前端：用户可以在界面上核对，甚至直接改写它再生成。
    fn emit_web_generation_digest(app: &AppHandle, generation_id: &str, digest: &str) {
        let _ = app.emit(
            "webpage-generation:event",
            WebGenerationEvent {
                generation_id: generation_id.to_string(),
                event_type: "digest".to_string(),
                message: format!("文档已梳理成 {} 字的梳理稿。", digest.chars().count()),
                html: None,
                delta: None,
                spec: None,
                digest: Some(digest.to_string()),
            },
        );
    }

    fn emit_web_generation_delta(app: &AppHandle, generation_id: &str, delta: String, total_chars: usize) {
        let _ = app.emit(
            "webpage-generation:event",
            WebGenerationEvent {
                generation_id: generation_id.to_string(),
                event_type: "chunk".to_string(),
                message: format!("正在生成 HTML，已接收约 {total_chars} 字符。"),
                html: None,
                delta: Some(delta),
                spec: None,
                digest: None,
            },
        );
    }

    /// 在后台根据需求生成网页，并通过事件实时推送状态。
    #[tauri::command]
    pub async fn start_webpage_generation(
        app: AppHandle,
        state: State<'_, AppState>,
        generation_id: String,
        requirements: String,
        feedback: Option<String>,
        current_html: Option<String>,
        // 前端缓存的上一次规格化结果，命中时跳过全部文档加工环节。
        normalized_spec: Option<String>,
        // 前端展示并可编辑的梳理稿；用户改过就绕过清洗与梳理，直接规格化。
        digest: Option<String>,
    ) -> Result<(), String> {
        if generation_id.is_empty() || generation_id.len() > 80 {
            return Err("网页生成任务编号无效。".to_string());
        }
        // 需求文本要跨线程移动到后台任务，这里取一份自有副本，避免借用函数入参。
        let requirements = requirements.trim().to_string();
        if requirements.is_empty() {
            return Err("请先导入或填写网页需求文档。".to_string());
        }
        if requirements.chars().count() > webgen::MAX_CHARS_PER_RUN {
            return Err(format!(
                "需求文档内容过长，请控制在 {} 字以内。",
                webgen::MAX_CHARS_PER_RUN
            ));
        }

        let cached_spec = normalized_spec.filter(|spec| !spec.trim().is_empty());
        let edited_digest = digest.filter(|text| !text.trim().is_empty());

        let feedback_text = feedback
            .map(|text| text.trim().to_string())
            .filter(|text| !text.is_empty());
        let previous_html = current_html.filter(|html| !html.trim().is_empty());

        // 运行参数从数据库实时读取：用户在设置里改了模型或地址后无需重启应用即可生效。
        let (client, model) = {
            let db = state.db.lock().await;
            let base_url = db
                .get_setting("ollama.base_url")
                .map_err(|e| e.to_string())?
                .filter(|value| !value.trim().is_empty())
                .unwrap_or_else(|| "http://localhost:11434".to_string());
            let model = db
                .get_setting("ollama.model")
                .map_err(|e| e.to_string())?
                .filter(|value| !value.trim().is_empty())
                .unwrap_or_else(|| "qwen3:8b".to_string());
            let timeout_ms = db
                .get_setting("ollama.timeout_ms")
                .map_err(|e| e.to_string())?
                .and_then(|value| value.parse::<u64>().ok())
                .map(|value| value.clamp(5_000, 300_000))
                .unwrap_or(60_000);
            (crate::llm::OllamaClient::new(base_url, timeout_ms), model)
        };

        // 迭代修改要把现有页面源码回传，这是提示词里最大的一块，超长时只回传开头部分。
        let (previous_payload, previous_note) = match previous_html.as_ref() {
            None => (None, String::new()),
            Some(html) => {
                let chars = html.chars().count();
                if chars <= WEBGEN_PREV_HTML_FULL_LIMIT {
                    (Some(html.clone()), String::new())
                } else {
                    (
                        Some(webgen::take_head(html, WEBGEN_PREV_HTML_HEAD_CHARS)),
                        format!(
                            "\n\n（注意：现有页面共约 {chars} 字，受上下文长度限制这里只回传了开头部分。请在保持其视觉风格、配色和整体结构的前提下，按修改意见输出完整的新版本。）"
                        ),
                    )
                }
            }
        };

        // 提示词里除需求之外的部分：迭代修改回传的现有页面 + 修改意见。
        let extra_chars = previous_payload
            .as_ref()
            .map(|html| html.chars().count())
            .unwrap_or(0)
            + feedback_text
                .as_ref()
                .map(|text| text.chars().count())
                .unwrap_or(0);

        let generation_id_for_task = generation_id.clone();
        emit_web_generation_event(
            &app,
            &generation_id,
            "log",
            format!("任务已启动，模型 {model}。"),
            None,
        );

        tauri::async_runtime::spawn(async move {
            let log = |app: &AppHandle, generation_id: &str, message: &str| {
                emit_web_generation_event(app, generation_id, "log", message, None);
            };

            // 文档加工的三级短路：已加工好的规格 > 用户编辑过的梳理稿 > 完整流水线。
            let normalized = if let Some(spec) = cached_spec {
                log(&app, &generation_id_for_task, "复用上一次加工好的标准格式规格。");
                webgen::NormalizeResult {
                    spec,
                    digest: None,
                    stage: webgen::NormalizeStage::Reused,
                }
            } else if let Some(digest) = edited_digest {
                log(&app, &generation_id_for_task, "使用你在界面上编辑过的梳理稿。");
                webgen::specify_digest(&client, &model, &digest, |message: &str| {
                    log(&app, &generation_id_for_task, message);
                })
                .await
            } else {
                webgen::normalize_document(&client, &model, &requirements, |message: &str| {
                    log(&app, &generation_id_for_task, message);
                })
                .await
            };

            if let Some(digest) = normalized.digest.as_ref() {
                emit_web_generation_digest(&app, &generation_id_for_task, digest);
            }
            emit_web_generation_spec(&app, &generation_id_for_task, &normalized.spec);
            emit_web_generation_event(
                &app,
                &generation_id_for_task,
                "log",
                format!("加工结果：{}。", normalized.stage.label()),
                None,
            );

            let spec = normalized.spec;
            // 数据契约要在配平窗口之前算出来：它会跟着需求一起进提示词，
            // 漏算这 1000 多字就可能把提示词顶出窗口，需求在生成中途被滑出去。
            let data_contract = webgen::data_contract_block(&spec);
            if !data_contract.is_empty() {
                log(
                    &app,
                    &generation_id_for_task,
                    "已从需求中提取出数据契约，要求模型原样内联并真实使用。",
                );
            }
            // 覆盖契约：把"每个枚举取值都要有独立分支"单独拎出来说。
            // 实测这是"表面工作"里最隐蔽的一种 —— 6 个场景的下拉框渲染得齐齐整整，
            // 而生成函数只认其中两个，用户选其余的会直接看到 undefined。
            let coverage_contract = webgen::coverage_contract_block(&spec);
            if !coverage_contract.is_empty() {
                log(
                    &app,
                    &generation_id_for_task,
                    "已提取出枚举覆盖清单，要求模型为每个取值写出独立分支。",
                );
            }
            // 窗口与单轮输出上限按真实需求长度配平：提示词 + 单轮输出不超窗口，
            // 生成过程中需求就不会被滑出上下文（被滑出会退化成长列表复读）。
            let budget = webgen::generation_budget(
                spec.chars().count(),
                WEBGEN_CONTINUATION_TAIL_CHARS,
                extra_chars + data_contract.chars().count() + coverage_contract.chars().count(),
            );
            log(
                &app,
                &generation_id_for_task,
                &format!(
                    "上下文窗口 {} token，单轮最多输出 {} token。",
                    budget.num_ctx, budget.num_predict
                ),
            );
            // 需求块要单独留着：续写轮次也必须带上它，否则模型只看到"一段没写完的列表"，
            // 唯一能做的事就是把列表继续写下去 —— 实测会退化成无限复读。
            let requirements_block = format!(
                "请严格依据下面的网页需求生成产品网页：\n\n<requirements>\n{spec}\n</requirements>{data_contract}{coverage_contract}"
            );
            let mut user = requirements_block.clone();
            if let Some(feedback) = feedback_text.as_ref() {
                user.push_str(&format!(
                    "\n\n请根据以下修改意见，在现有网页基础上修改，并返回完整 HTML：\n{feedback}"
                ));
                if let Some(previous) = previous_payload.as_ref() {
                    user.push_str(&format!(
                        "\n\n<current_html>\n{previous}\n</current_html>{previous_note}"
                    ));
                }
            }

            let options = serde_json::json!({
                "num_ctx": budget.num_ctx,
                // 必须设上限：num_ctx 只是窗口大小，上下文写满后 Ollama 会滑动窗口继续生成，
                // num_predict 为 -1 时模型可以无限写下去（实测一次跑出 2 万 token、耗时 11 分钟）。
                "num_predict": budget.num_predict,
                // 抑制长列表上的复读退化。
                "repeat_penalty": webgen::REPEAT_PENALTY,
                "repeat_last_n": webgen::REPEAT_LAST_N,
            });

            let mut html = String::new();
            let mut last_reported = 0usize;
            let mut continuation_count = 0usize;
            loop {
                let messages = if continuation_count == 0 {
                    vec![
                        OllamaMessage::system(webgen::WEBPAGE_SYSTEM_PROMPT),
                        OllamaMessage::user(&user),
                    ]
                } else {
                    // 只回传尾部片段（整份源码会撑爆窗口），但需求必须一起带上。
                    let tail = webgen::take_tail(&html, WEBGEN_CONTINUATION_TAIL_CHARS);
                    let continuation = format!(
                        "{requirements_block}\n\n上面这个单文件网页你已经写了一部分，但中途停止了。下面是已输出内容的结尾片段（不是文件开头）：\n\n<tail>\n{tail}\n</tail>\n\n请紧接这段结尾继续输出剩余的 HTML 源码，补齐尚未实现的功能，直到包含 </body></html> 为止。只输出续写部分，不要重复已有片段，不要重新开始，不要输出任何解释。"
                    );
                    vec![
                        OllamaMessage::system(webgen::WEBPAGE_SYSTEM_PROMPT),
                        OllamaMessage::user(&continuation),
                    ]
                };

                emit_web_generation_event(
                    &app,
                    &generation_id_for_task,
                    "log",
                    if continuation_count == 0 { "正在连接 Ollama 并发送需求。" } else { "正在请求模型从断点继续补全网页。" },
                    None,
                );
                let mut stream = match client
                    .chat_stream_with_options(&model, messages, 0.2, options.clone())
                    .await
                {
                    Ok(stream) => stream,
                    Err(error) => {
                        emit_web_generation_event(&app, &generation_id_for_task, "error", format!("启动模型请求失败：{error:#}"), None);
                        return;
                    }
                };

                if continuation_count == 0 {
                    emit_web_generation_event(&app, &generation_id_for_task, "log", "模型连接成功，开始接收生成内容。", None);
                }
                let round_start = html.chars().count();
                let mut done_reason = None;
                let mut page_closed = false;
                while let Some(chunk) = futures_util::StreamExt::next(&mut stream).await {
                    match chunk {
                        ChatChunk::Text(text) => {
                            html.push_str(&text);
                            let total_chars = html.chars().count();
                            if total_chars.saturating_sub(last_reported) >= 512 {
                                let delta = html.chars().skip(last_reported).collect::<String>();
                                last_reported = total_chars;
                                emit_web_generation_delta(&app, &generation_id_for_task, delta, total_chars);
                                emit_web_generation_event(
                                    &app,
                                    &generation_id_for_task,
                                    "progress",
                                    format!("模型持续输出中，已接收约 {total_chars} 字符。"),
                                    None,
                                );
                            }
                            // 页面一旦闭合就立刻停止接收。
                            //
                            // 模型写完 `</html>` 不会主动停手，它会接着把"状态管理 / 数据源"
                            // 整段再写一遍 —— 实测一份 2.9 万字符的产物里出现了两份
                            // `let currentScene`，浏览器直接抛 SyntaxError，整段脚本不执行，
                            // 页面所有按钮失效。这里是唯一的止血点：等这一轮自然结束再判断，
                            // 重复内容早就进来了。
                            if text.contains('<') && html.to_ascii_lowercase().contains("</html>") {
                                page_closed = true;
                                break;
                            }
                        }
                        ChatChunk::ToolCall(_) => emit_web_generation_event(
                            &app,
                            &generation_id_for_task,
                            "log",
                            "收到非网页文本输出，忽略工具调用。",
                            None,
                        ),
                        ChatChunk::Error(error) => {
                            emit_web_generation_event(&app, &generation_id_for_task, "error", format!("模型连接失败：{error}"), None);
                            return;
                        }
                        ChatChunk::Done(reason) => {
                            done_reason = reason;
                            break;
                        }
                    }
                }

                if page_closed {
                    log(
                        &app,
                        &generation_id_for_task,
                        "检测到 </html> 已提前停止接收本轮输出，避免模型重复输出整段逻辑。",
                    );
                }

                // 续写轮次要清理：模型时常重新输出整篇文档或带上代码围栏。
                if continuation_count > 0 {
                    html = webgen::merge_continuation(&html, round_start);
                    last_reported = html.chars().count().min(last_reported);
                }

                // 收尾修补：模型续写时经常漏掉 `</script>`，JS 本身完整，但浏览器会把
                // 后面所有内容都当成脚本，整段 JS 报错、页面失去交互。原先只检查
                // `<html>` / `</html>`，这种"看着成功、其实是死页面"的产出会直接通过。
                let mut cleaned = webgen::repair_html(
                    html.trim()
                        .trim_start_matches("```html")
                        .trim_start_matches("```HTML")
                        .trim_start_matches("```")
                        .trim_end_matches("```")
                        .trim(),
                );
                let lower = cleaned.to_ascii_lowercase();
                if lower.contains("<html") && lower.contains("</html>") {
                    // ---------- 确定性收口 ----------
                    //
                    // 先别急着让模型修。实测里有一种残法单靠"补一补"救不回来：单轮被输出上限
                    // 截断后，模型从断点接着写，写着写着**把整份脚本重开了一篇** —— 于是同一份
                    // 脚本里出现两代实现，前一代完整、后一代写到一半就断了。因为后声明的同名
                    // 函数会覆盖前面的，**残破的后一代反而胜出**，页面照样是死的。
                    //
                    // 这种残法是纯规则能确定的：找"同名函数第二次出现"的位置，从那里往回切，
                    // 就能把完整的那一代整份拿回来。视觉部分一个字节都不动，只重写脚本块。
                    if let Some((fixed, reason)) = webgen::restore_script(&cleaned) {
                        log(
                            &app,
                            &generation_id_for_task,
                            &format!("脚本自洽性收口：{reason}。"),
                        );
                        cleaned = fixed;
                    }

                    // 再一道确定性修复：`localStorage.getItem(k).split(...)` 这类读取，键不存在
                    // 时读到 null、`.split` 抛 TypeError，整段初始化中断，页面等于死的。而它
                    // 几乎必然出现在"首次打开"的路径上。纯机械缺陷，直接补括号兜底，不必劳模型。
                    {
                        let merged = webgen::merged_script(&cleaned);
                        if let Some(fixed) = webgen::fix_null_storage_reads(&merged) {
                            log(
                                &app,
                                &generation_id_for_task,
                                "给 localStorage / sessionStorage 的读取补了空值兜底（键不存在时会读到 null，直接当字符串用会抛错）。",
                            );
                            cleaned = webgen::splice_script_body(&cleaned, &fixed);
                        }
                    }

                    // 第三道确定性修复：单文件网页里的站内绝对路径链接（`<a href="/daily">`）。
                    // 产物只有这一个 HTML 文件，站点下没有 `/daily` 这个路径 —— 用户一点就是 404
                    // （实测录屏里正是如此）。纯机械缺陷，直接把 href 改成 hash 路由，不必劳模型。
                    if let Some(fixed) = webgen::fix_dead_links(&cleaned) {
                        log(
                            &app,
                            &generation_id_for_task,
                            "把站内绝对路径链接改成了 hash 路由（#/xxx），避免点开 404。",
                        );
                        cleaned = fixed;
                    }

                    // 第四道确定性修复：用 `alert(...)` 承载页面状态。
                    // 它是唯一常见的"非结构性"严重问题，却没有可行的自动修复路线：补写函数
                    // 治不了，整段重写又装不下。这里把 `alert(x)` 换成页内提示条 `inlineAlert(x)`
                    // 并追加实现 —— 语义严格等价（alert 返回 undefined），只是不再打断用户。
                    {
                        let merged = webgen::merged_script(&cleaned);
                        if let Some(fixed) = webgen::fix_placeholder_alerts(&merged) {
                            log(
                                &app,
                                &generation_id_for_task,
                                "把 alert 弹窗换成了页内提示条（提示内容与时机不变，但不再用系统弹窗承载页面状态）。",
                            );
                            cleaned = webgen::splice_script_body(&cleaned, &fixed);
                        }
                    }

                    // 第四道确定性修复：正则里认错写法的 Unicode 属性名（`\p{Han}`）。
                    // 模型想放宽中文校验时会这么写，而 `Han` 是 script 名，必须写成
                    // `\p{Script=Han}`；写错就抛 `Invalid property name in character class`，
                    // 整段脚本一行不执行。纯机械替换。
                    {
                        let merged = webgen::merged_script(&cleaned);
                        if let Some(fixed) = webgen::fix_regex_properties(&merged) {
                            log(
                                &app,
                                &generation_id_for_task,
                                "把正则里认错写法的 Unicode 属性名改对了（如 \\p{Han} → \\p{Script=Han}），否则浏览器会因它抛语法错、整段脚本不执行。",
                            );
                            cleaned = webgen::splice_script_body(&cleaned, &fixed);
                        }
                    }

                    // 第四道确定性修复：用 ASCII 的 `\w` 校验中文输入（`/^[^\w\s]+$/`）。
                    // `\w` 不含汉字，于是「要不要辞职去旅行」被判成非法 —— 用户输入什么都点不出
                    // 结果。这类判据出现位置固定，补一个 CJK 区段即可，不必劳模型。
                    {
                        let merged = webgen::merged_script(&cleaned);
                        if let Some(fixed) = webgen::fix_narrow_word_class(&merged) {
                            log(
                                &app,
                                &generation_id_for_task,
                                "把「用 \\w 校验中文输入」的判据改成认汉字的写法（否则中文输入会被判成非法）。",
                            );
                            cleaned = webgen::splice_script_body(&cleaned, &fixed);
                        }
                    }

                    // 第五道确定性修复：脚本里混进来的 Markdown 代码围栏。
                    // 实测第 4 轮：上一轮被截断在半句，续写的下一轮从"开一个代码块"写起，
                    // ```` ```}); ```` 就缝进了脚本第 59 行 —— 反引号在 JS 里开启模板字符串，
                    // 浏览器抛 SyntaxError，**整段脚本一行都不执行**，所有按钮集体失灵。
                    // 括号配平检查看它是"正常"的，只能在这里按脚本块逐个清。
                    if let Some(fixed) = webgen::strip_script_fences(&cleaned) {
                        log(
                            &app,
                            &generation_id_for_task,
                            "清掉了脚本里混进来的 Markdown 代码围栏（它会让整段脚本报语法错、页面全无交互）。",
                        );
                        cleaned = fixed;
                    }

                    // ---------- 功能自检 ----------
                    //
                    // 标签闭合只说明"文件是完整的"，不说明"功能是真的"。实测模型会把页面做得
                    // 看着像那么回事 —— 布局、按钮、提示语齐全 —— 核心逻辑却留一根桩：
                    // "生成名字"被写成 String.fromCharCode(Math.random()*26+97)，页面上滚出
                    // 一串 qpqtmcs，而需求给的词库只被渲染成底部一个装饰性 <ul>。
                    // 这里用纯规则过一遍数据来源与交互绑定，有问题就把证据回灌给模型定向修复。
                    let mut repair_round = 0usize;
                    // 上一轮的加权成本。判"有没有变好"要看它，不能只看问题条数 ——
                    // `UNDEFINED_REFERENCE` 会把"缺 10 个函数"与"缺 2 个函数"压成同一条，
                    // 只数条数会把"补进 8 个"误判成变差（见 `health_with_html` 说明）。
                    let mut last_health = (usize::MAX, usize::MAX);
                    // 修复本身可能把页面改坏：实测整段重写会把"功能缺一块"变成"点了没反应"
                    // （重写时把某个函数的调用写进去了、定义却没写）。所以每轮自检后留一份
                    // "至今最好"的快照，循环结束时若最后一版更差就回滚。
                    // 判据是加权致命度（`webgen::health`），不是问题条数 —— 1 条悬空调用
                    // 比 2 条死函数严重得多。
                    let mut best: Option<(String, (usize, usize))> = None;
                    loop {
                        let issues = webgen::verify_page(&cleaned, &spec);
                        let health = webgen::health_with_html(&cleaned, &issues);
                        if best
                            .as_ref()
                            .map_or(true, |(_, best_health)| health < *best_health)
                        {
                            best = Some((cleaned.clone(), health));
                        }
                        if issues.is_empty() {
                            log(
                                &app,
                                &generation_id_for_task,
                                "功能自检通过：数据来源、交互绑定与需求覆盖都正常。",
                            );
                            break;
                        }
                        let critical = issues
                            .iter()
                            .filter(|issue| issue.severity == webgen::Severity::Critical)
                            .count();
                        log(
                            &app,
                            &generation_id_for_task,
                            &format!(
                                "功能自检：发现 {} 项问题，其中 {critical} 项严重。",
                                issues.len()
                            ),
                        );
                        for issue in &issues {
                            log(&app, &generation_id_for_task, &issue.log_line());
                        }
                        // 有严重问题，或者剩下的是"可以就地补"的（缺函数 / 按钮没绑事件）。
                        // 只看严重问题会漏掉后者：那次抢救把损坏段连同尾部的绑定代码一起截掉，
                        // 页面画得出来、按钮点了没反应，而自检只报了提示级问题，于是被直接交付。
                        let plan = webgen::function_appendix_plan(&issues);
                        if critical == 0 && plan.is_none() {
                            log(
                                &app,
                                &generation_id_for_task,
                                "没有需要修复的严重问题，保留当前版本。",
                            );
                            break;
                        }
                        if repair_round >= MAX_FUNCTION_REPAIR_ROUNDS {
                            log(
                                &app,
                                &generation_id_for_task,
                                &format!(
                                    "已定向修复 {MAX_FUNCTION_REPAIR_ROUNDS} 轮仍有严重问题，先交付当前版本，请人工核对上面列出的问题。"
                                ),
                            );
                            break;
                        }
                        if repair_round > 0 && health >= last_health {
                            log(
                                &app,
                                &generation_id_for_task,
                                "修复没有减少问题，停止继续修复以免越改越差。",
                            );
                            break;
                        }
                        last_health = health;

                        // 整段重写前的容量预检。
                        // 重写一遍脚本所需的输出 token 若超过本轮输出上限，模型必然写到一半
                        // 断掉、交出残品 —— 实测页面脚本 8827 字符需约 4200 token，而输出上限
                        // 只有 3928，重写版少写了一个 `createCard` 函数，整个页面变成"点了没反应"。
                        // 装不下就别开始：保留当前版本，如实记录。
                        if plan.is_none() {
                            let (probe_system, probe_prompt) =
                                webgen::script_repair_prompt(&spec, &cleaned, &issues);
                            let cap = webgen::budget_for_prompt(&format!(
                                "{probe_system}{probe_prompt}"
                            ))
                            .num_predict;
                            let need = webgen::script_token_estimate(&cleaned);
                            if need as i64 > cap {
                                log(
                                    &app,
                                    &generation_id_for_task,
                                    &format!(
                                        "重写脚本约需 {need} token，超过本轮输出上限 {cap} token，装不下，跳过重写并保留当前版本。"
                                    ),
                                );
                                break;
                            }
                        }

                        // ---------- 定向修复 ----------
                        // 两条路线，按问题性质选：
                        // ① 缺函数 / 按钮没绑事件 → **就地补写**，把那几行追加到脚本末尾。
                        //    8B 模型写三五个小函数是做得到的，而重写一万五千字的脚本不可能
                        //    —— 修复轮的输出上限连原稿都放不下，只会交出更残的东西。
                        // ② 数据声明了却没被读取（`UNUSED_DATA_POOL`）→ **定点替换相关函数**。
                        //    缺陷是局部的，手段也必须是局部的：重写整段脚本装不下、还会顺手写坏。
                        // ③ 其它严重问题（脚本被截断、重复声明、假数据桩）→ 整段重写脚本块。
                        //    要改的是脚本里那几十行；让模型重写整页既慢（8GB 显存下得动用溢出到
                        //    CPU 的大窗口），又会顺手把已经正确的配色和布局改坏。
                        let missing = webgen::missing_function_names(&cleaned);
                        let appendix_route = plan.is_some();
                        let pool_patch = if appendix_route {
                            None
                        } else {
                            webgen::function_patch_plan(&cleaned, &issues)
                        };
                        let wiring_route = pool_patch.is_some();
                        let need_bindings = plan.unwrap_or(false);
                        let (system, prompt, repair_options) = if appendix_route {
                            let (system, prompt) = webgen::missing_function_prompt(
                                &spec,
                                &cleaned,
                                &missing,
                                need_bindings,
                            );
                            let options = serde_json::json!({
                                "num_ctx": webgen::NUM_CTX,
                                "num_predict": webgen::APPENDIX_NUM_PREDICT,
                                "repeat_penalty": webgen::REPEAT_PENALTY,
                                "repeat_last_n": webgen::REPEAT_LAST_N,
                            });
                            (system, prompt, options)
                        } else if let Some(pools) = &pool_patch {
                            let (system, prompt) =
                                webgen::data_wiring_prompt(&spec, &cleaned, pools);
                            let options = serde_json::json!({
                                "num_ctx": webgen::NUM_CTX,
                                "num_predict": webgen::WIRING_NUM_PREDICT,
                                "repeat_penalty": webgen::REPEAT_PENALTY,
                                "repeat_last_n": webgen::REPEAT_LAST_N,
                            });
                            (system, prompt, options)
                        } else {
                            let (system, prompt) =
                                webgen::script_repair_prompt(&spec, &cleaned, &issues);
                            let budget = webgen::budget_for_prompt(&format!("{system}{prompt}"));
                            let options = serde_json::json!({
                                "num_ctx": budget.num_ctx,
                                "num_predict": budget.num_predict,
                                "repeat_penalty": webgen::REPEAT_PENALTY,
                                "repeat_last_n": webgen::REPEAT_LAST_N,
                            });
                            (system, prompt, options)
                        };
                        if appendix_route {
                            let mut parts: Vec<String> = Vec::new();
                            if !missing.is_empty() {
                                parts.push(format!("补写 {} 个缺失函数（{}）", missing.len(), missing.join(" / ")));
                            }
                            if need_bindings {
                                parts.push("补上按钮的事件绑定".to_string());
                            }
                            log(
                                &app,
                                &generation_id_for_task,
                                &format!(
                                    "开始第 {} 轮定向修复：{}。",
                                    repair_round + 1,
                                    parts.join("，")
                                ),
                            );
                        } else if let Some(pools) = &pool_patch {
                            log(
                                &app,
                                &generation_id_for_task,
                                &format!(
                                    "开始第 {} 轮定向修复：定点接线，把 {} 真正接进生成逻辑。",
                                    repair_round + 1,
                                    pools.join(" / ")
                                ),
                            );
                        } else {
                            log(
                                &app,
                                &generation_id_for_task,
                                &format!(
                                    "开始第 {} 轮定向修复：整段重写脚本块（窗口 {} token，单轮输出 {} token）。",
                                    repair_round + 1,
                                    repair_options["num_ctx"],
                                    repair_options["num_predict"]
                                ),
                            );
                        }
                        let messages = vec![
                            OllamaMessage::system(&system),
                            OllamaMessage::user(&prompt),
                        ];
                        let mut stream = match client
                            .chat_stream_with_options(&model, messages, 0.2, repair_options)
                            .await
                        {
                            Ok(stream) => stream,
                            Err(error) => {
                                log(
                                    &app,
                                    &generation_id_for_task,
                                    &format!("修复请求启动失败，保留当前版本：{error:#}"),
                                );
                                break;
                            }
                        };
                        let mut patch = String::new();
                        let mut repair_broken = false;
                        while let Some(chunk) = futures_util::StreamExt::next(&mut stream).await {
                            match chunk {
                                ChatChunk::Text(text) => patch.push_str(&text),
                                ChatChunk::Error(error) => {
                                    log(
                                        &app,
                                        &generation_id_for_task,
                                        &format!("修复请求中断，保留当前版本：{error}"),
                                    );
                                    repair_broken = true;
                                    break;
                                }
                                ChatChunk::Done(_) => break,
                                ChatChunk::ToolCall(_) => {}
                            }
                        }
                        if repair_broken {
                            break;
                        }

                        // 修复轮自己也会被输出上限截断，交回来一段残脚本。接上去只会更糟，
                        // 所以残脚本一律拒收（实测接过一次，括号问题从"多一个 )"变成
                        // "整个 ( 没闭合"）。
                        let next = if appendix_route {
                            match webgen::tidy_function_appendix(&patch) {
                                Some(definitions) => {
                                    if let Some(problem) = webgen::delimiter_problem(&definitions) {
                                        log(
                                            &app,
                                            &generation_id_for_task,
                                            &format!("补写的函数自身就是残的（{problem}），拒收，保留当前版本。"),
                                        );
                                        None
                                    } else {
                                        // 追加的定义若和已有函数同名，会把原本正确的实现覆盖掉。
                                        let conflicts =
                                            webgen::appendix_conflicts(&cleaned, &definitions);
                                        if !conflicts.is_empty() {
                                            log(
                                                &app,
                                                &generation_id_for_task,
                                                &format!("补写内容会覆盖页面已有的函数（{}），拒收，保留当前版本。", conflicts.join(" / ")),
                                            );
                                            None
                                        } else {
                                            Some(webgen::append_to_script(&cleaned, &definitions))
                                        }
                                    }
                                }
                                None => {
                                    log(
                                        &app,
                                        &generation_id_for_task,
                                        "模型这一轮没有返回可用的函数定义，保留当前版本。",
                                    );
                                    None
                                }
                            }
                        } else if wiring_route {
                            // 定点接线：模型只回几个函数的完整新定义，逐个替换回脚本里。
                            let replacements = webgen::tidy_function_patch(&patch);
                            if replacements.is_empty() {
                                log(
                                    &app,
                                    &generation_id_for_task,
                                    "模型这一轮没有返回可用的函数定义，保留当前版本。",
                                );
                                None
                            } else if let Some((name, problem)) = replacements
                                .iter()
                                .find_map(|(name, definition)| {
                                    webgen::delimiter_problem(definition)
                                        .map(|problem| (name.clone(), problem))
                                })
                            {
                                log(
                                    &app,
                                    &generation_id_for_task,
                                    &format!("返回的函数 {name} 自身就是残的（{problem}），拒收，保留当前版本。"),
                                );
                                None
                            } else {
                                let names: Vec<String> =
                                    replacements.iter().map(|(name, _)| name.clone()).collect();
                                match webgen::replace_functions(&cleaned, &replacements) {
                                    Some(page) => {
                                        log(
                                            &app,
                                            &generation_id_for_task,
                                            &format!("已替换函数：{}。", names.join(" / ")),
                                        );
                                        Some(page)
                                    }
                                    None => {
                                        log(
                                            &app,
                                            &generation_id_for_task,
                                            "要改的函数在脚本里找不到对应声明（可能是箭头函数），保留当前版本。",
                                        );
                                        None
                                    }
                                }
                            }
                        } else {
                            match webgen::tidy_script_block(&patch) {
                                Some(block) => {
                                    if let Some(problem) = webgen::delimiter_problem(&block) {
                                        log(
                                            &app,
                                            &generation_id_for_task,
                                            &format!("模型这一轮返回的脚本自身就是残的（{problem}），拒收，保留当前版本。"),
                                        );
                                        None
                                    } else {
                                        Some(webgen::splice_script(&cleaned, &block))
                                    }
                                }
                                None => {
                                    log(
                                        &app,
                                        &generation_id_for_task,
                                        "模型这一轮没有返回可用的脚本块，保留当前版本。",
                                    );
                                    None
                                }
                            }
                        };
                        let Some(spliced) = next else {
                            break;
                        };
                        if spliced == cleaned {
                            log(&app, &generation_id_for_task, "修复没有带来任何改动，停止。");
                            break;
                        }
                        cleaned = spliced;
                        repair_round += 1;
                        log(
                            &app,
                            &generation_id_for_task,
                            &format!("第 {repair_round} 轮修复已应用，重新做功能自检。"),
                        );
                    }

                    // 修复越改越差就退回去。宁可交付"功能缺一块、但点得动"的版本，
                    // 也不要"看着齐全、点了没反应"的版本 —— 后者更隐蔽。
                    if let Some((snapshot, _)) = best {
                        if snapshot != cleaned {
                            log(
                                &app,
                                &generation_id_for_task,
                                "修复后的版本比修复前更差，已回滚到问题更少的那一版。",
                            );
                            cleaned = snapshot;
                        }
                    }

                    // 交付前的兜底抢救：修复轮本身也可能被输出上限截断，于是又留下一份残脚本。
                    // 残缺的尾巴会带走整段脚本的执行（SyntaxError），丢掉它至少页面是活的。
                    if let Some(problem) = webgen::delimiter_problem(&webgen::merged_script(&cleaned)) {
                        match webgen::restore_script(&cleaned) {
                            Some((fixed, reason)) => {
                                log(
                                    &app,
                                    &generation_id_for_task,
                                    &format!("脚本仍不完整（{problem}），已{reason}。"),
                                );
                                cleaned = fixed;
                            }
                            None => log(
                                &app,
                                &generation_id_for_task,
                                &format!("脚本不完整（{problem}），自动抢救失败，请人工检查。"),
                            ),
                        }
                    }

                    emit_web_generation_event(
                        &app,
                        &generation_id_for_task,
                        "complete",
                        format!("网页生成完成，共 {} 字符。", cleaned.chars().count()),
                        Some(cleaned),
                    );
                    return;
                }

                let grew = html.chars().count().saturating_sub(round_start);
                if grew < CONTINUATION_MIN_GAIN {
                    // 续写没产出新内容，再试也是白等 —— 但**不要**把已经攒下的大半个页面
                    // 连同错误一起丢掉。实测第 1+2 轮已经攒了 15090 字符（词库、布局、
                    // 大部分逻辑都在），只因第 3 轮返回空就整体作废，用户什么都拿不到。
                    // 这里改成能救就救：补齐闭合标签、跑一遍确定性收口，把当前内容交付出去，
                    // 并把剩下查到的问题一条条列进日志交给人工核对。
                    let partial = webgen::repair_html(
                        html.trim()
                            .trim_start_matches("```html")
                            .trim_start_matches("```HTML")
                            .trim_start_matches("```")
                            .trim_end_matches("```")
                            .trim(),
                    );
                    if partial.to_ascii_lowercase().contains("<html") {
                        let mut salvaged = partial;
                        if let Some((fixed, reason)) = webgen::restore_script(&salvaged) {
                            log(
                                &app,
                                &generation_id_for_task,
                                &format!("续写中断，脚本收口：{reason}。"),
                            );
                            salvaged = fixed;
                        }
                        if let Some(fixed) =
                            webgen::fix_null_storage_reads(&webgen::merged_script(&salvaged))
                        {
                            salvaged = webgen::splice_script_body(&salvaged, &fixed);
                        }
                        if let Some(fixed) = webgen::fix_dead_links(&salvaged) {
                            salvaged = fixed;
                        }
                        // 收口可能把断点留在围栏或半句模板字符串上：这两处都会让整段脚本
                        // 报 SyntaxError，页面等于死的。清掉围栏后再看一遍语法级判据。
                        if let Some(fixed) =
                            webgen::fix_narrow_word_class(&webgen::merged_script(&salvaged))
                        {
                            salvaged = webgen::splice_script_body(&salvaged, &fixed);
                        }
                        if let Some(fixed) =
                            webgen::fix_placeholder_alerts(&webgen::merged_script(&salvaged))
                        {
                            salvaged = webgen::splice_script_body(&salvaged, &fixed);
                        }
                        if let Some(fixed) =
                            webgen::fix_regex_properties(&webgen::merged_script(&salvaged))
                        {
                            salvaged = webgen::splice_script_body(&salvaged, &fixed);
                        }
                        if let Some(fixed) = webgen::strip_script_fences(&salvaged) {
                            salvaged = fixed;
                        }
                        let lower_salvaged = salvaged.to_ascii_lowercase();
                        if !lower_salvaged.contains("</body>") {
                            salvaged.push_str("\n</body>");
                        }
                        if !lower_salvaged.contains("</html>") {
                            salvaged.push_str("\n</html>");
                        }
                        let issues = webgen::verify_page(&salvaged, &spec);
                        log(
                            &app,
                            &generation_id_for_task,
                            &format!(
                                "续写中断（停止原因：{}），已交付当前可救的版本，仍有 {} 项问题待人工核对。",
                                done_reason.clone().unwrap_or_else(|| "未知".to_string()),
                                issues.len()
                            ),
                        );
                        for issue in &issues {
                            log(&app, &generation_id_for_task, &issue.log_line());
                        }
                        let count = salvaged.chars().count();
                        emit_web_generation_event(
                            &app,
                            &generation_id_for_task,
                            "complete",
                            format!(
                                "模型未能续写完成，已交付当前版本（{count} 字符，其中 {} 项问题已记入日志）。",
                                issues.len()
                            ),
                            Some(salvaged),
                        );
                        return;
                    }
                    emit_web_generation_event(
                        &app,
                        &generation_id_for_task,
                        "error",
                        format!("模型未继续输出网页内容（停止原因：{}）。可简化需求后重试。", done_reason.unwrap_or_else(|| "未知".to_string())),
                        None,
                    );
                    return;
                }

                if continuation_count >= MAX_CONTINUATION_ROUNDS {
                    emit_web_generation_event(
                        &app,
                        &generation_id_for_task,
                        "error",
                        format!(
                            "已自动续写 {MAX_CONTINUATION_ROUNDS} 次仍未写完（当前 {} 字符，停止原因：{}）。请精简需求后重试。",
                            html.chars().count(),
                            done_reason.unwrap_or_else(|| "未知".to_string())
                        ),
                        None,
                    );
                    return;
                }
                // 本轮是被输出上限截断的（stop reason = length）时，累积内容末尾很可能停在
                // 半个 token 上 —— 实测停过 `function showToast(message`，续写从这儿接上
                // 一个 `}`，得到 `function showToast(message}`，JS 直接语法错误、整页失效。
                // 续写之前先退回最后一个完整行，让模型从一个干净的边界往下写。
                if done_reason.as_deref() == Some("length") {
                    if let Some(index) = html.rfind('\n') {
                        html.truncate(index + 1);
                    }
                }

                continuation_count += 1;
                emit_web_generation_event(
                    &app,
                    &generation_id_for_task,
                    "log",
                    format!(
                        "本轮输出达到上限（{} 字符），开始第 {continuation_count} 次自动续写。",
                        html.chars().count()
                    ),
                    None,
                );
            }
        });

        Ok(())
    }

    /// 把生成好的网页发布到公网静态托管，返回可以直接打开的链接。
    ///
    /// 通道按令牌自动选择：配了 GitHub 令牌就走 GitHub Pages（免费、链接固定），
    /// 否则再用 Netlify。两边都是首次自动建站、之后复用，所以链接不会变。
    #[tauri::command]
    pub async fn publish_webpage(
        state: State<'_, AppState>,
        html: String,
        file_name: String,
    ) -> Result<crate::webpublish::PublishOutcome, String> {
        if html.trim().is_empty() {
            return Err("还没有生成网页，先点「根据文档生成网页」。".to_string());
        }
        let (github_token, github_repo, netlify_token, netlify_site, proxy) = {
            let db = state.db.lock().await;
            let read = |key: &str| db.get_setting(key).ok().flatten().unwrap_or_default();
            (
                read("publish.github_token"),
                read("publish.github_repo"),
                read("publish.netlify_token"),
                db.get_setting("publish.netlify_site").ok().flatten(),
                read("publish.proxy"),
            )
        };

        // 发布走哪个代理必须可控：reqwest 默认会捡起 HTTP_PROXY / HTTPS_PROXY 环境变量，
        // 而进程一旦继承了别处注入的代理变量，就会连到一个根本不存在的端口上，
        // 报出来只有一句 "error sending request"，极难定位。所以默认显式直连，
        // 用户真需要代理时在设置里填地址，那时以填的为准。
        let builder = reqwest::Client::builder().timeout(std::time::Duration::from_secs(120));
        let proxy = proxy.trim();
        let builder = if proxy.is_empty() {
            builder.no_proxy()
        } else {
            builder.proxy(
                reqwest::Proxy::all(proxy).map_err(|error| format!("代理地址填错了：{error}"))?,
            )
        };
        let client = builder
            .build()
            .map_err(|error| format!("创建网络客户端失败：{error}"))?;

        if !github_token.trim().is_empty() {
            let repo = Some(github_repo.as_str());
            let outcome =
                crate::webpublish::publish_github_pages(&client, &github_token, repo, &html)
                    .await?;
            return Ok(outcome);
        }

        if netlify_token.trim().is_empty() {
            return Err(
                "还没有配置发布通道。到「设置 → 网页发布」填一个 GitHub 访问令牌，生成完就会自动发布并给出网址。"
                    .to_string(),
            );
        }

        let outcome = crate::webpublish::publish_netlify(
            &client,
            &netlify_token,
            netlify_site.as_deref(),
            &file_name,
            &html,
        )
        .await?;

        // 站点 id 只在新站点上回填一次；已有站点不必反复写库。
        if netlify_site.as_deref() != Some(outcome.site.as_str()) {
            let db = state.db.lock().await;
            let _ = db.update_setting("publish.netlify_site", &outcome.site);
        }

        Ok(outcome)
    }

    /// 用系统默认浏览器打开一个链接。
    ///
    /// 只放行 http/https：这个命令的唯一用途是把刚发布出去的网页地址交给浏览器。
    #[tauri::command]
    pub async fn open_external(url: String) -> Result<(), String> {
        if !(url.starts_with("http://") || url.starts_with("https://")) {
            return Err("只支持打开 http/https 链接。".to_string());
        }
        #[cfg(target_os = "windows")]
        {
            std::process::Command::new("cmd")
                .args(["/C", "start", "", &url])
                .spawn()
                .map_err(|error| format!("打开浏览器失败：{error}"))?;
        }
        #[cfg(target_os = "macos")]
        {
            std::process::Command::new("open")
                .arg(&url)
                .spawn()
                .map_err(|error| format!("打开浏览器失败：{error}"))?;
        }
        #[cfg(all(unix, not(target_os = "macos")))]
        {
            std::process::Command::new("xdg-open")
                .arg(&url)
                .spawn()
                .map_err(|error| format!("打开浏览器失败：{error}"))?;
        }
        Ok(())
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
    pub async fn get_settings(state: State<'_, AppState>) -> Result<Vec<SettingItem>, String> {
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
    pub fn toggle_mini_window_inner(
        app_handle: &tauri::AppHandle,
    ) -> Result<(), Box<dyn std::error::Error>> {
        if let Some(window) = app_handle.get_webview_window("mini") {
            match window.is_visible() {
                Ok(true) => {
                    let _ = window.hide();
                }
                _ => {
                    let _ = window.show();
                    let _ = window.set_focus();
                }
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
        tokio::task::spawn_blocking(crate::monitoring::get_system_metrics)
            .await
            .map_err(|e| format!("系统指标查询失败: {}", e))
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
            state
                .minimize_to_tray
                .store(value == "true", Ordering::SeqCst);
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

        let mut stmt = conn
            .prepare(
                "SELECT id, timestamp, session_id, tool_name, tool_risk_level, \
             permission_result, success, error_code, error_message, duration_ms \
             FROM audit_logs ORDER BY timestamp DESC LIMIT ?1",
            )
            .map_err(|e| e.to_string())?;

        let logs = stmt
            .query_map(params![limit], |row| {
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
            })
            .map_err(|e| e.to_string())?;

        logs.collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())
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
        task_mgr
            .list_by_session(&conversation_id, limit.unwrap_or(20))
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
        task_mgr
            .get_active_by_session(&conversation_id)
            .map_err(|e| e.to_string())
    }

    /// 取消会话的活跃任务
    #[tauri::command]
    pub async fn cancel_task(
        state: State<'_, AppState>,
        conversation_id: String,
    ) -> Result<(), String> {
        {
            let agent = state.agent.lock().await;
            agent.cancel_session(&conversation_id).await;
        }
        let db = state.db.lock().await;
        let task_mgr = TaskManager::new(db.connection());
        task_mgr
            .cancel_active_by_session(&conversation_id)
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
        let memories =
            MemoryEngine::list_by_type(db.connection(), mtype, limit.unwrap_or(50) as usize)
                .map_err(|e| e.to_string())?;
        Ok(memories
            .into_iter()
            .map(|m| MemoryItemResponse {
                id: m.id,
                memory_type: m.memory_type.as_str().to_string(),
                content: m.content,
                importance: m.importance,
                access_count: m.access_count,
                created_at: m.created_at,
                last_accessed_at: m.last_accessed_at,
                source: m.source,
            })
            .collect())
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
        let mtype = memory_type
            .as_deref()
            .map(MemoryType::from_str)
            .unwrap_or(MemoryType::Knowledge);
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
        MemoryEngine::forget_by_id(db.connection(), &memory_id).map_err(|e| e.to_string())
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
        let memories =
            MemoryEngine::recall(db.connection(), &query, mtype, limit.unwrap_or(10) as usize)
                .map_err(|e| e.to_string())?;
        Ok(memories
            .into_iter()
            .map(|m| MemoryItemResponse {
                id: m.id,
                memory_type: m.memory_type.as_str().to_string(),
                content: m.content,
                importance: m.importance,
                access_count: m.access_count,
                created_at: m.created_at,
                last_accessed_at: m.last_accessed_at,
                source: m.source,
            })
            .collect())
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
    pub async fn get_skills(state: State<'_, AppState>) -> Result<Vec<SkillInfo>, String> {
        let _ = state; // 预留：未来从 state 获取
        let mgr = crate::skills::global();
        let skills = if let Some(m) = mgr {
            m.list_skills().await
        } else {
            // 如果全局管理器未初始化，直接加载
            let mgr = crate::skills::SkillManager::new().await;
            mgr.list_skills().await
        };

        Ok(skills
            .into_iter()
            .map(|s| SkillInfo {
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
            })
            .collect())
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

        skill
            .map(|s| SkillInfo {
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
            })
            .ok_or_else(|| format!("技能不存在: {}", skill_id))
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
            mgr.toggle_skill(&skill_id, enabled)
                .await
                .map_err(|e| e.to_string())
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
            let skill = mgr
                .install_skill(&source_dir)
                .await
                .map_err(|e| e.to_string())?;
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
            mgr.uninstall_skill(&skill_id)
                .await
                .map_err(|e| e.to_string())
        } else {
            Err("技能管理器未初始化".to_string())
        }
    }
}
