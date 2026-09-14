//! Agent 运行时模块 — Phase 4 Agent Loop 2.0 (ReAct)
//!
//! 驱动整个对话流程：接收用户输入 → 创建 Task → Think → Plan → Act → Observe → Evaluate → Finish
//!
//! 三层闸门的调用方：Agent Loop 协调 ToolRegistry → PermissionManager → ToolExecutor。

use std::sync::Arc;
use std::collections::HashMap;
use tokio::sync::Mutex;
use tokio_stream::StreamExt;
use tokio::time::{timeout, Duration};

use crate::db::conversation::{ConversationManager, ConversationMessage};
use crate::db::Database;
use crate::error::AppResult;
use crate::llm::{
    OllamaClient, OllamaMessage, ChatChunk, ToolCall,
};
use crate::security::PermissionManager;
use crate::tools::ToolRegistry;
use crate::tools::executor::{ToolExecutor, ToolCallRequest, PendingPermission};
pub mod task;
pub mod verification;
pub mod recovery;
pub mod observation;
pub mod context;
pub mod intent;
use task::{Task, TaskManager, TaskStatus, TaskStep, StepStatus, TaskUpdateEvent, ActionStatusEvent};
use verification::{VerificationEngine, VerificationResult};
use recovery::{RecoveryEngine, RecoveryResult, RecoveryStrategy, RecoveryEvent};
use observation::{Observation, ObservationBuffer};
use context::{ContextEngine, SystemStateSnapshot};

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
    ToolCall { name: String, arguments: serde_json::Value },
    /// 工具执行结果
    ToolResult { name: String, success: bool, content: String },
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

// ============================================================
// AgentRuntime
// ============================================================

