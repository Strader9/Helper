//! 低延迟自适应执行器。
//!
//! 与旧 ReAct 流程不同，这里不预先调用模型做分类、复杂度判断和计划生成。
//! 一次模型调用直接决定回复或工具调用；工具成功后能确定结果的操作直接结束，
//! 只有确实需要读取结果继续推理时才进入下一轮。

use std::sync::Arc;

use tokio::sync::Mutex;
use tokio::time::{timeout, Duration};
use tokio_stream::StreamExt;

use crate::db::conversation::ConversationManager;
use crate::db::Database;
use crate::error::AppResult;
use crate::llm::{ChatChunk, OllamaClient, OllamaMessage, ToolCall};
use crate::tools::executor::{ToolCallRequest, ToolExecutor};
use crate::tools::ToolRegistry;

use super::task::{ActionStatusEvent, Task, TaskManager, TaskStatus, TaskStep, TaskUpdateEvent};
use super::{extract_tool_calls, ChatEvent, RunCancellation};
use crate::tools::ToolCategory;

const MAX_ADAPTIVE_TURNS: usize = 6;
const MODEL_CHUNK_TIMEOUT_SECS: u64 = 20;
const MAX_TOOL_ROUTING_TEMPERATURE: f32 = 0.2;

/// 对话路径的上下文窗口默认值。
///
/// **必须显式指定。** Ollama 的 `num_ctx` 默认只有 4096 token，而本路径的消息里
/// 光工具定义就占掉大头（每个工具连同 parameters schema 约 80–150 token），
/// 剩下的空间装不下 system prompt、注入的记忆和对话历史 —— 提示词会被静默截断，
/// 表现为模型"记不住用户偏好"。
///
/// 8192 是 8GB 显存下能 100% 跑 GPU 的上限——再大就会有一部分层落到 CPU，
/// 单轮延迟明显变长。用户可在设置里调（`agent.num_ctx`），调大前应先确认
/// 自己的显存扛得住。
const DEFAULT_NUM_CTX: u32 = 8192;

/// 单轮输出 token 上限的默认值。
///
/// `num_ctx` 只是窗口大小、不是输出上限：上下文写满后 Ollama 会滑动窗口继续生成，
/// `num_predict: -1` 意味着模型可以无限写下去。对话场景的回答不需要很长，
/// 1024 足够放下带工具调用的回复，同时保证「提示词 + 单轮输出」不会撑爆窗口。
const DEFAULT_NUM_PREDICT: i64 = 1024;

/// 从设置读取本轮对话要用的 (num_ctx, num_predict)。
///
/// 每次发起对话都读一次，用户在设置页改完下一轮即生效，不必重启。
/// 读不到（锁被占用 / 值非法）就退回默认值——绝不因为一个配置项而
/// 中断对话。
fn resolve_context_options(db: &Arc<Mutex<Database>>) -> (u32, i64) {
    let (num_ctx, num_predict) = match db.try_lock() {
        Ok(guard) => (
            guard
                .get_setting("agent.num_ctx")
                .ok()
                .flatten()
                .and_then(|v| v.trim().parse::<u32>().ok())
                .map(|v| v.clamp(2048, 32768))
                .unwrap_or(DEFAULT_NUM_CTX),
            guard
                .get_setting("agent.num_predict")
                .ok()
                .flatten()
                .and_then(|v| v.trim().parse::<i64>().ok())
                .map(|v| v.clamp(128, 8192))
                .unwrap_or(DEFAULT_NUM_PREDICT),
        ),
        Err(_) => (DEFAULT_NUM_CTX, DEFAULT_NUM_PREDICT),
    };
    (num_ctx, num_predict)
}

/// 生成远期摘要的超时（秒）。
///
/// 摘要是「锦上添花」的一步，不能因为它拖住整个对话。超时就放弃本次压缩、
/// 保留原有摘要不变，而不是让用户干等。
const SUMMARY_TIMEOUT_SECS: u64 = 25;

