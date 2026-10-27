//! Jarvis 运行时：确定性快路径 + 单次自适应工具调用。
//!
//! 高频明确指令不经过模型；其余请求只进行一次“回答或调用工具”的决策。
//! 所有系统操作仍经过 ToolRegistry → PermissionManager → ToolExecutor。

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::Mutex;
use tokio::time::{timeout, Duration};
use tokio_stream::StreamExt;

use crate::db::conversation::{ConversationManager, ConversationMessage};
use crate::db::Database;
use crate::error::AppResult;
use crate::llm::{ChatChunk, OllamaClient, OllamaMessage, ToolCall};
use crate::security::PermissionManager;
use crate::tools::executor::{PendingPermission, ToolCallRequest, ToolExecutor};
use crate::tools::ToolRegistry;
pub mod context;
pub mod history;
pub mod intent;
pub mod jarvis;
pub mod observation;
pub mod recovery;
pub mod task;
pub mod verification;
use recovery::RecoveryEvent;
use task::{ActionStatusEvent, TaskManager, TaskUpdateEvent};

// ============================================================
// ChatEvent（流式推送给前端）
// ============================================================

#[derive(Debug, Clone, serde::Serialize)]
#[serde(tag = "type", content = "data")]
pub enum ChatEvent {
    /// 文本片段
    Text(String),
    /// LLM 开始思考
    Thinking,
    /// 工具调用
    ToolCall {
        name: String,
        arguments: serde_json::Value,
    },
    /// 工具执行结果
    ToolResult {
        name: String,
        success: bool,
        content: String,
    },
    /// 任务状态更新
    TaskUpdate(TaskUpdateEvent),
    /// 动作状态更新
    ActionStatus(ActionStatusEvent),
    /// V14: 恢复事件
    Recovery(RecoveryEvent),
    /// 完成
    Done,
    /// 错误
    Error(String),
}

pub struct AgentRun {
    pub run_id: String,
    pub events: tokio::sync::mpsc::Receiver<ChatEvent>,
}

pub(crate) struct RunCancellation {
    cancelled: AtomicBool,
    notify: tokio::sync::Notify,
}

impl RunCancellation {
    fn new() -> Self {
        Self {
            cancelled: AtomicBool::new(false),
            notify: tokio::sync::Notify::new(),
        }
    }

    pub(crate) fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
        // notify_one 会保留一个 permit，避免“刚检查完 flag、尚未开始 await”时丢通知。
        self.notify.notify_one();
    }

    pub(crate) fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
    }

    pub(crate) async fn cancelled(&self) {
        while !self.is_cancelled() {
            self.notify.notified().await;
        }
    }
}

// ============================================================
// AgentRuntime
// ============================================================

/// 从配置表解析单次推理要用的运行参数。
///
/// 返回 (温度, 最大轮次)。任何一项读不到或解析失败，都回落到
/// `fallback_*`——也就是构造 `AgentRuntime` 时的默认值。
///
/// 之所以独立成函数而不是内联：这段逻辑的正确性直接决定"设置改了到底
/// 有没有用"，必须能脱离 ollama 连接、脱离 tokio 运行时单独测。
fn resolve_run_params(
    db: &Database,
    fallback_temperature: f32,
    fallback_max_iterations: usize,
) -> (f32, usize) {
    let temperature = db
        .get_setting("agent.temperature")
        .ok()
        .flatten()
        .and_then(|v| v.trim().parse::<f32>().ok())
        .filter(|v| v.is_finite())
        .map(|v| v.clamp(0.0, 2.0))
        .unwrap_or(fallback_temperature);

    let max_iterations = db
        .get_setting("agent.max_iterations")
        .ok()
        .flatten()
        .and_then(|v| v.trim().parse::<usize>().ok())
        .map(|v| v.clamp(1, 50))
        .unwrap_or(fallback_max_iterations);

    (temperature, max_iterations)
}