pub struct AgentRuntime {
    ollama: Arc<OllamaClient>,
    tool_registry: Arc<ToolRegistry>,
    #[allow(dead_code)]
    permission_manager: Arc<Mutex<PermissionManager>>,
    executor: Arc<ToolExecutor>,
    model: String,
    temperature: f32,
    max_iterations: usize,
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
        }
    }

    /// 设置 AppHandle（用于权限确认弹窗）
    pub fn set_app_handle(&self, handle: tauri::AppHandle) {
        self.executor.set_app_handle(handle);
    }

    /// 设置待处理权限请求映射
    pub fn set_pending_permissions(&self, pending: Arc<Mutex<HashMap<String, PendingPermission>>>) {
        self.executor.set_pending_permissions(pending);
    }

    /// 发送消息并驱动 Agent（三层意图识别 + 快速路径 / 完整 ReAct 路径）
    pub async fn send_message(
        &self,
        session_id: &str,
        user_content: &str,
        db: &Arc<Mutex<Database>>,
    ) -> AppResult<tokio::sync::mpsc::Receiver<ChatEvent>> {
        let (tx, rx) = tokio::sync::mpsc::channel::<ChatEvent>(256);

        // 保存用户消息
        {
            let db_guard = db.lock().await;
            let mgr = ConversationManager::new(db_guard.connection());
            mgr.append_message(session_id, "user", user_content, None, None)?;
        }

        // V21: 三层意图识别
        let intent_result = intent::classify_intent(
            &self.ollama,
            &self.model,
            user_content,
            self.temperature,
        ).await;

        let session_id = session_id.to_string();
        let user_content = user_content.to_string();
        let db = db.clone();
        let ollama = self.ollama.clone();
        let executor = self.executor.clone();
        let tool_registry = self.tool_registry.clone();
        let model = self.model.clone();
        let temperature = self.temperature;
        let max_iterations = self.max_iterations;
        let tx_clone = tx.clone();

        match intent_result.intent {
            intent::IntentType::Chat => {
                // 快速路径：纯闲聊，无工具调用
                eprintln!("[Agent] 走快速路径（闲聊）: {}", user_content);
                tokio::spawn(async move {
                    let result = run_chat_fast(
                        &session_id,
                        &user_content,
                        &db,
                        &ollama,
                        &model,
                        temperature,
                        &tx_clone,
                    ).await;

                    if let Err(e) = result {
                        let _ = tx_clone.send(ChatEvent::Error(e.to_string())).await;
                    }
                    let _ = tx_clone.send(ChatEvent::Done).await;
                });
            }
            _ => {
                // 完整路径：需要操作电脑，走 ReAct Loop
                eprintln!("[Agent] 走完整路径（操作）: {}", user_content);

                // 取消会话中任何之前的活跃任务
                {
                    let db_guard = db.lock().await;
                    let task_mgr = TaskManager::new(db_guard.connection());
                    let _ = task_mgr.cancel_active_by_session(&session_id);
                }

                // 创建新 Task
                let task = {
                    let db_guard = db.lock().await;
                    let task_mgr = TaskManager::new(db_guard.connection());
                    task_mgr.create(&session_id, &user_content)?
                };

                // 发送初始 TaskUpdate
                let _ = tx_clone.send(ChatEvent::TaskUpdate(TaskUpdateEvent::from_task(&task))).await;

                let task_id = task.id.clone();

                tokio::spawn(async move {
                    let result = run_react_loop(
                        &session_id,
                        &user_content,
                        &task_id,
                        &db,
                        &ollama,
                        &executor,
                        &tool_registry,
                        &model,
                        temperature,
                        max_iterations,
                        &tx_clone,
                        &intent_result.categories,
                    )
                    .await;

                    if let Err(e) = result {
                        let _ = tx_clone.send(ChatEvent::Error(e.to_string())).await;
                    }

                    let _ = tx_clone.send(ChatEvent::Done).await;
                });
            }
        }

        Ok(rx)
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

// ============================================================
// ReAct Agent Loop 2.0
// ============================================================

/// ReAct 循环核心逻辑
///
/// 阶段：THINK → PLAN → ACT → OBSERVE → EVALUATE → (CONTINUE / REPLAN / FINISH)
/// V21: 支持按需注入工具（根据意图识别的类别）
async fn run_react_loop(
    session_id: &str,
    user_content: &str,
    task_id: &str,
    db: &Arc<Mutex<Database>>,
    ollama: &OllamaClient,
    executor: &ToolExecutor,
    tool_registry: &ToolRegistry,
    model: &str,
    temperature: f32,
    max_iterations: usize,
    tx: &tokio::sync::mpsc::Sender<ChatEvent>,
    categories: &[intent::ToolCategory],
) -> AppResult<()> {
    // 加载 Task
    let mut task = {
        let db_guard = db.lock().await;
        let task_mgr = TaskManager::new(db_guard.connection());
        task_mgr.get(task_id)?.ok_or_else(||
            crate::error::AppError::TaskNotFound(format!("Task {} not found", task_id))
        )?
    };

    // V13: Observation Buffer — 记录所有工具执行观察，用于上下文和验证
    let mut observation_buffer = ObservationBuffer::new(20);

    // V15: Context Engine — 聚合系统状态、任务状态、工具结果缓存
    let mut context_engine = ContextEngine::new();
    // 初始化系统状态快照
    {
        let metrics = crate::monitoring::get_system_metrics();
        let mut snapshot = SystemStateSnapshot::from_metrics(&metrics);
        // 尝试获取活动窗口和进程数（通过 get_system_context 工具的逻辑）
        snapshot.active_window = get_active_window_simple();
        snapshot.process_count = get_process_count_simple();
        context_engine.update_system_state(snapshot);
    }

    // 构建 System Prompt（注入长期记忆 + 按需工具列表）
    let memory_context = build_memory_context(&user_content);
    // V21: 按需获取工具名称（意图类别 + 始终加载的 General 类）
    let active_tool_names = tool_registry.list_names_by_categories(categories);
    let system_prompt = build_system_prompt_with_tools(tool_registry, &active_tool_names, &memory_context);

    // 获取对话历史
    let history = {
        let db_guard = db.lock().await;
        let mgr = ConversationManager::new(db_guard.connection());
        let msgs = mgr.get_messages(session_id)?;
        convert_messages(&msgs)
    };

    // V21: 预构建工具 JSON（仅按需加载的工具）
    let tools_json: Vec<serde_json::Value> = active_tool_names
        .iter()
        .filter_map(|name| tool_registry.get(name))
        .map(|tool| {
            serde_json::json!({
                "type": "function",
                "function": {
                    "name": tool.name(),
                    "description": tool.description(),
                    "parameters": tool.parameters(),
                }
            })
        })
        .collect();

    eprintln!(
        "[Agent] 按需加载工具: {} / {} 个 (类别: {:?})",
        tools_json.len(),
        tool_registry.len(),
        categories
    );

    let mut messages = history;
    if !messages.iter().any(|m| m.role == "system") {
        messages.insert(0, OllamaMessage::system(&system_prompt));
    }

    // ========================================================
    // PHASE 1: THINK — 分析用户意图
    // ========================================================
    task.set_status(TaskStatus::Analyzing);
    update_task_and_notify(&task, db, tx).await?;

    let _ = tx.send(ChatEvent::Thinking).await;
    let _ = tx.send(ChatEvent::ActionStatus(ActionStatusEvent {
        task_id: task.id.clone(),
        step: 0,
        action: "分析任务".to_string(),
        status: "thinking".to_string(),
        message: "正在理解您的请求...".to_string(),
    })).await;

    // 简单任务判定：通过 LLM 快速分析
    let is_simple = classify_simple_task(ollama, model, user_content, temperature).await?;
    task.is_simple = is_simple;

    if is_simple {
        // 简单任务：直接执行，不生成 plan
        task.set_status(TaskStatus::Executing);
        task.set_plan(vec![TaskStep::new(1, "执行请求")]);
        update_task_and_notify(&task, db, tx).await?;

        let result = execute_simple_task(
            session_id, user_content, db, ollama, executor, tool_registry,
            model, temperature, max_iterations, tx, &mut task, &tools_json, &mut messages,
        ).await;

        // 保存最终 Task 状态
        {
            let db_guard = db.lock().await;
            let task_mgr = TaskManager::new(db_guard.connection());
            let _ = task_mgr.update(&task);
        }
        let _ = tx.send(ChatEvent::TaskUpdate(TaskUpdateEvent::from_task(&task))).await;

        return result;
    }

    // ========================================================
    // PHASE 2: PLAN — 生成执行计划
    // ========================================================
    task.set_status(TaskStatus::Planning);
    update_task_and_notify(&task, db, tx).await?;

    let _ = tx.send(ChatEvent::ActionStatus(ActionStatusEvent {
        task_id: task.id.clone(),
        step: 0,
        action: "制定计划".to_string(),
        status: "planning".to_string(),
        message: "正在制定执行计划...".to_string(),
    })).await;

    let plan = generate_plan(ollama, model, user_content, tool_registry, &active_tool_names, temperature).await?;
    let steps: Vec<TaskStep> = plan.iter().enumerate().map(|(i, action)| {
        TaskStep::new(i + 1, action)
    }).collect();
    task.set_plan(steps.clone());
    task.set_status(TaskStatus::Executing);
    update_task_and_notify(&task, db, tx).await?;

    // 展示 plan 摘要
    let plan_summary = plan.join(" → ");
    let _ = tx.send(ChatEvent::ActionStatus(ActionStatusEvent {
        task_id: task.id.clone(),
        step: 0,
        action: "计划已生成".to_string(),
        status: "planned".to_string(),
        message: format!("共 {} 步: {}", plan.len(), plan_summary),
    })).await;

    // ========================================================
    // PHASE 3-5: ACT → OBSERVE → EVALUATE 循环
    // ========================================================
    let mut iteration = 0;

    loop {
        if iteration >= max_iterations {
            task.set_status(TaskStatus::Failed);
            task.summary = Some("任务过于复杂，达到最大迭代次数".to_string());
            update_task_and_notify(&task, db, tx).await?;
            let _ = tx.send(ChatEvent::Error(
                "已达到最大迭代次数，任务中断".to_string(),
            )).await;
            break;
        }
        iteration += 1;

        // 获取下一步
        let next_step = match task.next_pending_step() {
            Some(s) => s.clone(),
            None => {
                // 所有步骤完成
                if task.all_steps_completed() {
                    task.set_status(TaskStatus::Completed);
                    task.summary = Some("任务已完成".to_string());
                    update_task_and_notify(&task, db, tx).await?;
                } else {
                    task.set_status(TaskStatus::Failed);
                    task.summary = Some("任务执行异常终止".to_string());
                    update_task_and_notify(&task, db, tx).await?;
                }
                break;
            }
        };

        // 执行步骤
        task.start_step(next_step.step_id);
        update_task_and_notify(&task, db, tx).await?;

        let _ = tx.send(ChatEvent::ActionStatus(ActionStatusEvent {
            task_id: task.id.clone(),
            step: next_step.step_id,
            action: next_step.action.clone(),
            status: "executing".to_string(),
            message: format!("正在执行第 {} 步: {}", next_step.step_id, next_step.action),
        })).await;

        // 构建上下文：System + 历史 + 当前任务状态 + 工具
        let task_context = build_task_context(&task, tool_registry, &active_tool_names);
        let mut step_messages = messages.clone();
        step_messages.push(OllamaMessage::system(&task_context));

        // V15: Context Engine 注入运行时上下文（系统状态 + 任务进度 + 近期观察 + 工具缓存）
        let obs_vec: Vec<Observation> = observation_buffer.get_all().to_vec();
        let context_injection = context_engine.build_system_prompt_injection(&task, &obs_vec);
        if !context_injection.is_empty() {
            step_messages.push(OllamaMessage::system(&context_injection));
        }

        // 调用 LLM 生成工具调用（ACT）
        let _ = tx.send(ChatEvent::Thinking).await;

        let tools_param = if tools_json.is_empty() { None } else { Some(tools_json.clone()) };
        let mut stream = ollama
            .chat_stream(model, step_messages.clone(), tools_param, temperature)
            .await?;

        let mut assistant_text = String::new();
        let mut pending_tool_calls: Vec<ToolCall> = Vec::new();

        // 读取流式响应（每个 chunk 30秒超时，防止 Ollama 挂起时永久卡死）
        loop {
            let chunk = match timeout(Duration::from_secs(30), stream.next()).await {
                Ok(Some(chunk)) => chunk,
                Ok(None) => break,
                Err(_) => {
                    let err_msg = "LLM 响应超时，请检查 Ollama 服务是否正常".to_string();
                    let _ = tx.send(ChatEvent::Error(err_msg.clone())).await;
                    task.fail_step(next_step.step_id, &err_msg);
                    task.set_status(TaskStatus::Failed);
                    update_task_and_notify(&task, db, tx).await?;
                    break;
                }
            };
            match chunk {
                ChatChunk::Text(text) => {
                    assistant_text.push_str(&text);
                    let _ = tx.send(ChatEvent::Text(text)).await;
                }
                ChatChunk::ToolCall(tc) => {
                    pending_tool_calls.push(tc.clone());
                    let _ = tx.send(ChatEvent::ToolCall {
                        name: tc.function.name.clone(),
                        arguments: tc.function.arguments.clone(),
                    }).await;
                }
                ChatChunk::Done => break,
                ChatChunk::Error(e) => {
                    let _ = tx.send(ChatEvent::Error(e.clone())).await;
                    task.fail_step(next_step.step_id, &format!("LLM 错误: {}", e));
                    update_task_and_notify(&task, db, tx).await?;
                    break;
                }
            }
        }

        // 解析模拟 tool calls
        let (display_text, simulated_calls) = extract_tool_calls(&assistant_text);
        for tc in &simulated_calls {
            let _ = tx.send(ChatEvent::ToolCall {
                name: tc.function.name.clone(),
                arguments: tc.function.arguments.clone(),
            }).await;
        }
        pending_tool_calls.extend(simulated_calls);

        // 保存 assistant 消息
        let tool_calls_json = if pending_tool_calls.is_empty() {
            None
        } else {
            Some(serde_json::to_string(&pending_tool_calls)?)
        };

        {
            let db_guard = db.lock().await;
            let mgr = ConversationManager::new(db_guard.connection());
            mgr.append_message(
                session_id,
                "assistant",
                &display_text,
                tool_calls_json.as_deref(),
                None,
            )?;
        }

        // OBSERVE: 执行工具并收集结果（带 Verification + Recovery）
        let mut step_success = true;
        let mut step_results: Vec<String> = Vec::new();
        let verification_engine = VerificationEngine::new();
        let recovery_engine = RecoveryEngine::new(2);
        // V14: Replan/Escalate 跟踪
        let mut need_replan = false;
        let mut escalate_reason: Option<String> = None;

        if !pending_tool_calls.is_empty() {
            for tc in pending_tool_calls {
                let mut current_args: serde_json::Value = tc.function.arguments.clone();
                let current_tool = tc.function.name.clone();
                // V21: 从持久化的 TaskStep 中恢复 recovery_attempts（崩溃后不丢失）
                let mut retry_count = task.plan.iter()
                    .find(|s| s.step_id == task.current_step)
                    .map(|s| s.recovery_attempts)
                    .unwrap_or(0);
                let tool_final_success;
                let tool_final_content;

                'tool_retry: loop {
                    let request = ToolCallRequest {
                        tool_name: current_tool.clone(),
                        arguments: current_args.clone(),
                        session_id: Some(session_id.to_string()),
                    };

                    let exec_result = executor.execute(request).await?;
                    let tool_content = exec_result.to_tool_message_content();
                    let success = exec_result.success;

                    let _ = tx.send(ChatEvent::ToolResult {
                        name: current_tool.clone(),
                        success,
                        content: tool_content.clone(),
                    }).await;

                    // 保存 tool 结果
                    {
                        let db_guard = db.lock().await;
                        let mgr = ConversationManager::new(db_guard.connection());
                        mgr.append_message(
                            session_id,
                            "tool",
                            &tool_content,
                            None,
                            Some(&current_tool),
                        )?;
                    }
                    messages.push(OllamaMessage::tool(&current_tool, &tool_content));

                    // === Verification: 验证工具执行的真实结果 ===
                    let verification = verification_engine.verify(
                        &current_tool,
                        &current_args,
                        &exec_result.result,
                    );

                    match verification {
                        VerificationResult::Pass { summary } => {
                            tool_final_success = true;
                            tool_final_content = format!("{} (已验证: {})", current_tool, summary);
                            step_results.push(tool_final_content.clone());
                            // V13: 记录成功观察
                            let obs = Observation::from_tool_result(
                                task_id,
                                Some(&task.current_step.to_string()),
                                &current_tool,
                                true,
                                exec_result.result.data.clone().unwrap_or(serde_json::Value::Null),
                                summary.clone(),
                            );
                            observation_buffer.push(obs.clone());
                            persist_observation(db, &obs).await;
                            context_engine.add_tool_result(&current_tool, true, &summary);
                            break 'tool_retry;
                        }
                        VerificationResult::Fail { reason } => {
                            // V13: 记录失败观察
                            let obs = Observation::from_tool_result(
                                task_id,
                                Some(&task.current_step.to_string()),
                                &current_tool,
                                false,
                                exec_result.result.data.clone().unwrap_or(serde_json::Value::Null),
                                reason.clone(),
                            );
                            observation_buffer.push(obs.clone());
                            persist_observation(db, &obs).await;
                            context_engine.add_tool_result(&current_tool, false, &reason);
                            // === Recovery: 分析错误并决定恢复策略 ===
                            let recovery = recovery_engine.analyze_and_recover(
                                &current_tool,
                                &current_args,
                                &exec_result.result,
                                retry_count,
                            );

                            match recovery {
                                RecoveryResult::Recovered { strategy, message } => {
                                    // V14: 发送结构化恢复事件
                                    let strategy_name = match &strategy {
                                        RecoveryStrategy::Retry { .. } => "retry",
                                        RecoveryStrategy::AdjustParams { .. } => "adjust_params",
                                        RecoveryStrategy::SwitchTool { .. } => "switch_tool",
                                        RecoveryStrategy::Replan { .. } => "replan",
                                        RecoveryStrategy::Escalate { .. } => "escalate",
                                        RecoveryStrategy::Fail { .. } => "fail",
                                    };
                                    let recovery_event = RecoveryEvent::new(
                                        task_id,
                                        task.current_step,
                                        &current_tool,
                                        retry_count + 1,
                                        strategy_name,
                                        &message,
                                    );
                                    let _ = tx.send(ChatEvent::Recovery(recovery_event)).await;
                                    let _ = tx.send(ChatEvent::Text(
                                        format!("\n[恢复] {}: {}\n", current_tool, message)
                                    )).await;

                                    // V21: 持久化恢复状态到 TaskStep（崩溃后不丢失）
                                    if let Some(step) = task.plan.iter_mut()
                                        .find(|s| s.step_id == task.current_step)
                                    {
                                        step.record_recovery(strategy_name);
                                    }
                                    update_task_and_notify(&task, db, tx).await?;

                                    match strategy {
                                        RecoveryStrategy::Retry { backoff_ms } => {
                                            // V14: 指数退避后重试
                                            if backoff_ms > 0 {
                                                tokio::time::sleep(Duration::from_millis(backoff_ms)).await;
                                            }
                                            retry_count += 1;
                                            continue 'tool_retry;
                                        }
                                        RecoveryStrategy::AdjustParams { adjusted_arguments } => {
                                            current_args = adjusted_arguments;
                                            retry_count += 1;
                                            continue 'tool_retry;
                                        }
                                        RecoveryStrategy::SwitchTool { alternative_tool, adjusted_arguments } => {
                                            // 先执行替代工具（如 find_program）
                                            let alt_request = ToolCallRequest {
                                                tool_name: alternative_tool.clone(),
                                                arguments: adjusted_arguments.clone(),
                                                session_id: Some(session_id.to_string()),
                                            };
                                            let alt_result = executor.execute(alt_request).await?;
                                            let alt_content = alt_result.to_tool_message_content();

                                            let _ = tx.send(ChatEvent::ToolResult {
                                                name: alternative_tool.clone(),
                                                success: alt_result.success,
                                                content: alt_content.clone(),
                                            }).await;

                                            {
                                                let db_guard = db.lock().await;
                                                let mgr = ConversationManager::new(db_guard.connection());
                                                mgr.append_message(
                                                    session_id, "tool", &alt_content,
                                                    None, Some(&alternative_tool),
                                                )?;
                                            }
                                            messages.push(OllamaMessage::tool(&alternative_tool, &alt_content));

                                            // 如果替代工具成功，用找到的路径重试原工具
                                            if alt_result.success {
                                                if let Some(paths) = alt_result.result.data
                                                    .as_ref()
                                                    .and_then(|d| d.get("paths"))
                                                    .and_then(|p| p.as_array())
                                                {
                                                    if let Some(first_path) = paths.first().and_then(|p| p.as_str()) {
                                                        current_args = serde_json::json!({
                                                            "name_or_path": first_path
                                                        });
                                                        retry_count += 1;
                                                        continue 'tool_retry;
                                                    }
                                                }
                                            }

                                            // 替代工具也失败，标记失败
                                            tool_final_success = false;
                                            tool_final_content = format!("{}: 恢复失败 ({})", current_tool, reason);
                                            step_results.push(tool_final_content.clone());
                                            step_success = false;
                                            break 'tool_retry;
                                        }
                                        RecoveryStrategy::Replan { reason } => {
                                            // V14: 当前方法不可行，标记需要重新规划
                                            need_replan = true;
                                            tool_final_success = false;
                                            tool_final_content = format!("{}: 需要重新规划 ({})", current_tool, reason);
                                            step_results.push(tool_final_content.clone());
                                            step_success = false;
                                            break 'tool_retry;
                                        }
                                        RecoveryStrategy::Escalate { reason } => {
                                            // V14: 权限/系统限制，升级到用户
                                            escalate_reason = Some(reason.clone());
                                            tool_final_success = false;
                                            tool_final_content = format!("{}: 需要用户介入 ({})", current_tool, reason);
                                            step_results.push(tool_final_content.clone());
                                            step_success = false;
                                            break 'tool_retry;
                                        }
                                        RecoveryStrategy::Fail { reason: fail_reason } => {
                                            // 恢复策略本身就是 Fail（如进程不存在视为已关闭）
                                            tool_final_success = true;
                                            tool_final_content = format!("{}: {}", current_tool, fail_reason);
                                            step_results.push(tool_final_content.clone());
                                            break 'tool_retry;
                                        }
                                    }
                                }
                                RecoveryResult::Failed { reason } => {
                                    tool_final_success = false;
                                    tool_final_content = format!("{}: 验证失败且无法恢复 ({})", current_tool, reason);
                                    step_results.push(tool_final_content.clone());
                                    step_success = false;
                                    break 'tool_retry;
                                }
                            }
                        }
                    }
                }

                if !tool_final_success {
                    step_success = false;
                }
            }

            // 重新加入 assistant 消息
            let last_assistant = if let Some(ref tc_json) = tool_calls_json {
                let tcs: Vec<ToolCall> = serde_json::from_str(tc_json)?;
                OllamaMessage::assistant_with_tools(&display_text, tcs)
            } else {
                OllamaMessage::assistant(&display_text)
            };
            messages.push(last_assistant);
        } else {
            // 没有 tool calls，直接完成
            task.complete_step(next_step.step_id, &display_text);
            update_task_and_notify(&task, db, tx).await?;

            let _ = tx.send(ChatEvent::ActionStatus(ActionStatusEvent {
                task_id: task.id.clone(),
                step: next_step.step_id,
                action: next_step.action.clone(),
                status: "completed".to_string(),
                message: display_text.clone(),
            })).await;
            continue;
        }

        // V14: 处理 Replan — 重新生成剩余步骤的计划
        if need_replan {
            let _ = tx.send(ChatEvent::Text(
                "\n[规划] 当前方法不可行，正在重新制定计划...\n".to_string()
            )).await;

            // 标记当前步骤为失败
            let error_summary = step_results.join("; ");
            task.fail_step(next_step.step_id, &error_summary);

            // 重新生成计划（基于已完成的步骤）
            let remaining_goal = format!(
                "原目标: {}。已完成步骤: {}。请为剩余部分重新制定计划。",
                task.goal,
                task.completed_count()
            );
            match generate_plan(ollama, model, &remaining_goal, tool_registry, &active_tool_names, temperature).await {
                Ok(new_plan) => {
                    let new_steps: Vec<TaskStep> = new_plan.iter().enumerate().map(|(i, action)| {
                        TaskStep::new(task.completed_count() + i + 1, action)
                    }).collect();
                    // 保留已完成的步骤，替换未完成的
                    let mut updated_plan = task.plan.clone();
                    updated_plan.retain(|s| matches!(s.status, StepStatus::Completed));
                    updated_plan.extend(new_steps);
                    task.set_plan(updated_plan);
                    task.set_status(TaskStatus::Executing);
                    update_task_and_notify(&task, db, tx).await?;

                    let _ = tx.send(ChatEvent::ActionStatus(ActionStatusEvent {
                        task_id: task.id.clone(),
                        step: 0,
                        action: "重新规划".to_string(),
                        status: "replanned".to_string(),
                        message: format!("已重新规划 {} 个剩余步骤", new_plan.len()),
                    })).await;
                    continue;
                }
                Err(e) => {
                    let _ = tx.send(ChatEvent::Error(format!("重新规划失败: {}", e))).await;
                }
            }
        }

        // V14: 处理 Escalate — 告知用户需要介入
        if let Some(ref reason) = escalate_reason {
            let _ = tx.send(ChatEvent::Text(
                format!("\n[需要用户介入] {}\n", reason)
            )).await;
        }

        // EVALUATE: 判断步骤结果
        if step_success {
            let result_summary = step_results.join("; ");
            task.complete_step(next_step.step_id, &result_summary);
            update_task_and_notify(&task, db, tx).await?;

            let _ = tx.send(ChatEvent::ActionStatus(ActionStatusEvent {
                task_id: task.id.clone(),
                step: next_step.step_id,
                action: next_step.action.clone(),
                status: "completed".to_string(),
                message: format!("第 {} 步完成（已验证）", next_step.step_id),
            })).await;
        } else {
            let error_summary = step_results.join("; ");
            task.fail_step(next_step.step_id, &error_summary);
            task.set_status(TaskStatus::Failed);
            task.summary = Some(format!("第 {} 步执行失败", next_step.step_id));
            update_task_and_notify(&task, db, tx).await?;

            let _ = tx.send(ChatEvent::ActionStatus(ActionStatusEvent {
                task_id: task.id.clone(),
                step: next_step.step_id,
                action: next_step.action.clone(),
                status: "failed".to_string(),
                message: format!("第 {} 步失败: {}", next_step.step_id, error_summary),
            })).await;
            break;
        }

        // 检查是否所有步骤完成
        if task.all_steps_completed() {
            task.set_status(TaskStatus::Verifying);
            update_task_and_notify(&task, db, tx).await?;

            // 简化验证：直接标记完成
            task.set_status(TaskStatus::Completed);
            task.summary = Some("所有步骤已完成".to_string());
            update_task_and_notify(&task, db, tx).await?;

            let _ = tx.send(ChatEvent::ActionStatus(ActionStatusEvent {
                task_id: task.id.clone(),
                step: 0,
                action: "任务完成".to_string(),
                status: "finished".to_string(),
                message: "任务已完成".to_string(),
            })).await;
            break;
        }
    }

    // 保存最终 Task 状态
    {
        let db_guard = db.lock().await;
        let task_mgr = TaskManager::new(db_guard.connection());
        let _ = task_mgr.update(&task);
    }
    let _ = tx.send(ChatEvent::TaskUpdate(TaskUpdateEvent::from_task(&task))).await;

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
/// - 检测 [NEED_TOOL] 标记 → 自动切完整路径重跑
/// - 保存消息到数据库
async fn run_chat_fast(
    session_id: &str,
    _user_content: &str,
    db: &Arc<Mutex<Database>>,
    ollama: &OllamaClient,
    model: &str,
    temperature: f32,
    tx: &tokio::sync::mpsc::Sender<ChatEvent>,
) -> AppResult<()> {
    let _ = tx.send(ChatEvent::Thinking).await;

    // 构建快速路径 System Prompt
    let memory_context = build_memory_context(_user_content);
    let system_prompt = intent::fast_path_system_prompt(&memory_context);

    // 加载最近10条对话历史
    let history = {
        let db_guard = db.lock().await;
        let mgr = ConversationManager::new(db_guard.connection());
        let msgs = mgr.get_messages(session_id)?;
        // 只取最近10条（不含当前用户消息，因为已在 send_message 中保存）
        let recent: Vec<_> = msgs.into_iter().rev().take(10).collect::<Vec<_>>().into_iter().rev().collect();
        convert_messages(&recent)
    };

    // 构建消息列表
    let mut messages = vec![OllamaMessage::system(&system_prompt)];
    messages.extend(history);

    // 单次 LLM 流式调用（无工具）
    let mut stream = ollama
        .chat_stream(model, messages, None, temperature)
        .await?;

    let mut full_response = String::new();

    // 读取流式响应（30秒超时）
    loop {
        let chunk = match timeout(Duration::from_secs(30), stream.next()).await {
            Ok(Some(chunk)) => chunk,
            Ok(None) => break,
            Err(_) => {
                let err_msg = "LLM 响应超时，请检查 Ollama 服务是否正常".to_string();
                let _ = tx.send(ChatEvent::Error(err_msg.clone())).await;
                return Ok(());
            }
        };
        match chunk {
            ChatChunk::Text(text) => {
                full_response.push_str(&text);
                let _ = tx.send(ChatEvent::Text(text)).await;
            }
            ChatChunk::ToolCall(_) => {
                // 快速路径不应有工具调用，忽略
            }
            ChatChunk::Done => break,
            ChatChunk::Error(e) => {
                let _ = tx.send(ChatEvent::Error(e)).await;
                return Ok(());
            }
        }
    }

    // 第三层兜底：检测 [NEED_TOOL] 标记
    if intent::detect_need_tool(&full_response) {
        eprintln!("[Agent] 快速路径检测到 [NEED_TOOL]，切换到完整路径");
        let _ = tx.send(ChatEvent::Text(
            "\n[切换到操作模式...]".to_string()
        )).await;

        // 清除已发送的快速路径回复，重新走完整路径
        // 注意：这里不重新保存用户消息（已在 send_message 中保存）
        // 但需要删除快速路径产生的 assistant 消息（如果有的话）
        // 简化处理：直接返回，由前端重新触发或用户再次发送
        // 更好的做法：在这里直接调用完整路径，但需要 task_id 等
        // 当前实现：告知用户需要重新发送
        let _ = tx.send(ChatEvent::Text(
            "\n检测到需要操作电脑，请重新发送该请求以使用完整功能。".to_string()
        )).await;
        return Ok(());
    }

    // 保存 assistant 消息到数据库
    {
        let db_guard = db.lock().await;
        let mgr = ConversationManager::new(db_guard.connection());
        mgr.append_message(session_id, "assistant", &full_response, None, None)?;
    }

    Ok(())
}

// ============================================================
// 简单任务执行
// ============================================================

async fn execute_simple_task(
    session_id: &str,
    _user_content: &str,
    db: &Arc<Mutex<Database>>,
    ollama: &OllamaClient,
    executor: &ToolExecutor,
    _tool_registry: &ToolRegistry,
    model: &str,
    temperature: f32,
    max_iterations: usize,
    tx: &tokio::sync::mpsc::Sender<ChatEvent>,
    task: &mut Task,
    tools_json: &[serde_json::Value],
    messages: &mut Vec<OllamaMessage>,
) -> AppResult<()> {
    let mut iteration = 0;
    // V13: 简单任务也使用 Observation Buffer
    let mut observation_buffer = ObservationBuffer::new(10);
    // V15: Context Engine
    let mut context_engine = ContextEngine::new();
    {
        let metrics = crate::monitoring::get_system_metrics();
        let mut snapshot = SystemStateSnapshot::from_metrics(&metrics);
        snapshot.active_window = get_active_window_simple();
        snapshot.process_count = get_process_count_simple();
        context_engine.update_system_state(snapshot);
    }

    loop {
        if iteration >= max_iterations {
            task.set_status(TaskStatus::Failed);
            task.summary = Some("达到最大迭代次数".to_string());
            let _ = tx.send(ChatEvent::Error("已达到最大迭代次数".to_string())).await;
            break;
        }
        iteration += 1;

        let _ = tx.send(ChatEvent::Thinking).await;

        let tools_param = if tools_json.is_empty() { None } else { Some(tools_json.to_vec()) };

        // V15: Context Engine 注入（系统状态 + 近期观察）
        let mut step_messages = messages.clone();
        let obs_vec: Vec<Observation> = observation_buffer.get_all().to_vec();
        let context_injection = context_engine.build_system_prompt_injection(task, &obs_vec);
        if !context_injection.is_empty() {
            step_messages.push(OllamaMessage::system(&context_injection));
        }

        let mut stream = ollama
            .chat_stream(model, step_messages, tools_param, temperature)
            .await?;

        let mut assistant_text = String::new();
        let mut pending_tool_calls: Vec<ToolCall> = Vec::new();

        // 读取流式响应（30秒超时，防止 Ollama 挂起时永久卡死）
        loop {
            let chunk = match timeout(Duration::from_secs(30), stream.next()).await {
                Ok(Some(chunk)) => chunk,
                Ok(None) => break,
                Err(_) => {
                    let _ = tx.send(ChatEvent::Error("LLM 响应超时，请检查 Ollama 服务".to_string())).await;
                    return Ok(());
                }
            };
            match chunk {
                ChatChunk::Text(text) => {
                    assistant_text.push_str(&text);
                    let _ = tx.send(ChatEvent::Text(text)).await;
                }
                ChatChunk::ToolCall(tc) => {
                    pending_tool_calls.push(tc.clone());
                    let _ = tx.send(ChatEvent::ToolCall {
                        name: tc.function.name.clone(),
                        arguments: tc.function.arguments.clone(),
                    }).await;
                }
                ChatChunk::Done => break,
                ChatChunk::Error(e) => {
                    let _ = tx.send(ChatEvent::Error(e)).await;
                    return Ok(());
                }
            }
        }

        let (display_text, simulated_calls) = extract_tool_calls(&assistant_text);
        for tc in &simulated_calls {
            let _ = tx.send(ChatEvent::ToolCall {
                name: tc.function.name.clone(),
                arguments: tc.function.arguments.clone(),
            }).await;
        }
        pending_tool_calls.extend(simulated_calls);

        let tool_calls_json = if pending_tool_calls.is_empty() {
            None
        } else {
            Some(serde_json::to_string(&pending_tool_calls)?)
        };

        {
            let db_guard = db.lock().await;
            let mgr = ConversationManager::new(db_guard.connection());
            mgr.append_message(
                session_id,
                "assistant",
                &display_text,
                tool_calls_json.as_deref(),
                None,
            )?;
        }

        if pending_tool_calls.is_empty() {
            task.complete_step(1, &display_text);
            task.set_status(TaskStatus::Completed);
            task.summary = Some(display_text);
            break;
        }

        // 执行工具（带 Verification + Recovery）
        let mut all_success = true;
        let mut results: Vec<String> = Vec::new();
        let verification_engine = VerificationEngine::new();
        let recovery_engine = RecoveryEngine::new(2);

        for tc in pending_tool_calls {
            let mut current_args = tc.function.arguments.clone();
            let current_tool = tc.function.name.clone();
            // V21: 从持久化的 TaskStep 中恢复 recovery_attempts
            let mut retry_count = task.plan.iter()
                .find(|s| s.step_id == 1)
                .map(|s| s.recovery_attempts)
                .unwrap_or(0);
            let tool_success;
            let tool_content_final;

            'simple_retry: loop {
                let request = ToolCallRequest {
                    tool_name: current_tool.clone(),
                    arguments: current_args.clone(),
                    session_id: Some(session_id.to_string()),
                };

                let exec_result = executor.execute(request).await?;
                let tool_content = exec_result.to_tool_message_content();

                let _ = tx.send(ChatEvent::ToolResult {
                    name: exec_result.tool_name.clone(),
                    success: exec_result.success,
                    content: tool_content.clone(),
                }).await;

                // Verification: 验证真实结果
                match verification_engine.verify(
                    &current_tool,
                    &current_args,
                    &exec_result.result,
                ) {
                    VerificationResult::Pass { summary } => {
                        results.push(format!("{} (已验证: {})", current_tool, summary));
                        let obs = Observation::from_tool_result(
                            &task.id, None, &current_tool, true,
                            exec_result.result.data.clone().unwrap_or(serde_json::Value::Null),
                            summary.clone(),
                        );
                        observation_buffer.push(obs.clone());
                        persist_observation(db, &obs).await;
                        context_engine.add_tool_result(&current_tool, true, &summary);
                        tool_success = true;
                        tool_content_final = tool_content;
                        break 'simple_retry;
                    }
                    VerificationResult::Fail { reason } => {
                        results.push(format!("{} (验证失败: {})", current_tool, reason));
                        let obs = Observation::from_tool_result(
                            &task.id, None, &current_tool, false,
                            exec_result.result.data.clone().unwrap_or(serde_json::Value::Null),
                            reason.clone(),
                        );
                        observation_buffer.push(obs.clone());
                        persist_observation(db, &obs).await;
                        context_engine.add_tool_result(&current_tool, false, &reason);

                        // V14: Recovery
                        let recovery = recovery_engine.analyze_and_recover(
                            &current_tool, &current_args, &exec_result.result, retry_count,
                        );
                        match recovery {
                            RecoveryResult::Recovered { strategy, message } => {
                                let strategy_name = match &strategy {
                                    RecoveryStrategy::Retry { .. } => "retry",
                                    RecoveryStrategy::AdjustParams { .. } => "adjust_params",
                                    RecoveryStrategy::SwitchTool { .. } => "switch_tool",
                                    RecoveryStrategy::Replan { .. } => "replan",
                                    RecoveryStrategy::Escalate { .. } => "escalate",
                                    RecoveryStrategy::Fail { .. } => "fail",
                                };
                                let _ = tx.send(ChatEvent::Recovery(RecoveryEvent::new(
                                    &task.id, 1, &current_tool, retry_count + 1, strategy_name, &message,
                                ))).await;
                                let _ = tx.send(ChatEvent::Text(format!("\n[恢复] {}: {}\n", current_tool, message))).await;

                                // V21: 持久化恢复状态
                                if let Some(step) = task.plan.iter_mut().find(|s| s.step_id == 1) {
                                    step.record_recovery(strategy_name);
                                }
                                update_task_and_notify(task, db, tx).await?;

                                match strategy {
                                    RecoveryStrategy::Retry { backoff_ms } => {
                                        if backoff_ms > 0 {
                                            tokio::time::sleep(Duration::from_millis(backoff_ms)).await;
                                        }
                                        retry_count += 1;
                                        continue 'simple_retry;
                                    }
                                    RecoveryStrategy::AdjustParams { adjusted_arguments } => {
                                        current_args = adjusted_arguments;
                                        retry_count += 1;
                                        continue 'simple_retry;
                                    }
                                    RecoveryStrategy::SwitchTool { alternative_tool, adjusted_arguments } => {
                                        // 简单任务：执行替代工具后用结果重试
                                        let alt_request = ToolCallRequest {
                                            tool_name: alternative_tool.clone(),
                                            arguments: adjusted_arguments.clone(),
                                            session_id: Some(session_id.to_string()),
                                        };
                                        let alt_result = executor.execute(alt_request).await?;
                                        let alt_content = alt_result.to_tool_message_content();
                                        let _ = tx.send(ChatEvent::ToolResult {
                                            name: alternative_tool.clone(),
                                            success: alt_result.success,
                                            content: alt_content.clone(),
                                        }).await;
                                        messages.push(OllamaMessage::tool(&alternative_tool, &alt_content));
                                        if alt_result.success {
                                            if let Some(paths) = alt_result.result.data.as_ref()
                                                .and_then(|d| d.get("paths")).and_then(|p| p.as_array())
                                            {
                                                if let Some(first_path) = paths.first().and_then(|p| p.as_str()) {
                                                    current_args = serde_json::json!({ "name_or_path": first_path });
                                                    retry_count += 1;
                                                    continue 'simple_retry;
                                                }
                                            }
                                        }
                                        tool_success = false;
                                        tool_content_final = alt_content;
                                        break 'simple_retry;
                                    }
                                    RecoveryStrategy::Replan { reason } | RecoveryStrategy::Escalate { reason } => {
                                        let _ = tx.send(ChatEvent::Text(format!("\n[需要注意] {}\n", reason))).await;
                                        tool_success = false;
                                        tool_content_final = tool_content;
                                        break 'simple_retry;
                                    }
                                    RecoveryStrategy::Fail { reason: fail_reason } => {
                                        tool_success = true;
                                        tool_content_final = format!("{}: {}", current_tool, fail_reason);
                                        results.push(tool_content_final.clone());
                                        break 'simple_retry;
                                    }
                                }
                            }
                            RecoveryResult::Failed { reason } => {
                                tool_success = false;
                                tool_content_final = tool_content;
                                let _ = tx.send(ChatEvent::Text(format!("\n[恢复失败] {}: {}\n", current_tool, reason))).await;
                                break 'simple_retry;
                            }
                        }
                    }
                }
            }

            if !tool_success {
                all_success = false;
            }

            // 保存 tool 消息到历史
            {
                let db_guard = db.lock().await;
                let mgr = ConversationManager::new(db_guard.connection());
                mgr.append_message(session_id, "tool", &tool_content_final, None, Some(&current_tool))?;
            }
            messages.push(OllamaMessage::tool(&current_tool, &tool_content_final));
        }

        let last_assistant = if let Some(ref tc_json) = tool_calls_json {
            let tcs: Vec<ToolCall> = serde_json::from_str(tc_json)?;
            OllamaMessage::assistant_with_tools(&display_text, tcs)
        } else {
            OllamaMessage::assistant(&display_text)
        };
        messages.push(last_assistant);

        if !all_success {
            task.fail_step(1, &results.join("; "));
            task.set_status(TaskStatus::Failed);
            break;
        }

        // 简单任务通常一轮 tool call 就结束，但允许继续
    }

    Ok(())
}