pub(crate) struct JarvisContext<'a> {
    pub session_id: &'a str,
    pub user_content: &'a str,
    pub db: &'a Arc<Mutex<Database>>,
    pub ollama: &'a OllamaClient,
    pub executor: &'a ToolExecutor,
    pub tool_registry: &'a ToolRegistry,
    pub model: &'a str,
    pub temperature: f32,
    pub max_iterations: usize,
    pub tx: &'a tokio::sync::mpsc::Sender<ChatEvent>,
    pub categories: &'a [ToolCategory],
    pub run_id: &'a str,
    pub cancellation: &'a RunCancellation,
}

pub(crate) async fn run(context: JarvisContext<'_>, mut task: Task) -> AppResult<()> {
    let active_tool_names = context
        .tool_registry
        .list_names_by_categories(context.categories);
    let tools_json = build_tools_json(context.tool_registry, &active_tool_names);

    // 窗口与输出上限从设置读取，用户可在「设置 → 通用设置 → 生成参数」调整。
    let (num_ctx, num_predict) = resolve_context_options(context.db);

    // 把检索到的长期记忆拼进 System Prompt。
    //
    // 原先这里只有 `build_jarvis_prompt()` 那个静态字符串，记忆只在闲聊快速路径
    // （`run_chat_fast`）里注入 —— 结果就是用户明确说过的偏好，在真正需要执行操作的
    // 路径上反而看不见。用户偏好属于「任何时候都该生效」的信息，不该依赖检索撞运气。
    let system_prompt = {
        let memory = super::build_memory_context(context.user_content);
        if memory.trim().is_empty() {
            build_jarvis_prompt()
        } else {
            format!(
                "{}\n\n## 关于用户的长期记忆\n以下是此前确认过的用户偏好与事实，执行时应当遵守：\n{}",
                build_jarvis_prompt(),
                memory
            )
        }
    };

    // 按 token 预算分级组装历史，而不是按条数一刀切。
    //
    // 原先的 `take(10)` 是按条数取：第 11 轮之前的对话从数据库角度就没被读进来，
    // 用户第一轮交代的背景（"我在改 v7 分支"）到第 11 轮会凭空消失。
    // 现在改为：先算出留给历史的预算，再让 `assemble_history` 从最新往回装，
    // 装不下的部分归入「远期」，用缓存的摘要代替。
    let (history, cached_summary, to_summarize) = {
        let db = context.db.lock().await;
        let manager = ConversationManager::new(db.connection());
        let all = manager.get_messages(context.session_id)?;

        // 已缓存的摘要（只压一次，覆盖到 summary_upto_id）
        let cached = manager.get_summary(context.session_id).ok().flatten();
        let already_covered = cached.as_ref().map(|(_, upto)| *upto).unwrap_or(0);

        // 预算 = 窗口 - 工具定义 - system prompt - 输出余量 - 已有摘要
        let tools_tokens = tools_json
            .iter()
            .map(|t| {
                t.get("function")
                    .and_then(|f| f.get("description"))
                    .and_then(|d| d.as_str())
                    .map(|d| super::history::estimate_tokens(d) + 60)
                    .unwrap_or(60)
            })
            .sum::<usize>()
            + 200; // 工具调用的外层结构开销
        let system_tokens = super::history::estimate_tokens(&system_prompt);
        let summary_tokens = cached
            .as_ref()
            .map(|(s, _)| super::history::estimate_tokens(s))
            .unwrap_or(0);
        let budget = (num_ctx as usize)
            .saturating_sub(tools_tokens)
            .saturating_sub(system_tokens)
            .saturating_sub(summary_tokens)
            .saturating_sub(super::history::OUTPUT_RESERVE_TOKENS);

        let assembled = super::history::assemble_history(&all, budget);

        // 远期条目中，已被缓存摘要覆盖的部分不再重复压缩
        let pending: Vec<_> = all
            .iter()
            .take(assembled.pending_summary_count)
            .filter(|m| m.id > already_covered)
            .cloned()
            .collect();

        (
            assembled.messages,
            cached.map(|(s, _)| s),
            pending,
        )
    };

    let mut messages = vec![OllamaMessage::system(&system_prompt)];

    // 有新的远期内容需要压缩时，先做一次摘要并落库缓存。
    //
    // 这一步只在该轮首次「撑爆预算」时触发：压缩结果连同覆盖范围写回会话，
    // 后续轮次直接复用（`already_covered` 过滤掉了已覆盖的条目）。
    let cached_summary = if to_summarize.is_empty() {
        cached_summary
    } else {
        let flat = super::history::flatten_for_summary(&to_summarize);
        let upto_id = to_summarize.last().map(|m| m.id).unwrap_or(0);
        // 已有摘要时，把新段接在旧摘要后面，保持时间顺序
        let combined_source = match &cached_summary {
            Some(prev) if !prev.trim().is_empty() => {
                format!("（更早的内容摘要）{}\n\n{}", prev, flat)
            }
            _ => flat,
        };

        match summarize_history(context.ollama, context.model, &combined_source).await {
            Some(summary) => {
                let db = context.db.lock().await;
                let manager = ConversationManager::new(db.connection());
                if manager.save_summary(context.session_id, &summary, upto_id).is_ok() {
                    Some(summary)
                } else {
                    cached_summary // 落库失败：沿用旧摘要，不用这次的
                }
            }
            // 压缩失败：保留原状（用户偏好——拿不准就不改，绝不交出残缺内容）
            None => cached_summary,
        }
    };

    // 远期摘要以 user 消息形式注入在历史之前 —— 用明确的标题让模型知道
    // 这是「更早聊过的内容」，而不是当前正在发生的事。
    if let Some(summary) = cached_summary.filter(|s| !s.trim().is_empty()) {
        messages.push(OllamaMessage::user(&format!(
            "【此前对话摘要】以下是本次对话更早的内容，供你回忆上下文：\n{}",
            summary
        )));
    }

    messages.extend(history);

    task.is_simple = true;
    task.set_plan(vec![TaskStep::new(1, "理解并执行指令")]);
    task.start_step(1);
    update_task(&task, context.db, context.tx).await?;
    emit_status(&task, context.tx, "routing", "正在选择最快执行路径").await;

    let max_turns = context.max_iterations.clamp(1, MAX_ADAPTIVE_TURNS);
    for turn in 0..max_turns {
        if context.cancellation.is_cancelled() || is_cancelled(&task.id, context.db).await? {
            return Ok(());
        }

        let _ = context.tx.send(ChatEvent::Thinking).await;
        let mut stream = match context
            .ollama
            .chat_stream_with_tools_and_options(
                context.model,
                messages.clone(),
                (!tools_json.is_empty()).then(|| tools_json.clone()),
                context.temperature.min(MAX_TOOL_ROUTING_TEMPERATURE),
                serde_json::json!({
                    "num_ctx": num_ctx,
                    "num_predict": num_predict,
                }),
            )
            .await
        {
            Ok(stream) => stream,
            Err(error) => {
                return fail_task(
                    &mut task,
                    context.db,
                    context.tx,
                    &format!("模型连接失败：{}", error),
                )
                .await;
            }
        };

        let mut assistant_text = String::new();
        let mut tool_calls = Vec::<ToolCall>::new();

        loop {
            let next_chunk = tokio::select! {
                _ = context.cancellation.cancelled() => return Ok(()),
                chunk = timeout(Duration::from_secs(MODEL_CHUNK_TIMEOUT_SECS), stream.next()) => chunk,
            };
            let chunk = match next_chunk {
                Ok(chunk) => chunk,
                Err(_) => {
                    return fail_task(
                        &mut task,
                        context.db,
                        context.tx,
                        "模型响应超时（20秒）",
                    )
                    .await;
                }
            };

            match chunk {
                Some(ChatChunk::Text(text)) => assistant_text.push_str(&text),
                Some(ChatChunk::ToolCall(call)) => tool_calls.push(call),
                Some(ChatChunk::Done(_, _)) | None => break,
                Some(ChatChunk::Error(error)) => {
                    return fail_task(&mut task, context.db, context.tx, &error).await;
                }
            }
        }

        // 兼容不支持原生 tool_calls、但会输出 <tool_call> 的本地模型。
        let (display_text, simulated_calls) = extract_tool_calls(&assistant_text);
        tool_calls.extend(simulated_calls);

        persist_assistant(context.session_id, context.db, &display_text, &tool_calls).await?;

        if tool_calls.is_empty() {
            if display_text.trim().is_empty() {
                return fail_task(
                    &mut task,
                    context.db,
                    context.tx,
                    "模型未返回有效响应",
                )
                .await;
            }
            let _ = context.tx.send(ChatEvent::Text(display_text.clone())).await;
            finish_task(&mut task, context.db, context.tx, &display_text).await?;
            return Ok(());
        }

        messages.push(OllamaMessage::assistant_with_tools(
            &display_text,
            tool_calls.clone(),
        ));

        let one_call = tool_calls.len() == 1;
        for call in tool_calls {
            let tool_name = call.function.name.clone();
            let arguments = call.function.arguments.clone();

            if !active_tool_names.iter().any(|name| name == &tool_name) {
                return fail_task(
                    &mut task,
                    context.db,
                    context.tx,
                    &format!("模型请求了当前未授权的工具：{}", tool_name),
                )
                .await;
            }

            let _ = context
                .tx
                .send(ChatEvent::ToolCall {
                    name: tool_name.clone(),
                    arguments: arguments.clone(),
                })
                .await;
            emit_status(
                &task,
                context.tx,
                "executing",
                &format!("正在执行 {}", tool_name),
            )
            .await;

            let execution_result = tokio::select! {
                _ = context.cancellation.cancelled() => return Ok(()),
                result = context.executor.execute(ToolCallRequest {
                    tool_name: tool_name.clone(),
                    arguments,
                    session_id: Some(context.session_id.to_string()),
                    run_id: Some(context.run_id.to_string()),
                }) => result,
            };
            let execution = match execution_result {
                Ok(execution) => execution,
                Err(error) => {
                    return fail_task(
                        &mut task,
                        context.db,
                        context.tx,
                        &format!("工具执行异常：{}", error),
                    )
                    .await;
                }
            };
            if context.cancellation.is_cancelled() {
                return Ok(());
            }
            let tool_content = execution.to_tool_message_content();

            let _ = context
                .tx
                .send(ChatEvent::ToolResult {
                    name: tool_name.clone(),
                    success: execution.success,
                    content: tool_content.clone(),
                })
                .await;

            persist_tool_message(context.session_id, context.db, &tool_name, &tool_content).await?;
            messages.push(OllamaMessage::tool(&tool_name, &tool_content));

            if !execution.success {
                let message = execution.result.error.as_deref().unwrap_or("工具执行失败");
                // 不再盲目重试同一调用。失败立即反馈，避免一次指令拖到几十秒。
                return fail_task(&mut task, context.db, context.tx, message).await;
            }

            if one_call && is_terminal_tool(&tool_name) {
                let summary = result_summary(&execution.result.data);
                persist_assistant(context.session_id, context.db, &summary, &[]).await?;
                let _ = context.tx.send(ChatEvent::Text(summary.clone())).await;
                finish_task(&mut task, context.db, context.tx, &summary).await?;
                return Ok(());
            }
        }

        emit_status(
            &task,
            context.tx,
            "thinking",
            &format!("已完成第 {} 轮，正在根据结果继续", turn + 1),
        )
        .await;
    }

    fail_task(
        &mut task,
        context.db,
        context.tx,
        "任务超过最大执行轮次，已停止以避免无效循环",
    )
    .await
}