pub struct AgentRuntime {
    ollama: Arc<OllamaClient>,
    tool_registry: Arc<ToolRegistry>,
    #[allow(dead_code)]
    permission_manager: Arc<Mutex<PermissionManager>>,
    executor: Arc<ToolExecutor>,
    model: String,
    temperature: f32,
    max_iterations: usize,
    active_runs: Arc<Mutex<HashMap<String, Arc<RunCancellation>>>>,
}

impl AgentRuntime {
    pub fn new(
        ollama: Arc<OllamaClient>,
        tool_registry: Arc<ToolRegistry>,
        permission_manager: Arc<Mutex<PermissionManager>>,
        db: Arc<Mutex<Database>>,
        model: String,
        temperature: f32,
        max_iterations: usize,
    ) -> Self {
        let executor = Arc::new(ToolExecutor::new(
            permission_manager.clone(),
            tool_registry.clone(),
            db,
        ));

        Self {
            ollama,
            tool_registry,
            permission_manager,
            executor,
            model,
            temperature,
            max_iterations,
            active_runs: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// 设置权限确认事件发射器（用于权限确认弹窗）
    pub fn set_permission_emitter(&self, emitter: crate::tools::executor::PermissionEmitter) {
        self.executor.set_permission_emitter(emitter);
    }

    /// 设置待处理权限请求映射
    pub fn set_pending_permissions(&self, pending: Arc<Mutex<HashMap<String, PendingPermission>>>) {
        self.executor.set_pending_permissions(pending);
    }

    /// 发送消息并驱动 Agent（确定性直达 + 本地路由 + 单次自适应工具调用）
    pub async fn send_message(
        &self,
        session_id: &str,
        user_content: &str,
        model_override: Option<&str>,
        requested_run_id: Option<&str>,
        db: &Arc<Mutex<Database>>,
    ) -> AppResult<AgentRun> {
        let (tx, rx) = tokio::sync::mpsc::channel::<ChatEvent>(256);

        let run_id = requested_run_id
            .filter(|id| !id.trim().is_empty() && id.len() <= 128)
            .map(str::to_owned)
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        let cancellation = Arc::new(RunCancellation::new());
        let previous_run = {
            let mut active = self.active_runs.lock().await;
            active.insert(session_id.to_string(), cancellation.clone())
        };
        if let Some(previous) = previous_run {
            previous.cancel();
            self.executor.cancel_pending_for_session(session_id).await;
        }

        // 新指令以单飞方式接管当前会话：取消旧任务，再保存用户消息。
        let persist_result = {
            let db_guard = db.lock().await;
            let task_mgr = TaskManager::new(db_guard.connection());
            let _ = task_mgr.cancel_active_by_session(session_id);
            let mgr = ConversationManager::new(db_guard.connection());
            mgr.append_message(session_id, "user", user_content, None, None)
        };
        if let Err(error) = persist_result {
            let mut active = self.active_runs.lock().await;
            if active
                .get(session_id)
                .map(|current| Arc::ptr_eq(current, &cancellation))
                .unwrap_or(false)
            {
                active.remove(session_id);
            }
            return Err(error);
        }

        let session_id = session_id.to_string();
        let user_content = user_content.to_string();
        let db = db.clone();
        let ollama = self.ollama.clone();
        let executor = self.executor.clone();
        let tool_registry = self.tool_registry.clone();
        let model = model_override
            .filter(|model| !model.trim().is_empty())
            .unwrap_or(&self.model)
            .to_string();
        // 运行参数每次发起推理时从库里现读，而不是沿用启动时的快照。
        //
        // 为什么：用户改了温度/轮次上限后，如果只写库不重读，界面显示新值、
        // 实际仍跑旧值——设置项看着可调却无效，正是这次要消灭的东西。
        //
        // 这里用 await 取锁而非 try_lock：try_lock 撞上锁竞争会静默沿用旧值，
        // 用户刚改的参数就白白不生效，而且没有任何提示。等一小会儿拿到锁、
        // 读准新值，比"快但可能读错"更符合设置项的语义。
        let (temperature, max_iterations) = {
            let guard = db.lock().await;
            resolve_run_params(&guard, self.temperature, self.max_iterations)
        };
        let active_runs = self.active_runs.clone();
        let execution_run_id = run_id.clone();
        let task_cancellation = cancellation.clone();
        // 路由和任务创建全部移到后台。IPC 在用户消息落库后立即返回。
        // 非明确闲聊不再额外调用一次模型分类，避免每条指令固定增加数秒延迟。
        tokio::spawn(async move {
            let started_at = Instant::now();
            let result: AppResult<()> = async {
                if task_cancellation.is_cancelled() {
                    return Ok(());
                }
                let _ = tx.send(ChatEvent::Thinking).await;

                // 高频、单义指令直接执行，省掉意图分类 + 工具选择两次模型调用。
                // 工具仍统一经过 ToolExecutor，因此权限确认与审计不会被绕过。
                if let Some((tool_name, arguments)) = quick_direct_tool_call(&user_content) {
                    eprintln!("[Agent] 命中零模型快速通道: {}", tool_name);
                    execute_direct_tool(
                        DirectToolContext {
                            session_id: &session_id,
                            db: &db,
                            executor: &executor,
                            tx: &tx,
                            run_id: &execution_run_id,
                            cancellation: &task_cancellation,
                        },
                        tool_name,
                        arguments,
                    )
                    .await?;
                    return Ok(());
                }

                let local_intent = intent::quick_classify(&user_content);
                let explicit_chat = matches!(
                    local_intent.as_ref().map(|result| &result.intent),
                    Some(intent::IntentType::Chat)
                );
                if explicit_chat {
                    eprintln!("[Agent] 走快速路径（闲聊）: {}", user_content);
                    let needs_tool = run_chat_fast(ChatFastContext {
                        session_id: &session_id,
                        user_content: &user_content,
                        db: &db,
                        ollama: &ollama,
                        model: &model,
                        temperature,
                        tx: &tx,
                        cancellation: &task_cancellation,
                    })
                    .await?;
                    if !needs_tool {
                        return Ok(());
                    }
                    eprintln!("[Agent] 闲聊模型请求工具，自动切换 Jarvis 路径");
                }

                // 明确动作使用本地推断的窄工具集；闲聊回退时保留全部类别
                // （模型已经判断过需要工具，不再二次收窄，避免该有的工具被砍掉）；
                // 完全判断不出意图时用安全子集，而不是开放全部 —— 后者会把 33 个工具的
                // schema 全塞进上下文，吃掉半个窗口。
                let categories = if explicit_chat {
                    all_tool_categories()
                } else {
                    local_intent
                        .map(|result| result.categories)
                        .unwrap_or_else(intent::fallback_categories)
                };
                eprintln!("[Agent] 走 Jarvis 自适应路径: {}", user_content);
                let task = {
                    let db_guard = db.lock().await;
                    let task_mgr = TaskManager::new(db_guard.connection());
                    task_mgr.create(&session_id, &user_content)?
                };
                let _ = tx
                    .send(ChatEvent::TaskUpdate(TaskUpdateEvent::from_task(&task)))
                    .await;

                jarvis::run(
                    jarvis::JarvisContext {
                        session_id: &session_id,
                        user_content: &user_content,
                        db: &db,
                        ollama: &ollama,
                        executor: &executor,
                        tool_registry: &tool_registry,
                        model: &model,
                        temperature,
                        max_iterations,
                        tx: &tx,
                        categories: &categories,
                        run_id: &execution_run_id,
                        cancellation: &task_cancellation,
                    },
                    task,
                )
                .await?;
                Ok(())
            }
            .await;

            if let Err(e) = result {
                if !task_cancellation.is_cancelled() {
                    let _ = tx.send(ChatEvent::Error(e.to_string())).await;
                }
            }
            let _ = tx.send(ChatEvent::Done).await;

            eprintln!(
                "[Agent] run={} session={} status={} duration_ms={}",
                execution_run_id,
                session_id,
                if task_cancellation.is_cancelled() {
                    "cancelled"
                } else {
                    "finished"
                },
                started_at.elapsed().as_millis()
            );

            let mut active = active_runs.lock().await;
            let is_current = active
                .get(&session_id)
                .map(|current| Arc::ptr_eq(current, &task_cancellation))
                .unwrap_or(false);
            if is_current {
                active.remove(&session_id);
            }
        });

        Ok(AgentRun { run_id, events: rx })
    }

    pub async fn cancel_session(&self, session_id: &str) -> bool {
        let run = self.active_runs.lock().await.get(session_id).cloned();
        if let Some(run) = run {
            run.cancel();
            self.executor.cancel_pending_for_session(session_id).await;
            true
        } else {
            false
        }
    }

    /// 获取对话历史
    pub async fn get_history(
        &self,
        session_id: &str,
        db: &Arc<Mutex<Database>>,
    ) -> AppResult<Vec<OllamaMessage>> {
        let db_guard = db.lock().await;
        let mgr = ConversationManager::new(db_guard.connection());
        let messages = mgr.get_messages(session_id)?;
        Ok(convert_messages(&messages))
    }
}

fn all_tool_categories() -> Vec<intent::ToolCategory> {
    vec![
        intent::ToolCategory::File,
        intent::ToolCategory::Program,
        intent::ToolCategory::ProcessWindow,
        intent::ToolCategory::Browser,
        intent::ToolCategory::System,
        intent::ToolCategory::Memory,
        intent::ToolCategory::Skill,
        intent::ToolCategory::Automation,
    ]
}

/// 识别少量高频且语义明确的单步指令。复杂句子一律返回 None，交给正常 Agent 流程。
fn quick_direct_tool_call(user_content: &str) -> Option<(String, serde_json::Value)> {
    let mut text = user_content.trim();
    for polite in ["请帮我", "麻烦帮我", "帮我", "请"] {
        if let Some(rest) = text.strip_prefix(polite) {
            text = rest.trim();
            break;
        }
    }
    text = text.trim_end_matches(['。', '！', '!', '？', '?']).trim();
    let lower = text.to_lowercase();

    // 包含多步连接词时不做直达解析，防止只执行半条指令。
    if ["然后", "接着", "并且", "之后", "再帮", " and ", ";", "；"]
        .iter()
        .any(|marker| lower.contains(marker))
    {
        return None;
    }

    if ["截图", "截屏", "screenshot", "take a screenshot"].contains(&lower.as_str()) {
        return Some(("take_screenshot".to_string(), serde_json::json!({})));
    }

    if [
        "系统状态",
        "系统信息",
        "查看系统状态",
        "查看系统信息",
        "system status",
        "system info",
    ]
    .contains(&lower.as_str())
    {
        return Some(("get_system_context".to_string(), serde_json::json!({})));
    }

    for suffix in ["在运行吗", "是否在运行", "运行状态"] {
        if let Some(name) = text.strip_suffix(suffix) {
            let name = clean_direct_target(name);
            if !name.is_empty() {
                return Some((
                    "is_application_running".to_string(),
                    serde_json::json!({ "name": name }),
                ));
            }
        }
    }

    if let Some(target) = extract_direct_target(
        text,
        &["打开", "启动", "运行"],
        &["open ", "launch ", "start ", "run "],
    ) {
        // 文件、网址和多义对象仍交给 Agent，避免误当成应用程序。
        let target_lower = target.to_lowercase();
        if ![
            "文件",
            "文件夹",
            "目录",
            "网页",
            "网站",
            "http://",
            "https://",
        ]
        .iter()
        .any(|marker| target_lower.contains(marker))
        {
            return Some((
                "launch_program".to_string(),
                serde_json::json!({ "name_or_path": target }),
            ));
        }
    }

    if let Some(target) = extract_direct_target(
        text,
        &["关闭", "退出", "关掉"],
        &["close ", "quit ", "exit "],
    ) {
        return Some((
            "close_program".to_string(),
            serde_json::json!({ "name": target }),
        ));
    }

    None
}

fn extract_direct_target(
    text: &str,
    chinese_prefixes: &[&str],
    english_prefixes: &[&str],
) -> Option<String> {
    for prefix in chinese_prefixes {
        if let Some(rest) = text.strip_prefix(prefix) {
            let target = clean_direct_target(rest);
            return (!target.is_empty()).then_some(target);
        }
    }

    let lower = text.to_lowercase();
    for prefix in english_prefixes {
        if lower.starts_with(prefix) {
            let target = clean_direct_target(&text[prefix.len()..]);
            return (!target.is_empty()).then_some(target);
        }
    }
    None
}

fn clean_direct_target(target: &str) -> String {
    target
        .trim()
        .trim_start_matches("一下")
        .trim()
        .trim_end_matches("应用")
        .trim_end_matches("程序")
        .trim_end_matches('吧')
        .trim()
        .to_string()
}

struct DirectToolContext<'a> {
    session_id: &'a str,
    db: &'a Arc<Mutex<Database>>,
    executor: &'a ToolExecutor,
    tx: &'a tokio::sync::mpsc::Sender<ChatEvent>,
    run_id: &'a str,
    cancellation: &'a RunCancellation,
}

async fn execute_direct_tool(
    context: DirectToolContext<'_>,
    tool_name: String,
    arguments: serde_json::Value,
) -> AppResult<()> {
    let _ = context
        .tx
        .send(ChatEvent::ToolCall {
            name: tool_name.clone(),
            arguments: arguments.clone(),
        })
        .await;

    let execution = tokio::select! {
        _ = context.cancellation.cancelled() => return Ok(()),
        result = context.executor.execute(ToolCallRequest {
            tool_name: tool_name.clone(),
            arguments,
            session_id: Some(context.session_id.to_string()),
            run_id: Some(context.run_id.to_string()),
        }) => result?,
    };
    if context.cancellation.is_cancelled() {
        return Ok(());
    }
    let tool_content = execution.to_tool_message_content();
    let _ = context
        .tx
        .send(ChatEvent::ToolResult {
            name: tool_name,
            success: execution.success,
            content: tool_content,
        })
        .await;

    let response = if execution.verification.status
        == crate::tools::receipt::VerificationStatus::Contradicted
    {
        // 工具说成功、现实说没成：对用户只能报后者。
        // 这里不许沿用工具自述，否则就是把"自述"转述成"事实"。
        format!(
            "指令未能确认完成：{}",
            execution
                .verification
                .evidence
                .as_deref()
                .unwrap_or("独立核验显示实际状态与执行结果不符")
        )
    } else if execution.success {
        execution
            .result
            .data
            .as_ref()
            .and_then(|data| data.get("message").or_else(|| data.get("summary")))
            .and_then(|value| value.as_str())
            .unwrap_or("指令已执行完成")
            .to_string()
    } else {
        execution
            .result
            .error
            .clone()
            .unwrap_or_else(|| "指令执行失败".to_string())
    };

    {
        let db_guard = context.db.lock().await;
        let manager = ConversationManager::new(db_guard.connection());
        manager.append_message(context.session_id, "assistant", &response, None, None)?;
    }
    let _ = context.tx.send(ChatEvent::Text(response)).await;
    Ok(())
}

// ============================================================
// V21: 快速路径（纯闲聊，无工具调用）
// ============================================================

/// 快速路径：纯闲聊模式
///
/// - 精简 System Prompt（无工具列表）
/// - 加载最近10条对话
/// - 单次 LLM 流式调用
/// - 检测 [NEED_TOOL] 标记 → 自动切 Jarvis 工具路径
/// - 保存消息到数据库
struct ChatFastContext<'a> {
    session_id: &'a str,
    user_content: &'a str,
    db: &'a Arc<Mutex<Database>>,
    ollama: &'a OllamaClient,
    model: &'a str,
    temperature: f32,
    tx: &'a tokio::sync::mpsc::Sender<ChatEvent>,
    cancellation: &'a RunCancellation,
}