// ============================================================
// 工具函数
// ============================================================

/// V13: 持久化 Observation 到数据库（失败仅打印日志，不阻塞主流程）
async fn persist_observation(db: &Arc<Mutex<Database>>, obs: &Observation) {
    let db_guard = db.lock().await;
    if let Err(e) = db_guard.insert_observation(obs) {
        eprintln!("[Observation] 持久化失败: {}", e);
    }
}

/// 更新 Task 并通知前端
async fn update_task_and_notify(
    task: &Task,
    db: &Arc<Mutex<Database>>,
    tx: &tokio::sync::mpsc::Sender<ChatEvent>,
) -> AppResult<()> {
    {
        let db_guard = db.lock().await;
        let task_mgr = TaskManager::new(db_guard.connection());
        task_mgr.update(task)?;
    }
    let _ = tx.send(ChatEvent::TaskUpdate(TaskUpdateEvent::from_task(task))).await;
    Ok(())
}

/// 简单任务判定
///
/// 第一层：关键词快速预判（零延迟），命中常见简单模式直接返回
/// 第二层：LLM 快速分析（兜底），判断是否为单步可完成的简单任务
async fn classify_simple_task(
    ollama: &OllamaClient,
    model: &str,
    user_content: &str,
    temperature: f32,
) -> AppResult<bool> {
    // 第一层：关键词快速预判（零延迟）
    if let Some(is_simple) = quick_classify_simple_task(user_content) {
        eprintln!("[Agent] 简单任务关键词预判: {} → {}", user_content, is_simple);
        return Ok(is_simple);
    }

    // 第二层：LLM 判定（兜底）
    let prompt = format!(
        r#"判断以下用户请求是否为简单任务。

简单任务定义：单条工具调用即可完成，无需前置观察或后续验证。例如：
- "打开 Chrome" -> 简单
- "截图" -> 简单
- "运行计算器" -> 简单
- "整理桌面文件" -> 复杂（需多步）
- "找出昨天下载的PDF" -> 复杂（需先观察再决策）
- "把下载的PDF移到文档" -> 复杂（需多步）

用户请求："{}"

请只回答一个单词：SIMPLE 或 COMPLEX"#,
        user_content
    );

    let messages = vec![OllamaMessage::user(&prompt)];
    let (content, _tool_calls) = ollama.chat(model, messages, None, temperature).await?;

    let content_lower = content.to_lowercase();
    Ok(content_lower.contains("simple") || content_lower.contains("简单"))
}