fn build_tools_json(registry: &ToolRegistry, names: &[String]) -> Vec<serde_json::Value> {
    names
        .iter()
        .filter_map(|name| registry.get(name))
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
        .collect()
}

fn build_jarvis_prompt() -> String {
    r#"你是用户电脑中的贾维斯式执行助手。默认使用简体中文。

目标：以最少步骤完成用户指令。可以直接回答时立即回答；需要操作电脑时立即调用工具。

执行规则：
1. 不要先写计划、不要复述请求、不要虚构执行结果。
2. `launch_program` 本身会查找已安装程序；打开应用时直接调用它，不要先调用 `find_program`。
3. 同一个失败调用不得原参数重试。收到错误后简洁说明原因。
4. 一次只做当前请求必要的操作，不做用户未要求的扩展动作。
5. 工具成功且结果中已有 message/summary 时，直接采用该结果，不再额外推理。
6. 严格使用原生工具调用；只有模型不支持原生调用时才输出 <tool_call> 兼容格式。
7. 可用能力以本次请求附带的工具定义为准，不要猜测不存在的工具。"#
        .to_string()
}

/// 把远期对话压成一段摘要。
///
/// 这是「智能清理」的核心：超限时不丢弃、也不无限堆积，而是把更早的内容
/// 浓缩成一段可读的文字继续带着走。摘要**只生成一次并落库缓存**
/// （见 `ConversationManager::save_summary`），后续轮次直接复用，
/// 不重复付出推理代价。
///
/// 失败时返回 `None` —— 拿不准就保留原状，绝不写入一段残缺的摘要
/// 污染后续所有对话。
async fn summarize_history(
    ollama: &OllamaClient,
    model: &str,
    flat: &str,
) -> Option<String> {
    let instruction = format!(
        "请把下面这段对话压缩成一段简洁的中文摘要，供后续对话回忆上下文。\n\
         要求：\n\
         1. 只保留用户交代过的背景、已确认的偏好、已完成和待办的事项\n\
         2. 不要复述寒暄，不要编造未提及的内容\n\
         3. 按时间顺序陈述，直接输出摘要正文，不要任何前缀\n\
         4. 控制在 300 字以内\n\n\
         对话内容：\n{}",
        flat
    );

    let messages = vec![OllamaMessage::user(&instruction)];
    let mut stream = ollama
        .chat_stream_with_options(
            model,
            messages,
            0.3,
            serde_json::json!({
                "num_ctx": 4096,
                "num_predict": 512, // 摘要不需要长输出，同时限制单次耗时
            }),
        )
        .await
        .ok()?;

    let mut out = String::new();
    loop {
        match timeout(Duration::from_secs(SUMMARY_TIMEOUT_SECS), stream.next()).await {
            Ok(Some(ChatChunk::Text(text))) => out.push_str(&text),
            Ok(Some(ChatChunk::Done(_, _))) | Ok(None) => break,
            // 出错或超时：放弃本次摘要，保留原状
            Ok(Some(ChatChunk::Error(_))) | Err(_) => return None,
            Ok(Some(ChatChunk::ToolCall(_))) => {} // 摘要不需要工具，忽略
        }
    }

    let trimmed = out.trim().to_string();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed)
    }
}