async fn run_chat_fast(context: ChatFastContext<'_>) -> AppResult<bool> {
    let _ = context.tx.send(ChatEvent::Thinking).await;

    // 构建快速路径 System Prompt
    let memory_context = build_memory_context(context.user_content);
    let system_prompt = intent::fast_path_system_prompt(&memory_context);

    // 加载最近10条对话历史
    let history = {
        let db_guard = context.db.lock().await;
        let mgr = ConversationManager::new(db_guard.connection());
        let msgs = mgr.get_messages(context.session_id)?;
        // 只取最近10条（不含当前用户消息，因为已在 send_message 中保存）
        let recent: Vec<_> = msgs
            .into_iter()
            .rev()
            .take(10)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        convert_messages(&recent)
    };

    // 构建消息列表
    let mut messages = vec![OllamaMessage::system(&system_prompt)];
    messages.extend(history);

    // 单次 LLM 流式调用（无工具）
    let mut stream = context
        .ollama
        .chat_stream(context.model, messages, None, context.temperature)
        .await?;

    let mut full_response = String::new();
    let mut response_forwarded = false;

    // 读取流式响应（30秒超时）
    loop {
        let next_chunk = tokio::select! {
            _ = context.cancellation.cancelled() => return Ok(false),
            chunk = timeout(Duration::from_secs(30), stream.next()) => chunk,
        };
        let chunk = match next_chunk {
            Ok(Some(chunk)) => chunk,
            Ok(None) => break,
            Err(_) => {
                let err_msg = "LLM 响应超时，请检查 Ollama 服务是否正常".to_string();
                let _ = context.tx.send(ChatEvent::Error(err_msg.clone())).await;
                return Ok(false);
            }
        };
        match chunk {
            ChatChunk::Text(text) => {
                full_response.push_str(&text);
                if response_forwarded {
                    let _ = context.tx.send(ChatEvent::Text(text)).await;
                } else {
                    let candidate = full_response.trim_start();
                    let marker = "[NEED_TOOL]";
                    if !marker.starts_with(candidate) && !candidate.starts_with(marker) {
                        response_forwarded = true;
                        let _ = context
                            .tx
                            .send(ChatEvent::Text(full_response.clone()))
                            .await;
                    }
                }
            }
            ChatChunk::ToolCall(_) => {
                // 快速路径不应有工具调用，忽略
            }
            ChatChunk::Done(_, _) => break,
            ChatChunk::Error(e) => {
                let _ = context.tx.send(ChatEvent::Error(e)).await;
                return Ok(false);
            }
        }
    }

    if context.cancellation.is_cancelled() {
        return Ok(false);
    }

    // 第三层兜底：检测 [NEED_TOOL] 标记
    if intent::detect_need_tool(&full_response) {
        eprintln!("[Agent] 快速路径检测到 [NEED_TOOL]，切换到完整路径");
        return Ok(true);
    }

    if !response_forwarded && !full_response.is_empty() {
        let _ = context
            .tx
            .send(ChatEvent::Text(full_response.clone()))
            .await;
    }

    // 保存 assistant 消息到数据库
    {
        let db_guard = context.db.lock().await;
        let mgr = ConversationManager::new(db_guard.connection());
        mgr.append_message(context.session_id, "assistant", &full_response, None, None)?;
    }

    Ok(false)
}