/// 第一层：关键词快速预判简单任务（零延迟）
///
/// 返回 Some(true) 表示明确是简单任务，Some(false) 表示明确是复杂任务，None 表示需要 LLM 兜底
fn quick_classify_simple_task(user_content: &str) -> Option<bool> {
    let lower = user_content.to_lowercase();
    let trimmed = lower.trim();

    // 空消息视为简单（直接回复）
    if trimmed.is_empty() {
        return Some(true);
    }

    // 复杂任务关键词（命中 → 复杂）
    let complex_patterns = [
        "整理", "清理", "找出", "搜索.*并", "把.*移到", "然后", "接着",
        "帮我.*然后", "先.*再", "首先.*然后", "批量", "全部", "所有",
        "对比", "比较", "分析", "统计", "汇总", "导出", "导入",
        "安装", "卸载", "配置", "设置.*并", "创建.*并",
    ];
    for pattern in &complex_patterns {
        // 简单子串匹配（不用 regex 避免依赖）
        if pattern.contains(".*") {
            // 通配符模式：拆成前后两部分，检查是否都存在且顺序正确
            let parts: Vec<&str> = pattern.split(".*").collect();
            if parts.len() == 2 {
                if let Some(pos1) = trimmed.find(parts[0]) {
                    if trimmed[pos1 + parts[0].len()..].find(parts[1]).is_some() {
                        return Some(false);
                    }
                }
            }
        } else if trimmed.contains(pattern) {
            return Some(false);
        }
    }

    // 简单任务关键词（命中 → 简单）
    let simple_patterns = [
        // 程序操作
        "打开", "启动", "运行", "open ", "launch ", "run ", "start ",
        "关闭", "退出", "close ", "quit ", "exit ", "kill ",
        "重启", "restart ", "reboot",
        // 系统操作
        "截图", "截屏", "screenshot", "snap",
        "锁屏", "锁定", "lock",
        "睡眠", "sleep",
        "关机", "shutdown", "power off",
        "静音", "unmute", "音量",
        "亮度",
        // 文件操作（单步）
        "查看", "列出", "list ", "show ",
        "读取", "读", "read ", "cat ",
        // 记忆操作
        "记住", "记忆", "remember", "memorize",
        "回忆", "recall", "检索记忆",
        "忘记", "删除记忆", "forget",
        // 应用状态查询
        "在运行吗", "是否运行", "状态", "status",
        "进程", "process",
        "窗口", "window",
        // 系统信息
        "系统信息", "系统状态", "system info", "system status",
        "cpu", "内存", "磁盘", "disk",
    ];
    for pattern in &simple_patterns {
        if trimmed.contains(pattern) {
            return Some(true);
        }
    }

    // 纯闲聊（无操作词）→ 简单（直接回复，不需要工具）
    let action_words = ["打开", "启动", "运行", "关闭", "退出", "查看", "列出", "读取",
        "写入", "删除", "移动", "重命名", "复制", "创建", "执行", "截图", "点击",
        "输入", "导航", "滚动", "等待", "记住", "回忆", "忘记", "重启", "关机",
        "睡眠", "锁定", "安装", "卸载", "整理", "清理", "搜索", "找出"];
    let has_action = action_words.iter().any(|w| trimmed.contains(w));
    if !has_action {
        return Some(true); // 纯闲聊，简单任务
    }

    // 未命中任何模式 → 需要 LLM 兜底
    None
}