fn is_terminal_tool(name: &str) -> bool {
    matches!(
        name,
        "launch_program"
            | "close_program"
            | "take_screenshot"
            | "get_system_context"
            | "is_application_running"
            | "get_application_status"
            | "focus_window"
            | "minimize_window"
            | "close_window"
            | "browser_open"
            | "browser_navigate"
            | "open_file"
            | "remember"
            | "forget"
    )
}

fn result_summary(data: &Option<serde_json::Value>) -> String {
    data.as_ref()
        .and_then(|value| value.get("message").or_else(|| value.get("summary")))
        .and_then(|value| value.as_str())
        .unwrap_or("指令已执行完成")
        .to_string()
}

async fn persist_assistant(
    session_id: &str,
    db: &Arc<Mutex<Database>>,
    content: &str,
    calls: &[ToolCall],
) -> AppResult<()> {
    let tool_calls = (!calls.is_empty())
        .then(|| serde_json::to_string(calls))
        .transpose()?;
    let db = db.lock().await;
    let manager = ConversationManager::new(db.connection());
    manager.append_message(
        session_id,
        "assistant",
        content,
        tool_calls.as_deref(),
        None,
    )?;
    Ok(())
}

async fn persist_tool_message(
    session_id: &str,
    db: &Arc<Mutex<Database>>,
    tool_name: &str,
    content: &str,
) -> AppResult<()> {
    let db = db.lock().await;
    let manager = ConversationManager::new(db.connection());
    manager.append_message(session_id, "tool", content, None, Some(tool_name))?;
    Ok(())
}