/// 构建记忆上下文（从长期记忆中检索相关记忆并格式化）
fn build_memory_context(user_content: &str) -> String {
    use crate::memory::MemoryEngine;

    let conn = match crate::memory::open_connection() {
        Some(c) => c,
        None => return String::new(),
    };

    let context = match MemoryEngine::build_adaptive_context(&conn, user_content, 4, 5) {
        Ok(value) => value,
        Err(_) => return String::new(),
    };
    context
}

/// 从 assistant 文本中提取 `<tool_call>` 块
fn extract_tool_calls(text: &str) -> (String, Vec<ToolCall>) {
    let mut tool_calls = Vec::new();
    let mut display_parts = Vec::new();
    let mut remaining = text;

    while let Some(start) = remaining.find("<tool_call>") {
        display_parts.push(&remaining[..start]);

        let after_start = &remaining[start + 11..];
        if let Some(end) = after_start.find("</tool_call>") {
            let json_str = after_start[..end].trim();
            if let Ok(json) = serde_json::from_str::<serde_json::Value>(json_str) {
                if let (Some(name), Some(args)) = (
                    json.get("name").and_then(|v| v.as_str()),
                    json.get("arguments"),
                ) {
                    tool_calls.push(ToolCall {
                        id: None,
                        call_type: Some("function".to_string()),
                        function: crate::llm::FunctionCall {
                            name: name.to_string(),
                            arguments: args.clone(),
                        },
                    });
                }
            }
            remaining = &after_start[end + 12..];
        } else {
            display_parts.push(&remaining[start..]);
            remaining = "";
            break;
        }
    }
    display_parts.push(remaining);

    let display_text = display_parts.join("").trim().to_string();
    (display_text, tool_calls)
}