/// 生成执行计划（V21: 支持按需工具列表）
///
/// 请求 LLM 将复杂任务拆解为步骤列表。
async fn generate_plan(
    ollama: &OllamaClient,
    model: &str,
    user_content: &str,
    tool_registry: &ToolRegistry,
    tool_names: &[String],
    temperature: f32,
) -> AppResult<Vec<String>> {
    let tools_desc = tool_names
        .iter()
        .filter_map(|name| tool_registry.get(name))
        .map(|tool| format!("- {}: {}", tool.name(), tool.description()))
        .collect::<Vec<_>>()
        .join("\n");

    let prompt = format!(
        r#"你是一个任务规划助手。请将以下用户请求拆解为具体的执行步骤。

可用工具：
{}

用户请求："{}"

请按以下格式输出步骤（每行一步，不要编号）：
步骤描述（简洁，不超过20字）

最多 10 步。如果任务很简单只需 1 步，直接输出那一步。"#,
        tools_desc, user_content
    );

    let messages = vec![OllamaMessage::user(&prompt)];
    let (content, _tool_calls) = ollama.chat(model, messages, None, temperature).await?;

    let plan: Vec<String> = content
        .lines()
        .map(|line| line.trim())
        .filter(|line| !line.is_empty() && !line.starts_with("可用工具") && !line.starts_with("用户请求"))
        .take(10)
        .map(|s| s.trim_start_matches(|c: char| c.is_numeric() || c == '.' || c == ' ').to_string())
        .collect();

    if plan.is_empty() {
        Ok(vec!["执行请求".to_string()])
    } else {
        Ok(plan)
    }
}

