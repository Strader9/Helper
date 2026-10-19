//! PC Guardian AI - 核心库
//!
//! 三层闸门架构：ToolRegistry → PermissionManager → ToolExecutor
//! LLM 永不直接接触系统 API

pub mod agent;
pub mod autostart;
pub mod browser;
pub mod computer;
pub mod db;
pub mod error;
pub mod evidence;
pub mod knowledge;
pub mod llm;
pub mod memory;
pub mod monitoring;
pub mod proactive;
pub mod scheduler;
pub mod security;
pub mod shortcut;
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
    ///
    /// 用 Arc<Mutex<>> 而非普通 String：用户改快捷键时需要热重注册，
    /// 注册结果（可能回退到备用键）要能写回这里，供下次热切换时注销旧键。
    pub global_shortcut: Arc<Mutex<String>>,
    /// Phase 4: 待处理的权限确认请求（request_id → PendingPermission）
    pub pending_permissions: Arc<Mutex<HashMap<String, PendingPermission>>>,
    /// V18: Proactive 主动监控引擎
    pub proactive_engine: Arc<ProactiveEngine>,
    /// 常驻调度器：到点自动执行定时作业
    pub scheduler: Arc<crate::scheduler::Scheduler>,
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

        // 把老库中预设规则的通知文案升级为 `{threshold}` 模板。
        // 只动预设规则、且只动仍含旧硬编码数字的那些，用户自写的文案不碰。
        match crate::proactive::upgrade_preset_messages(db.connection()) {
            Ok(n) if n > 0 => eprintln!("[Proactive] 升级了 {} 条预设规则的通知文案", n),
            _ => {}
        }

        // V17: 设置全局数据库路径（供 MemoryEngine 工具层使用）
        crate::memory::set_db_path(db_path.clone());
        // 自省工具要能回答"这个助手跑了多久"
        crate::tools::executors::client::mark_startup();

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

        // 开机自启**不在启动路径上查注册表**。
        //
        // 早先的写法是启动时调 `autostart::is_enabled()` 拿系统真实状态回写库，
        // 好处是用户若在"任务管理器 → 启动"里手动禁用过，界面不会说谎。
        // 但代价是：**每次启动都必须能成功执行 reg.exe**，一旦它在某些环境下
        // 不可用（受限沙箱、组策略禁用、安全软件拦截），整个应用启动就失败了——
        // 为一个开关付出"起不来"的代价，明显不成比例。
        //
        // 现在的策略：库里存的值就是界面上显示的值；只有用户**主动拨动**
        // 那个开关时才去写注册表（见 update_setting 里的处理），写入成功才落库、
        // 失败就把错误显示给用户。系统侧的变动改由设置页的"重新检测"按钮按需同步。

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

        // 模型协议与凭据。protocol 为 "openai" 时走 OpenAI 兼容端点
        // （可指向 sidecar 网关或任意云模型），否则走 Ollama 原生协议。
        let protocol = db
            .get_setting("llm.protocol")?
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| "ollama".to_string());
        let api_key = db
            .get_setting("llm.api_key")?
            .filter(|value| !value.trim().is_empty());

        // 浏览器自动化：把设置里的偏好注入 BrowserManager 单例。
        //
        // BrowserManager 是 OnceLock 单例，创建时机不确定（首次有浏览器工具
        // 调用时才初始化），所以必须在这里提前注入，否则它会用硬编码默认值。
        // 注入失败（已注入过）不算错误——单例语义就是"只认第一份"。
        {
            let default_browser = db
                .get_setting("browser.default")
                .ok()
                .flatten()
                .unwrap_or_else(|| "auto".to_string());
            let cdp_port = db
                .get_setting("browser.cdp_port")
                .ok()
                .flatten()
                .and_then(|v| v.trim().parse::<u16>().ok())
                .filter(|p| *p >= 1024)
                .unwrap_or(9222);
            if !crate::browser::init_prefs(crate::browser::BrowserPrefs {
                default_browser,
                cdp_port,
            }) {
                eprintln!("[Browser] 偏好已注入过，跳过");
            }
        }

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

        let ollama = if protocol == "openai" {
            Arc::new(OllamaClient::openai_compat(ollama_url, api_key, timeout_ms))
        } else {
            Arc::new(OllamaClient::new(ollama_url, timeout_ms))
        };

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

        // 常驻调度器（同样在 main.rs setup 中启动）
        let scheduler = Arc::new(crate::scheduler::Scheduler::new(app.handle().clone()));

        Ok(Self {
            db: db_arc,
            tool_registry,
            tool_executor,
            permission_manager,
            agent: Arc::new(Mutex::new(agent)),
            ollama,
            ollama_model,
            global_shortcut: Arc::new(Mutex::new(global_shortcut)),
            pending_permissions,
            proactive_engine,
            scheduler,
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
    /// 定向修改（点选了元素）且整页超长时，围绕目标元素回传的片段窗口。
    ///
    /// 比"开头片段"大一倍：这一段的唯一使命就是让模型看清"要改哪儿、周围长什么样"，
    /// 窗口越大，模型越不容易因为看不见邻近结构而改坏东西。
    const WEBGEN_PREV_HTML_PATCH_WINDOW: usize = 6_000;
    /// 单轮输出被封顶后，最多自动续写几次。
    /// 每轮约一万多字符，六轮足够覆盖一个信息量很大的页面。
    const MAX_CONTINUATION_ROUNDS: usize = 6;
    /// 续写轮次至少要新增这么多字符才算"有进展"，否则判定为模型卡住。
    const CONTINUATION_MIN_GAIN: usize = 40;
    /// 功能自检不通过时，最多定向修复几次。
    ///
    /// 修复只重写 `<script>` 块（几行到几十行），不碰 HTML/CSS，所以在标准窗口里就能跑完，
    /// 一轮约一分钟。留五轮，并且要求后一轮必须比前一轮的加权成本更低才继续 ——
    /// 没有进展就停，免得越改越差。
    ///
    /// 为什么是五轮而不是三轮：交付闸门收紧之后，"按钮没绑事件""功能只写空壳"这类问题
    /// 也会触发返修，返修面比过去宽。这三类问题的收敛不是一步到位的 ——
    /// 走"就地补写"时补进去的函数自己还会调用页面里本来就没写的小助手
    /// （实测补了 10 个函数，其中两个又引出 `hash` / `randomPick`），天然需要**多级收敛**。
    /// 三轮会在还剩两个缺失函数时就停下，页面照旧点不动；五轮给足收敛空间。
    const MAX_FUNCTION_REPAIR_ROUNDS: usize = 5;

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
        // 版本链标识。同一份需求的多次修改沿用同一串，这样每次产物都进同一条链、
        // 可以互相回退。前端不传时按需求指纹自动算一条。
        chain_id: Option<String>,
        // 用户在预览里点选出来的"要改哪里"（选择器路径 + 文本摘要）。
        // 只在点选局部修改时带上；不带就是整页/追加式修改。
        target: Option<String>,
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
            let protocol = db
                .get_setting("llm.protocol")
                .map_err(|e| e.to_string())?
                .filter(|value| !value.trim().is_empty())
                .unwrap_or_else(|| "ollama".to_string());
            let api_key = db
                .get_setting("llm.api_key")
                .map_err(|e| e.to_string())?
                .filter(|value| !value.trim().is_empty());
            let client = if protocol == "openai" {
                crate::llm::OllamaClient::openai_compat(base_url, api_key, timeout_ms)
            } else {
                crate::llm::OllamaClient::new(base_url, timeout_ms)
            };
            (client, model)
        };

        // 点选出来的"改哪里"。截断到 600 字：这是给模型的定位提示，不是完整 DOM 导出，
        // 过长的路径描述反而会挤掉需求与修改意见本身的空间。
        //
        // 必须定义在 `previous_payload` 之前 —— 后者要用它来决定回传哪一段源码。
        let target_text = target
            .map(|text| text.trim().to_string())
            .filter(|text| !text.is_empty())
            .map(|text| {
                if text.chars().count() > 600 {
                    text.chars().take(600).collect::<String>()
                } else {
                    text
                }
            });

        // 迭代修改要把现有页面源码回传，这是提示词里最大的一块，超长时只能给片段。
        //
        // 给哪一段很关键：点选局部修改时，**必须让模型看见它要改的那一处**。
        // 早先的实现一律只给开头若干字符，于是当目标元素落在页面后半段时，
        // 模型看不到它，只能凭需求猜着重写 —— 实测出现过"整份照抄、什么都没改"。
        // 现在按用户点选的位置摘一段，把目标元素包在中间。
        //
        // **片段模式（`patch_mode`）**：整页超长时，让模型"看着 1/6 的片段输出完整页面"
        // 是不可能完成的任务 —— 它只能凭空造出剩下的 5/6。实测就是这么坏的：18918 字的
        // 页面回传 3006 字，模型把整页替换成那 3006 字，`<body>` 与 `<script>` 全丢。
        // 所以这条分支改成"模型只输出改后的那一小块，程序负责贴回原位"（`webgen::merge_patch`）。
        let mut patch_mode = false;
        let (previous_payload, previous_note) = match previous_html.as_ref() {
            None => (None, String::new()),
            Some(html) => {
                let chars = html.chars().count();
                if chars <= WEBGEN_PREV_HTML_FULL_LIMIT {
                    (Some(html.clone()), String::new())
                } else if let Some(target) = target_text.as_ref() {
                    // 定向修改：围绕目标元素摘片段，让"改哪里"真的有据可依。
                    let (excerpt, located) = webgen::take_excerpt(
                        html,
                        target,
                        WEBGEN_PREV_HTML_PATCH_WINDOW,
                    );
                    // 定位到了才走片段模式：没定位到就等于不知道改哪儿，
                    // 让模型"只输出一小块"没有依据，会改出别的东西。
                    patch_mode = located;
                    let note = if located {
                        format!(
                            "\n\n（注意：现有页面共约 {chars} 字，这里只回传了包含**你要修改的元素**的片段。）"
                        )
                    } else {
                        format!(
                            "\n\n（注意：现有页面共约 {chars} 字，受上下文长度限制只回传了开头片段，且没能定位到你要修改的元素。请结合上面的元素描述与修改意见进行修改，并保持原有视觉风格与结构。）"
                        )
                    };
                    (Some(excerpt), note)
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
        // 片段模式下提示词与要求都要换一套：不再要"完整页面"，只要"改后的那一小块"。
        let previous_note = if patch_mode {
            format!(
                "{previous_note}\n\n**只输出你改过的那一小块代码**（例如那一个按钮、那一段样式、那一个函数），\
                 不要输出整页，不要输出片段里其余未改动的部分。程序会把这一小块贴回原页对应位置。\
                 除了这一小块之外，什么都不要返回。"
            )
        } else {
            previous_note
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
        // 版本链：前端给了就用它的（同一需求反复改时保持一致），没给就按需求指纹算。
        // 在这里算而不是在后台任务里，是为了让它能被闭包安全捕获。
        let chain = chain_id
            .filter(|id| !id.trim().is_empty())
            .unwrap_or_else(|| webgen::history::chain_id_for(&requirements));
        // 首版与修改版的说明文字不同 —— 列表里要能一眼看出"这一版做了什么"。
        let history_note = feedback_text
            .as_ref()
            .map(|text| {
                let trimmed = text.trim();
                if trimmed.chars().count() > 60 {
                    format!("修改：{}…", trimmed.chars().take(60).collect::<String>())
                } else {
                    format!("修改：{trimmed}")
                }
            })
            .unwrap_or_else(|| "首版".to_string());

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
                // 点选局部修改：先把"改哪里"说清楚，再说"怎么改"。
                //
                // 为什么顺序不能反：8B 模型在长上下文里对**最后出现**的位置信息最敏感。
                // 先说位置、再说要求，它才会把这条要求挂到那个元素上；反过来则容易忽略位置、
                // 把整页重写一遍 —— 而整页重写正是"只想改个主色，结果按钮全点不动"的成因。
                if let Some(target) = target_text.as_ref() {
                    user.push_str(&format!(
                        "\n\n【只改这一处】用户在预览里点选了下面这个元素，请**只针对它**做修改，其余部分原样保留：\n{target}"
                    ));
                }
                // 片段模式下要求也必须是"只给一小块"。
                //
                // 这里是本次修的一个真坑：早先无论哪种模式都写"并返回完整 HTML"，
                // 而片段模式的 `previous_note` 又说"只输出你改过的那一小块"——
                // 两条互斥指令同时摆在一个 8B 模型面前，它两头都不照做：
                // 实测只吐了 30 token 的敷衍内容（既不是完整页面，也不是有效片段）。
                if patch_mode {
                    user.push_str(&format!(
                        "\n\n请根据以下修改意见，**只输出你改动过的那一小块代码**，不要输出整页：\n{feedback}"
                    ));
                } else {
                    user.push_str(&format!(
                        "\n\n请根据以下修改意见，在现有网页基础上修改，并返回完整 HTML：\n{feedback}"
                    ));
                }
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
                // 本轮（或最近一轮）的用量统计，用于日志与生成摘要。
                let mut last_usage: Option<crate::llm::ollama::TokenUsage> = None;
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
                        ChatChunk::Done(reason, usage) => {
                            done_reason = reason;
                            // 用量只在最后一个分片里给。记下来，本轮结束后写进日志与
                            // 生成摘要 —— 这是量化"一次生成消耗多少上下文"的唯一入口，
                            // 也是判断提示词加固有没有效果的基线。
                            if let Some(usage) = usage {
                                if usage.is_measured() {
                                    log(
                                        &app,
                                        &generation_id_for_task,
                                        &format!("本轮用量：{}。", usage.log_line()),
                                    );
                                    last_usage = Some(usage);
                                }
                            }
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

                // ---------- 片段模式合并 ----------
                //
                // 定向修改、整页超长时，模型只返回了"改后的那一小块"，这里要把它贴回原页。
                //
                // 这一步必须**在最前面**做：贴回之后的产物才是一份完整的页面，
                // 后面那些确定性修复（脚本自洽性收口、空值兜底、死链修补……）才有东西可修。
                //
                // 合并失败一律**保留原页**：宁可告诉用户"这次没改成"，
                // 也不能把一份残缺的页面交出去。这正是本次修的那个坑 ——
                // 早先没有这一步，模型返回的 3006 字片段直接顶替了 18918 字的整页。
                if patch_mode {
                    if let (Some(original), Some(target)) =
                        (previous_html.as_ref(), target_text.as_ref())
                    {
                        match webgen::merge_patch(
                            original,
                            target,
                            WEBGEN_PREV_HTML_PATCH_WINDOW,
                            &html,
                        ) {
                            Some(outcome) if outcome.changed => {
                                log(
                                    &app,
                                    &generation_id_for_task,
                                    "定向修改：模型只改了这一处，已由程序贴回原页（其余部分保持原样）。",
                                );
                                html = outcome.html;
                            }
                            Some(_) => {
                                log(
                                    &app,
                                    &generation_id_for_task,
                                    "定向修改：模型没有改动这一处，保留原页面。",
                                );
                                html = original.clone();
                            }
                            None => {
                                log(
                                    &app,
                                    &generation_id_for_task,
                                    "定向修改：改后内容无法安全贴回原页（结构或目标对不上），已保留原页面。请换一种说法再试，或改用整页修改。",
                                );
                                html = original.clone();
                            }
                        }
                    }
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

                    // ---------- 产物退化兜底 ----------
                    //
                    // 最后一道，也是最后一道能救命的：**改写把页面改烂了就该保留原版**。
                    //
                    // 为什么自检拦不住这种：`verify_page` 是按"页面上有的东西"找缺陷的，
                    // 而一份被截断的残品上**什么都没有** —— 于是它报"通过"。
                    // 实测：18918 字的页面被替换成 3006 字片段，`<body>` 与 `<script>` 全丢，
                    // 自检无声通过，用户点"只改一个按钮"结果整页被删掉 84%。
                    //
                    // 判据只看两条硬事实：结构标签没了、体量骤降。
                    if let Some(original) = previous_html.as_ref() {
                        if let Some(reason) = webgen::looks_degraded(original, &cleaned) {
                            log(
                                &app,
                                &generation_id_for_task,
                                &format!(
                                    "改动会让页面退化（{reason}），已保留原页面。请换一种说法再试。"
                                ),
                            );
                            cleaned = original.clone();
                        }
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
                        // 交付闸门（三重）：
                        // ① 有 Critical —— 语法/运行期就崩，必须修；
                        // ② 有可"就地补写"的计划 —— 缺函数、按钮没绑事件；
                        // ③ 有够格的功能性缺陷 —— 页面点得动但功能是空壳（成片死函数、需求成片没落地）。
                        // 第 ③ 条是这次加固的重点：它对应的正是"看着齐全、其实不具备功能"的成品，
                        // 过去因为既不是 Critical 也没有 repair 计划而被直接放行。
                        let functional = webgen::functional_repair_worthwhile(&issues);
                        if critical == 0 && plan.is_none() && !functional {
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
                                ChatChunk::Done(_, usage) => {
                                    // 修复轮同样记用量：修一轮要花多少上下文，直接决定
                                    // "值得修几轮"以及窗口还够不够再修一轮。
                                    if let Some(usage) = usage.filter(|usage| usage.is_measured()) {
                                        log(
                                            &app,
                                            &generation_id_for_task,
                                            &format!("本轮修复用量：{}。", usage.log_line()),
                                        );
                                        last_usage = Some(usage);
                                    }
                                    break;
                                }
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

                    // 交付前的终检：把"这一版到底还有没有问题"如实报出来。
                    //
                    // 这是"具备功能"的可验证指标 —— 自检一旦干净，日志里就只剩一句
                    // "交付版本功能自检通过"；若仍有残留缺陷，用户能直接看到是哪几类，
                    // 而不是打开页面挨个点按钮去发现。用量一并带上，便于对照调教效果。
                    let final_issues = webgen::verify_page(&cleaned, &spec);
                    let functional_left = final_issues
                        .iter()
                        .filter(|issue| webgen::is_functional_defect(issue))
                        .count();
                    let usage_note = last_usage
                        .as_ref()
                        .map(|usage| format!("，{}", usage.log_line()))
                        .unwrap_or_default();
                    if final_issues.is_empty() {
                        log(
                            &app,
                            &generation_id_for_task,
                            &format!(
                                "交付版本功能自检通过：按钮绑定、数据链路、需求覆盖均正常（修复 {repair_round} 轮{usage_note}）。"
                            ),
                        );
                    } else {
                        log(
                            &app,
                            &generation_id_for_task,
                            &format!(
                                "交付版本仍有 {} 项自检问题（其中 {} 项影响功能），已尽力修复 {repair_round} 轮{usage_note}。清单如下：",
                                final_issues.len(),
                                functional_left
                            ),
                        );
                        for issue in &final_issues {
                            log(&app, &generation_id_for_task, &issue.log_line());
                        }
                    }

                    // 存一版快照。放在交付之前而不是之后：`cleaned` 马上要被 move 进
                    // `complete` 事件，而快照必须是**最终交付的这一版**——
                    // 存错了版本，用户点"回退"就会拿到一个从没见过的页面。
                    //
                    // 自检指标跟着一起存：前端列表里要能直观看出"哪一版更健康"，
                    // 否则回退就成了凭运气的操作。
                    let version = webgen::history::push(
                        &chain,
                        cleaned.clone(),
                        history_note.clone(),
                        webgen::health(&final_issues).0,
                        final_issues.len(),
                        functional_left,
                    );
                    log(
                        &app,
                        &generation_id_for_task,
                        &format!("已保存为第 {version} 版，可在「历史版本」里回退。"),
                    );

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
                        // 抢救版同样入链：它是用户实际拿到的东西，必须能回退到它之前的状态。
                        // 缺陷指标据实记录 —— 抢救版通常问题不少，列表里一眼能看出来。
                        let salvage_health = webgen::health_with_html(&salvaged, &issues).0;
                        let salvage_functional = issues
                            .iter()
                            .filter(|issue| webgen::is_functional_defect(issue))
                            .count();
                        let version = webgen::history::push(
                            &chain,
                            salvaged.clone(),
                            format!("{history_note}（续写中断抢救版）"),
                            salvage_health,
                            issues.len(),
                            salvage_functional,
                        );
                        log(
                            &app,
                            &generation_id_for_task,
                            &format!("已保存为第 {version} 版（含未修复问题，可在「历史版本」里回退）。"),
                        );
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

    // ============================================================
    // 网页版本快照
    //
    // 解决"改坏了回不去"：每次生成/修改都留下完整一版，前端可以列出、取回、回退。
    // 快照只活在内存里（见 webgen::history 的模块说明），进程重启即清空。
    // ============================================================

    /// 列出当前版本链的全部版本（摘要，不含 HTML 正文）。
    ///
    /// 不返回正文是刻意的：列表只需要版本号、说明与自检指标，正文可达几十 KB，
    /// 传过去纯属浪费；真要回退时再按版本号取。
    #[tauri::command]
    pub fn list_webpage_versions(chain_id: Option<String>) -> Vec<webgen::history::SnapshotSummary> {
        webgen::history::list(chain_id.as_deref())
    }

    /// 取回指定版本的完整 HTML，供前端回退。
    ///
    /// 取回后前端把它设回预览区与内部状态；下一次修改就基于这一版继续，
    /// 相当于"把这一版重新放回编辑位"。
    #[tauri::command]
    pub fn restore_webpage_version(
        chain_id: Option<String>,
        version: u32,
    ) -> Result<String, String> {
        webgen::history::get(chain_id.as_deref(), version)
            .ok_or_else(|| format!("第 {version} 版已不在历史里（可能已被更早的版本挤出保留窗口）。"))
    }

    /// 按需求文本算版本链标识。
    ///
    /// 前端首次进入某个需求时需要拿到这个串，之后每次生成都带上它，
    /// 才能保证"同一份需求的多次修改"落进同一条链。
    #[tauri::command]
    pub fn webpage_chain_id(requirements: String) -> String {
        webgen::history::chain_id_for(&requirements)
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
    ///
    /// 保存前按该键声明的 value_type 与取值区间校验。校验失败直接返回错误、
    /// **不落库**——此前是静默 clamp，用户在界面上看到自己填的 99999，
    /// 实际生效的却是 300000 上限，属于看不懂的偏差。
    /// 校验交给调用方之前先做，保证库里的值永远在合法区间内。
    #[tauri::command]
    pub async fn update_setting(
        state: State<'_, AppState>,
        app_handle: tauri::AppHandle,
        key: String,
        value: String,
    ) -> Result<(), String> {
        // 校验需要读库，先取锁做完，再决定是否写入。
        {
            let db = state.db.lock().await;
            let bounds = db
                .get_setting_bounds(&key)
                .map_err(|e| e.to_string())?;

            if let Some((value_type, min_value, max_value, unit)) = bounds {
                match value_type.to_uppercase().as_str() {
                    "BOOLEAN" => {
                        if value != "true" && value != "false" {
                            return Err(format!("{} 只能是 true 或 false", key));
                        }
                    }
                    "NUMBER" => {
                        let parsed: f64 = value
                            .trim()
                            .parse()
                            .map_err(|_| format!("{} 需要填一个数字，当前填的是「{}」", key, value))?;
                        if !parsed.is_finite() {
                            return Err(format!("{} 不能填 NaN 或无穷大", key));
                        }
                        if let Some(low) = min_value {
                            if parsed < low {
                                return Err(format!(
                                    "{} 不能小于 {}{}",
                                    key,
                                    low,
                                    unit.as_deref().unwrap_or("")
                                ));
                            }
                        }
                        if let Some(high) = max_value {
                            if parsed > high {
                                return Err(format!(
                                    "{} 不能大于 {}{}",
                                    key,
                                    high,
                                    unit.as_deref().unwrap_or("")
                                ));
                            }
                        }
                    }
                    _ => {}
                }
            }
        }

        // V21: 同步更新内存中的原子变量（避免窗口事件回调读旧值）
        if key == "tray.minimize_on_close" {
            state
                .minimize_to_tray
                .store(value == "true", Ordering::SeqCst);
        }

        // 开机自启要真正落到操作系统的启动项里，只写库不生效。
        //
        // 顺序很关键：**先动系统、成功了才落库**。反过来会出现
        // "库里说已启用、系统里其实没有"的假象。
        if key == "startup.auto_launch" {
            let want = value == "true";
            crate::autostart::apply_serialized(want).map_err(|e| e.to_string())?;
        }

        // 主题切换立即作用于当前窗口，无需重启。
        if key == "ui.theme" {
            // 主题的实际切换由前端监听 settings 变化后自己执行（data-theme 属性），
            // 后端只需保证值合法即可。
            if value != "dark" && value != "light" {
                return Err("ui.theme 只能是 dark 或 light".to_string());
            }
        }

        // Ollama 请求超时立即生效，无需重启。
        //
        // Client 构造时烘死的那个 timeout 改不了，但请求路径已经改成每轮现取
        // 这个原子量，所以在这里 store 一下就能影响之后的所有请求。
        if key == "ollama.timeout_ms" {
            if let Ok(parsed) = value.trim().parse::<u64>() {
                state.ollama.set_timeout_ms(parsed.clamp(5_000, 300_000));
            }
        }

        // MEDIUM 风险确认策略立即生效。
        //
        // 改动前它只在启动时读一次，用户在设置里关掉这个开关后实际仍会弹窗——
        // 一个安全的开关却是个假开关，比没有更糟：用户以为自己关掉了。
        if key == "security.confirm_medium_risk" {
            let mut pm = state.permission_manager.lock().await;
            pm.update_config(value == "true");
        }

        // 全局快捷键立即重注册，无需重启。        //
        // 先注销旧的再注册新的，否则改完之后两个键都能唤起窗口——用户会以为
        // 设置没生效。注册结果可能不是他填的那个（被占用时走回退链），
        // 这时把实际生效值落库，让界面如实显示。
        if key == "global_shortcut" {
            let previous = state.global_shortcut.lock().await.clone();
            let effective = crate::shortcut::register_global_shortcut_at(
                &app_handle,
                value.trim(),
                Some(previous.as_str()),
            );
            *state.global_shortcut.lock().await = effective.clone();

            let db = state.db.lock().await;
            return db
                .update_setting(&key, &effective)
                .map_err(|e| e.to_string());
        }

        let db = state.db.lock().await;
        db.update_setting(&key, &value).map_err(|e| e.to_string())
    }

    /// 查询开机自启在系统里的真实状态
    ///
    /// 单独做成命令而**不在启动时调用**：它要执行 `reg.exe`，而这个外部程序
    /// 在某些受限环境（沙箱、组策略、安全软件）里可能不可用。放在设置页由用户
    /// 按需触发，就不会因为一个查询动作拖垮应用启动。
    ///
    /// 返回 `None` 表示"查不到"（reg 不可用或执行失败），与 `Some(false)`
    /// （确认未启用）区分开，让界面能如实显示"未知"而不是误报"已关闭"。
    #[tauri::command]
    pub fn check_autostart_status() -> Option<bool> {
        crate::autostart::query_or_none()
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

    /// 读取工具回执账本（事实层）。
    ///
    /// 与 `get_audit_logs` 并存而不是替代：审计回答"允许了吗"，
    /// 回执回答"实际改变了什么、有没有证据"。
    #[tauri::command]
    pub async fn get_tool_receipts(
        state: State<'_, AppState>,
        limit: Option<i64>,
        tool_name: Option<String>,
    ) -> Result<Vec<crate::tools::receipt::ToolReceipt>, String> {
        let db = state.db.lock().await;
        db.list_tool_receipts(limit.unwrap_or(100), tool_name.as_deref())
            .map_err(|e| e.to_string())
    }

    /// 一轮任务的真值快照。
    ///
    /// 界面在收到最终回复后调用它：账本里这一轮有没有证据、
    /// 模型那句话能不能算数，都由这里给结论，前端不自己猜。
    #[derive(Debug, Serialize)]
    pub struct TaskTruthResponse {
        pub snapshot: crate::evidence::task_truth::TaskTruthSnapshot,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub claim: Option<crate::evidence::task_truth::ClaimAssessment>,
    }

    #[tauri::command]
    pub async fn get_task_truth(
        state: State<'_, AppState>,
        run_id: String,
        claim_text: Option<String>,
    ) -> Result<TaskTruthResponse, String> {
        let receipts = {
            let db = state.db.lock().await;
            db.list_tool_receipts_by_run(&run_id, 200)
                .map_err(|e| e.to_string())?
        };
        let snapshot = crate::evidence::task_truth::build(Some(&run_id), &receipts);
        let claim = claim_text
            .as_deref()
            .filter(|text| !text.trim().is_empty())
            .map(|text| crate::evidence::task_truth::assess_claim(text, &snapshot));
        Ok(TaskTruthResponse { snapshot, claim })
    }

    /// 能力契约覆盖率：给界面和自检用。
    #[tauri::command]
    pub async fn get_capability_coverage(
        state: State<'_, AppState>,
    ) -> Result<crate::tools::contract::CoverageReport, String> {
        Ok(crate::tools::contract::coverage(
            &state.tool_registry.list_names(),
        ))
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
        /// 数值型配置的合法下界；None 表示无约束（字符串/布尔项恒为 None）
        pub min_value: Option<f64>,
        /// 数值型配置的合法上界；None 表示无约束
        pub max_value: Option<f64>,
        /// 数值单位（%、ms、s、token…），仅用于界面展示
        pub unit: Option<String>,
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
    // 常驻调度命令
    // ============================================================
    //
    // 与工具层（`schedule_job` 等）共用同一套 schema 与校验函数：前端建的任务
    // 和模型建的任务必须是同一种东西，否则两边的边界校验会各说各话。

    /// 列出所有定时任务
    #[tauri::command]
    pub async fn list_scheduled_jobs(
        state: State<'_, AppState>,
    ) -> Result<Vec<crate::scheduler::ScheduledJob>, String> {
        let db = state.db.lock().await;
        crate::scheduler::list_jobs(db.connection())
    }

    /// 新建定时任务
    #[tauri::command]
    pub async fn create_scheduled_job(
        state: State<'_, AppState>,
        name: String,
        kind: String,
        schedule: String,
        goal: String,
        max_runs: Option<i64>,
    ) -> Result<crate::scheduler::ScheduledJob, String> {
        let kind_enum = crate::scheduler::JobKind::from_str(&kind)
            .ok_or_else(|| format!("未知的调度类型：{}", kind))?;
        let normalized = crate::scheduler::normalize_schedule(kind_enum, &schedule)?;

        let db = state.db.lock().await;
        crate::scheduler::insert_job(
            db.connection(),
            crate::scheduler::NewJob {
                name: name.trim(),
                kind: kind_enum,
                schedule: &normalized,
                goal: goal.trim(),
                enabled: true,
                max_runs: max_runs.filter(|v| *v > 0),
            },
        )
    }

    /// 删除定时任务（连带运行历史）
    #[tauri::command]
    pub async fn delete_scheduled_job(
        state: State<'_, AppState>,
        id: String,
    ) -> Result<bool, String> {
        let db = state.db.lock().await;
        crate::scheduler::delete_job(db.connection(), &id)
    }

    /// 启用/暂停定时任务
    #[tauri::command]
    pub async fn set_scheduled_job_enabled(
        state: State<'_, AppState>,
        id: String,
        enabled: bool,
    ) -> Result<(), String> {
        let db = state.db.lock().await;
        crate::scheduler::set_enabled(db.connection(), &id, enabled)
    }

    /// 立刻执行一次（标记到期，调度器下一轮接手）
    #[tauri::command]
    pub async fn run_scheduled_job_now(
        state: State<'_, AppState>,
        id: String,
    ) -> Result<(), String> {
        let db = state.db.lock().await;
        let job = crate::scheduler::get_job(db.connection(), &id)?
            .ok_or_else(|| format!("任务不存在：{}", id))?;
        if !job.enabled {
            return Err("任务已暂停，请先启用".to_string());
        }
        crate::scheduler::mark_due_now(db.connection(), &job)
    }

    /// 查执行历史
    #[tauri::command]
    pub async fn get_scheduled_job_runs(
        state: State<'_, AppState>,
        id: Option<String>,
        limit: Option<i64>,
    ) -> Result<Vec<crate::scheduler::JobRun>, String> {
        let db = state.db.lock().await;
        crate::scheduler::recent_runs(
            db.connection(),
            id.as_deref(),
            limit.unwrap_or(20),
        )
    }

    /// 调度器运行状态
    #[tauri::command]
    pub async fn get_scheduler_status(
        state: State<'_, AppState>,
    ) -> Result<crate::scheduler::SchedulerStatus, String> {
        Ok(state.scheduler.status().await)
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