/// 将数据库消息转换为 OllamaMessage
fn convert_messages(db_messages: &[ConversationMessage]) -> Vec<OllamaMessage> {
    db_messages
        .iter()
        .map(|msg| match msg.role.as_str() {
            "user" => OllamaMessage::user(&msg.content),
            "assistant" => {
                if let Some(ref tc_json) = msg.tool_calls {
                    match serde_json::from_str::<Vec<ToolCall>>(tc_json) {
                        Ok(tcs) => OllamaMessage::assistant_with_tools(&msg.content, tcs),
                        Err(_) => OllamaMessage::assistant(&msg.content),
                    }
                } else {
                    OllamaMessage::assistant(&msg.content)
                }
            }
            "tool" => OllamaMessage::tool(
                msg.tool_call_id.as_deref().unwrap_or("unknown"),
                &msg.content,
            ),
            _ => OllamaMessage::user(&msg.content),
        })
        .collect()
}

#[cfg(test)]
mod fast_path_tests {
    use super::{quick_direct_tool_call, RunCancellation};

    #[test]
    fn recognizes_unambiguous_program_commands() {
        let (tool, args) = quick_direct_tool_call("请帮我打开微信").expect("应命中快速通道");
        assert_eq!(tool, "launch_program");
        assert_eq!(args["name_or_path"], "微信");

        let (tool, args) = quick_direct_tool_call("打开一下QQ吧").expect("应命中快速通道");
        assert_eq!(tool, "launch_program");
        assert_eq!(args["name_or_path"], "QQ");

        let (tool, args) = quick_direct_tool_call("关闭 Chrome").expect("应命中快速通道");
        assert_eq!(tool, "close_program");
        assert_eq!(args["name"], "Chrome");
    }