/// 构建任务上下文提示（V21: 支持按需工具列表）
fn build_task_context(task: &Task, tool_registry: &ToolRegistry, tool_names: &[String]) -> String {
    let tools_desc = tool_names
        .iter()
        .filter_map(|name| tool_registry.get(name))
        .map(|tool| format!("- {}: {}", tool.name(), tool.description()))
        .collect::<Vec<_>>()
        .join("\n");

    let plan_desc = task.plan.iter().map(|s| {
        let status_icon = match s.status {
            StepStatus::Completed => "✅",
            StepStatus::Executing => "▶️",
            StepStatus::Failed => "❌",
            StepStatus::Skipped => "⏭️",
            StepStatus::Pending => "⏳",
        };
        format!("{} {}. {}", status_icon, s.step_id, s.action)
    }).collect::<Vec<_>>().join("\n");

    let observations_desc = if task.observations.is_empty() {
        "暂无".to_string()
    } else {
        task.observations.iter().map(|o| {
            format!("- 步骤 {}: {}", o.step_id, o.result)
        }).collect::<Vec<_>>().join("\n")
    };

    let current_step_desc = task.current_step_ref()
        .map(|s| format!("{}. {}", s.step_id, s.action))
        .unwrap_or_else(|| "无".to_string());

    format!(
        r#"## 当前任务上下文

目标：{}
状态：{}
进度：{} / {} 步

### 执行计划
{}

### 已完成观察
{}

### 当前步骤
{}

### 可用工具
{}

请执行当前步骤，直接调用工具。如果当前步骤已完成，总结结果。"#,
        task.goal,
        task.status.as_str(),
        task.completed_count(),
        task.total_steps(),
        plan_desc,
        observations_desc,
        current_step_desc,
        tools_desc,
    )
}

