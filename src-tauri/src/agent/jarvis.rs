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
use super::{convert_messages, extract_tool_calls, ChatEvent, RunCancellation};
use crate::tools::ToolCategory;

const MAX_ADAPTIVE_TURNS: usize = 6;
const MODEL_CHUNK_TIMEOUT_SECS: u64 = 20;
const MAX_TOOL_ROUTING_TEMPERATURE: f32 = 0.2;

pub(crate) struct JarvisContext<'a> {
    pub session_id: &'a str,
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
    let system_prompt = build_jarvis_prompt();

    let history = {
        let db = context.db.lock().await;
        let manager = ConversationManager::new(db.connection());
        let messages = manager.get_messages(context.session_id)?;
        let recent = messages
            .into_iter()
            .rev()
            .take(10)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect::<Vec<_>>();
        convert_messages(&recent)
    };

    let mut messages = vec![OllamaMessage::system(&system_prompt)];
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
            .chat_stream(
                context.model,
                messages.clone(),
                (!tools_json.is_empty()).then(|| tools_json.clone()),
                context.temperature.min(MAX_TOOL_ROUTING_TEMPERATURE),
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