async fn update_task(
    task: &Task,
    db: &Arc<Mutex<Database>>,
    tx: &tokio::sync::mpsc::Sender<ChatEvent>,
) -> AppResult<()> {
    {
        let db = db.lock().await;
        TaskManager::new(db.connection()).update(task)?;
    }
    let _ = tx
        .send(ChatEvent::TaskUpdate(TaskUpdateEvent::from_task(task)))
        .await;
    Ok(())
}

async fn emit_status(
    task: &Task,
    tx: &tokio::sync::mpsc::Sender<ChatEvent>,
    status: &str,
    message: &str,
) {
    let _ = tx
        .send(ChatEvent::ActionStatus(ActionStatusEvent {
            task_id: task.id.clone(),
            step: task.current_step,
            action: task
                .current_step_ref()
                .map(|step| step.action.clone())
                .unwrap_or_else(|| "执行指令".to_string()),
            status: status.to_string(),
            message: message.to_string(),
        }))
        .await;
}

async fn finish_task(
    task: &mut Task,
    db: &Arc<Mutex<Database>>,
    tx: &tokio::sync::mpsc::Sender<ChatEvent>,
    summary: &str,
) -> AppResult<()> {
    task.complete_step(1, summary);
    task.set_status(TaskStatus::Completed);
    task.summary = Some(summary.to_string());
    update_task(task, db, tx).await
}