/// 构建 System Prompt（V21: 支持按需工具列表）
fn build_system_prompt_with_tools(
    tool_registry: &ToolRegistry,
    tool_names: &[String],
    memory_context: &str,
) -> String {
    let mut tools_desc = String::new();

    for name in tool_names {
        if let Some(tool) = tool_registry.get(name) {
            tools_desc.push_str(&format!(
                "\n- {}: {} (风险等级: {})",
                tool.name(),
                tool.description(),
                tool.risk_level().as_str()
            ));
        }
    }

    let memory_section = if memory_context.is_empty() {
        String::new()
    } else {
        format!("\n## 关于用户的记忆\n以下是从长期记忆中检索到的与当前对话相关的信息，请在回答时参考：\n{}\n", memory_context)
    };

    // 浏览器环境信息（V21: 注入系统已安装的浏览器，避免 LLM 调用不存在的 Chrome）
    let browser_env = crate::browser::get_browser_environment_description();
    let browser_section = format!("\n## 系统浏览器环境\n{}\n调用 browser_open 时无需指定 browser 参数，系统会自动选择可用浏览器。\n", browser_env);

    // 当前可用类别描述
    let categories_desc: Vec<String> = tool_names
        .iter()
        .filter_map(|n| tool_registry.get_category(n))
        .collect::<std::collections::HashSet<_>>()
        .into_iter()
        .map(|c| c.as_str().to_string())
        .collect();

    format!(
        r#"你是 PC Guardian AI，一个本地优先的 Windows 桌面 AI Agent。

## 核心原则
- 安全第一：所有系统操作必须经过权限校验
- 透明可信：执行任何操作前必须向用户说明原因
- 精确执行：严格按照工具定义的参数 schema 调用工具
- 主动执行：你是用户的本地助手，可以直接操作用户的电脑。当用户要求执行操作时，直接调用工具执行，不要只是给出文字指导
{}{}
## 可用工具（当前已加载类别: {}）{}

## 执行能力指南
当用户要求以下操作时，直接调用对应工具执行，不要只给出操作步骤：
- "打开QQ/微信/记事本" -> 使用 `launch_program` 工具
- "打开 xxx.txt" -> 使用 `open_file` 工具
- "查看C盘有什么" -> 使用 `list_directory` 工具
- "读取这个文件" -> 使用 `read_file` 工具
- "截屏" -> 使用 `take_screenshot` 工具
- "运行 xxx 命令" -> 使用 `execute_command` 工具
- "记住xxx" / "别忘了xxx" -> 使用 `remember` 工具保存到长期记忆
- "回忆xxx" / "之前说过什么" -> 使用 `recall` 工具检索长期记忆
- "忘掉xxx" -> 使用 `forget` 工具删除记忆

## 工具调用格式
当你需要调用工具时，在回复末尾输出以下格式的 JSON：

<tool_call>
{{
  "name": "工具名",
  "arguments": {{ ...参数... }}
}}
</tool_call>

示例：
用户说"打开记事本"
你的回复：
好的，我来帮你打开记事本。

<tool_call>
{{
  "name": "launch_program",
  "arguments": {{"name_or_path": "notepad"}}
}}
</tool_call>

重要规则：
- 只输出纯 JSON，不要加 markdown 代码块标记
- 如果需要调用多个工具，输出多个 <tool_call> 块
- 不要给出操作步骤，直接调用工具执行
- 工具调用块必须放在回复末尾
- 如果需要的工具不在当前可用列表中，在思考中说明需要哪个类别的工具

## 调用规则
1. 只有当工具确实能解决问题时才调用工具
2. 严格按照工具的 parameters schema 传递参数
3. 一次可以调用多个工具（如果它们是独立的）
4. 工具调用后，等待执行结果再继续对话
5. 如果工具执行失败，向用户说明原因，不要重试

## 安全限制
- SAFE/LOW 风险工具：直接执行
- MEDIUM 风险工具：需要用户确认
- HIGH 风险工具：需要二次确认
- CRITICAL 风险工具：禁止执行

当前日期: {}
"#,
        memory_section,
        browser_section,
        categories_desc.join(", "),
        tools_desc,
        chrono::Local::now().format("%Y-%m-%d %H:%M:%S"),
    )
}