    #[test]
    fn recognizes_read_only_shortcuts() {
        assert_eq!(
            quick_direct_tool_call("系统状态").map(|call| call.0),
            Some("get_system_context".to_string())
        );
        assert_eq!(
            quick_direct_tool_call("截图").map(|call| call.0),
            Some("take_screenshot".to_string())
        );
    }

    #[test]
    fn rejects_ambiguous_or_multi_step_commands() {
        assert!(quick_direct_tool_call("打开下载文件夹").is_none());
        assert!(quick_direct_tool_call("打开微信然后截图").is_none());
        assert!(quick_direct_tool_call("打开 https://example.com").is_none());
    }

    #[tokio::test]
    async fn cancellation_wakes_waiters_immediately() {
        let cancellation = RunCancellation::new();
        cancellation.cancel();
        tokio::time::timeout(
            std::time::Duration::from_millis(20),
            cancellation.cancelled(),
        )
        .await
        .expect("取消通知不应丢失");
    }
}

#[cfg(test)]
mod run_param_tests {
    use super::*;
    use std::path::PathBuf;

    fn fresh_db(tag: &str) -> (Database, PathBuf) {
        let dir = std::env::temp_dir().join(format!("pcg-runparam-{}-{}", tag, std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("创建临时目录");
        let db = Database::new(&dir.join("t.db")).expect("打开数据库");
        db.init_schema().expect("初始化 schema");
        (db, dir)
    }

    /// 这是本次改动的核心断言：设置改完之后，下一次取运行参数必须拿到新值。
    ///
    /// 改动前这两项是启动时快照，改设置只写库、运行时仍用旧值——
    /// 界面上改了温度却毫无效果，正是"假可调"的典型。
    #[test]
    fn picking_up_new_settings_immediately() {
        let (db, dir) = fresh_db("hot");

        // 先确认默认值与构造时的回落值不同，避免测试因为"新旧值恰好一样"而假通过
        db.update_setting("agent.temperature", "1.8")
            .expect("写温度");
        db.update_setting("agent.max_iterations", "33")
            .expect("写轮次");

        let (temperature, max_iterations) = resolve_run_params(&db, 0.7, 10);
        assert_eq!(temperature, 1.8, "温度应读到刚写入的新值");
        assert_eq!(max_iterations, 33, "轮次上限应读到刚写入的新值");

        // 再改一次，验证不是"只在首次生效"
        db.update_setting("agent.temperature", "0.2")
            .expect("再写温度");
        let (temperature, _) = resolve_run_params(&db, 0.7, 10);
        assert_eq!(temperature, 0.2, "第二次修改同样要立即生效");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn out_of_range_settings_are_clamped() {
        let (db, dir) = fresh_db("clamp");

        // 直接绕过命令层写库（模拟历史数据或外部改库），
        // 读取侧必须自己兜住，不能把 99.0 的温度丢给模型。
        db.connection()
            .execute(
                "UPDATE settings SET value = '99.0' WHERE key = 'agent.temperature'",
                [],
            )
            .expect("写入越界温度");
        db.connection()
            .execute(
                "UPDATE settings SET value = '9999' WHERE key = 'agent.max_iterations'",
                [],
            )
            .expect("写入越界轮次");

        let (temperature, max_iterations) = resolve_run_params(&db, 0.7, 10);
        assert_eq!(temperature, 2.0, "温度应被夹到上界");
        assert_eq!(max_iterations, 50, "轮次上限应被夹到上界");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn unparseable_settings_fall_back_instead_of_panicking() {
        let (db, dir) = fresh_db("fallback");

        db.connection()
            .execute(
                "UPDATE settings SET value = 'hot' WHERE key = 'agent.temperature'",
                [],
            )
            .expect("写入非数字温度");

        // 读不出来就回落，绝不能因为一个脏配置让整个对话流程崩掉
        let (temperature, max_iterations) = resolve_run_params(&db, 0.7, 10);
        assert_eq!(temperature, 0.7, "解析失败应回落到默认值");
        assert_eq!(max_iterations, 10, "未改动项应保持默认值");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn nan_is_rejected_rather_than_clamped() {
        let (db, dir) = fresh_db("nan");

        db.connection()
            .execute(
                "UPDATE settings SET value = 'NaN' WHERE key = 'agent.temperature'",
                [],
            )
            .expect("写入 NaN");

        // f32::clamp 遇到 NaN 会返回 NaN，若直接放行会一路传进模型请求。
        // 这里必须挡住并回落到默认值。
        let (temperature, _) = resolve_run_params(&db, 0.7, 10);
        assert!(
            temperature.is_finite(),
            "温度不能是 NaN 或无穷大，实际为 {}",
            temperature
        );
        assert_eq!(temperature, 0.7);

        let _ = std::fs::remove_dir_all(&dir);
    }
}