async fn fail_task(
    task: &mut Task,
    db: &Arc<Mutex<Database>>,
    tx: &tokio::sync::mpsc::Sender<ChatEvent>,
    error: &str,
) -> AppResult<()> {
    task.fail_step(1, error);
    task.set_status(TaskStatus::Failed);
    task.summary = Some(error.to_string());
    update_task(task, db, tx).await?;
    let _ = tx.send(ChatEvent::Error(error.to_string())).await;
    Ok(())
}

async fn is_cancelled(task_id: &str, db: &Arc<Mutex<Database>>) -> AppResult<bool> {
    let db = db.lock().await;
    Ok(TaskManager::new(db.connection())
        .get(task_id)?
        .map(|task| task.status == TaskStatus::Cancelled)
        .unwrap_or(true))
}

#[cfg(test)]
mod tests {
    use super::{is_terminal_tool, result_summary};

    #[test]
    fn terminal_tools_finish_without_second_model_call() {
        assert!(is_terminal_tool("launch_program"));
        assert!(is_terminal_tool("get_system_context"));
        assert!(!is_terminal_tool("read_file"));
        assert!(!is_terminal_tool("find_program"));
    }

    #[test]
    fn extracts_human_readable_result() {
        let value = Some(serde_json::json!({"message": "已打开 QQ"}));
        assert_eq!(result_summary(&value), "已打开 QQ");
    }
}