/// 构建 System Prompt（兼容旧接口，加载全部工具）
#[allow(dead_code)]
fn build_system_prompt(tool_registry: &ToolRegistry, memory_context: &str) -> String {
    let all_names = tool_registry.list_names();
    build_system_prompt_with_tools(tool_registry, &all_names, memory_context)
}

/// 构建记忆上下文（从长期记忆中检索相关记忆并格式化）
fn build_memory_context(user_content: &str) -> String {
    use crate::memory::MemoryEngine;

    let conn = match crate::memory::open_connection() {
        Some(c) => c,
        None => return String::new(),
    };

    let memories = match MemoryEngine::get_relevant_memories(&conn, user_content, 5) {
        Ok(m) => m,
        Err(_) => return String::new(),
    };

    if memories.is_empty() {
        return String::new();
    }

    let mut context = String::new();
    for (i, mem) in memories.iter().enumerate() {
        let type_label = match mem.memory_type {
            crate::memory::MemoryType::UserPreference => "用户偏好",
            crate::memory::MemoryType::TaskHistory => "任务历史",
            crate::memory::MemoryType::Knowledge => "知识",
            crate::memory::MemoryType::ConversationSummary => "对话摘要",
            crate::memory::MemoryType::AppUsage => "应用习惯",
        };
        context.push_str(&format!("{}. [{}] {}\n", i + 1, type_label, mem.content));
    }
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

// ============================================================
// V15: 系统状态辅助函数
// ============================================================

/// 获取活动窗口标题（简单版，用于 Context Engine）
#[cfg(target_os = "windows")]
fn get_active_window_simple() -> Option<String> {
    use std::process::Command;
    use std::os::windows::process::CommandExt;
    let output = Command::new("powershell")
        .args(["-NoProfile", "-NonInteractive", "-WindowStyle", "Hidden", "-Command",
            "Get-Process | Where-Object { $_.MainWindowTitle -ne '' } | Sort-Object StartTime -Descending | Select-Object -First 1 -ExpandProperty MainWindowTitle"])
        .creation_flags(0x08000000)
        .output().ok()?;
    let text = String::from_utf8_lossy(&output.stdout);
    let trimmed = text.trim();
    if trimmed.is_empty() { None } else { Some(trimmed.to_string()) }
}

#[cfg(not(target_os = "windows"))]
fn get_active_window_simple() -> Option<String> { None }

/// 获取进程总数（简单版，用于 Context Engine）
#[cfg(target_os = "windows")]
fn get_process_count_simple() -> usize {
    use std::process::Command;
    use std::os::windows::process::CommandExt;
    let output = Command::new("powershell")
        .args(["-NoProfile", "-NonInteractive", "-WindowStyle", "Hidden", "-Command", "(Get-Process).Count"])
        .creation_flags(0x08000000)
        .output().ok();
    match output {
        Some(out) => String::from_utf8_lossy(&out.stdout).trim().parse::<usize>().unwrap_or(0),
        None => 0,
    }
}

#[cfg(not(target_os = "windows"))]
fn get_process_count_simple() -> usize { 0 }

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
