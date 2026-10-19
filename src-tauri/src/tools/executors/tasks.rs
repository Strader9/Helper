//! 任务编排工具（agents 域）
//!
//! 让助手能自己排计划、报进度、记断点、接着干。
//!
//! 与 LumiCore 的 workflow 那套的差别：那边是"可发布、可复用的流程包"
//! （save/publish/run/pause/reconcile 一整套生命周期），这里只做
//! **单个任务的执行轨迹**——计划、步骤状态、断点、错误。理由很简单：
//! helper 现在连"这一轮干了什么、干到哪一步"都留不下来，先把这个补上，
//! 复用型流程等有真实用例再说。
//!
//! 数据落在已有的 `agent_tasks` 表（schema 里早就有，但此前没有工具能用）。

use async_trait::async_trait;
use serde_json::{json, Value};

use crate::agent::task::{StepStatus, Task, TaskManager, TaskStatus, TaskStep};
use crate::error::{AppError, AppResult};
use crate::tools::{AgentTool, RiskLevel, ToolResult};

// ============================================================
// 公共辅助
// ============================================================

/// 借一个短连接和一个 TaskManager，避免每个工具重复写样板。
fn with_tasks<F, T>(f: F) -> AppResult<T>
where
    F: FnOnce(&TaskManager<'_>) -> AppResult<T>,
{
    let conn = crate::memory::open_connection()
        .ok_or_else(|| AppError::Internal("数据库未初始化".to_string()))?;
    let manager = TaskManager::new(&conn);
    f(&manager)
}

/// 取任务；不存在时报得具体一点。
fn load_task(manager: &TaskManager<'_>, task_id: &str) -> AppResult<Task> {
    manager
        .get(task_id)?
        .ok_or_else(|| AppError::InvalidArgument(format!("找不到任务 {}", task_id)))
}

/// 把入参里的 steps 解析成步骤列表。
///
/// 两种写法都收：纯字符串数组（最常见），或对象数组（要绑定工具时用）。
/// 下标从 1 开始——`current_step` 默认值是 0，表示"还没开始"，
/// 用 0 编号会让"第 0 步"和"没开始"撞在一起。
fn parse_steps(raw: &[Value]) -> AppResult<Vec<TaskStep>> {
    let mut steps = Vec::with_capacity(raw.len());
    for item in raw {
        // 编号按"已收下的步数"排，而不是按入参下标：
        // 中间夹一个空串就出现 step 3 前面是 step 1 的空洞，
        // 模型照着报 step_id 会报错一个"不存在的步骤"。
        let step_id = steps.len() + 1;
        match item {
            Value::String(action) => {
                let action = action.trim();
                if action.is_empty() {
                    continue; // 空串跳过，但不影响后面步骤
                }
                steps.push(TaskStep::new(step_id, action));
            }
            Value::Object(map) => {
                let action = map
                    .get("action")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .ok_or_else(|| {
                        AppError::InvalidArgument(format!("第 {} 步缺少 action", step_id))
                    })?;
                let mut step = TaskStep::new(step_id, action);
                if let Some(tool) = map.get("tool_name").and_then(Value::as_str) {
                    step.tool_name = Some(tool.to_string());
                }
                if let Some(args) = map.get("tool_arguments") {
                    step.tool_arguments = Some(args.clone());
                }
                steps.push(step);
            }
            _ => {
                return Err(AppError::InvalidArgument(format!(
                    "第 {} 步既不是字符串也不是对象",
                    step_id
                )))
            }
        }
    }
    Ok(steps)
}

/// 精简的计划视图：只带界面和模型真正要看的东西。
fn task_brief(task: &Task) -> Value {
    json!({
        "task_id": task.id,
        "goal": task.goal,
        "status": task.status.as_str(),
        "progress_percent": task.progress_percent(),
        "completed_steps": task.completed_count(),
        "total_steps": task.total_steps(),
        "current_step": task.current_step,
        "created_at": task.created_at,
        "updated_at": task.updated_at,
        "summary": task.summary,
    })
}

/// 完整视图：含每条步骤的状态与结果。
fn task_detail(task: &Task) -> Value {
    let steps: Vec<Value> = task
        .plan
        .iter()
        .map(|step| {
            json!({
                "step_id": step.step_id,
                "action": step.action,
                "status": step.status.as_str(),
                "result": step.result,
                "tool_name": step.tool_name,
                "recovery_attempts": step.recovery_attempts,
            })
        })
        .collect();

    let errors: Vec<Value> = task
        .errors
        .iter()
        .map(|err| {
            json!({
                "step_id": err.step_id,
                "error": err.error,
                "recovery_attempts": err.recovery_attempts,
                "timestamp": err.timestamp,
            })
        })
        .collect();

    json!({
        "task_id": task.id,
        "goal": task.goal,
        "status": task.status.as_str(),
        "progress_percent": task.progress_percent(),
        "is_simple": task.is_simple,
        "current_step": task.current_step,
        "steps": steps,
        "errors": errors,
        "summary": task.summary,
        "created_at": task.created_at,
        "updated_at": task.updated_at,
    })
}

// ============================================================
// create_task_plan — 排计划
// ============================================================

/// 把一条复杂指令拆成可跟踪的步骤计划。
pub struct CreateTaskPlanTool;

#[async_trait]
impl AgentTool for CreateTaskPlanTool {
    fn name(&self) -> &'static str {
        "create_task_plan"
    }

    fn description(&self) -> &'static str {
        "把一条需要多步完成的指令拆成可跟踪的计划，落库保存。适合「先备份再改配置最后重启」这类多步骤任务；单步能完成的不要用，直接做。创建后用 update_task_step 逐步推进，中断后用 resume_task_context 接着做。"
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "goal": {
                    "type": "string",
                    "description": "这条计划要达成的目标（一句话）"
                },
                "steps": {
                    "type": "array",
                    "description": "步骤列表。字符串数组，或对象数组（可带 tool_name / tool_arguments 绑定具体工具）",
                    "items": {}
                },
                "is_simple": {
                    "type": "boolean",
                    "description": "是否是简单任务（跳过分析阶段）"
                }
            },
            "required": ["goal", "steps"]
        })
    }

    fn risk_level(&self) -> RiskLevel {
        RiskLevel::Low
    }

    async fn execute(&self, args: Value) -> AppResult<ToolResult> {
        let goal = args["goal"]
            .as_str()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| AppError::InvalidArgument("goal is required".to_string()))?;

        let raw_steps = args["steps"]
            .as_array()
            .ok_or_else(|| AppError::InvalidArgument("steps must be an array".to_string()))?;

        let steps = parse_steps(raw_steps)?;
        if steps.is_empty() {
            return Ok(ToolResult::err(
                "EMPTY_PLAN",
                "步骤列表是空的，没有可跟踪的内容",
            ));
        }

        let session_id = crate::tools::current_session();
        let mut task = with_tasks(|manager| manager.create(&session_id, goal))?;
        task.set_plan(steps);
        task.is_simple = args["is_simple"].as_bool().unwrap_or(false);
        task.set_status(TaskStatus::Planning);
        with_tasks(|manager| manager.update(&task))?;

        // 回读校验：写进去的和读出来的一致才算建成，不拿内存里的对象自证。
        let persisted = with_tasks(|manager| load_task(manager, &task.id))?;
        let verified = persisted.plan.len() == task.plan.len() && persisted.goal == task.goal;

        Ok(ToolResult::ok(json!({
            "task_id": task.id,
            "goal": task.goal,
            "step_count": task.plan.len(),
            "steps": task.plan.iter().map(|s| json!({
                "step_id": s.step_id,
                "action": s.action,
            })).collect::<Vec<_>>(),
            "persisted": verified,
        })))
    }
}

// ============================================================
// list_agent_tasks — 看手上有什么
// ============================================================

pub struct ListAgentTasksTool;

#[async_trait]
impl AgentTool for ListAgentTasksTool {
    fn name(&self) -> &'static str {
        "list_agent_tasks"
    }

    fn description(&self) -> &'static str {
        "列出已创建的任务计划。回答「你手上还有什么没做完」「刚才那个计划进展如何」时用。不带状态则按创建时间倒序列最近任务。"
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "status": {
                    "type": "string",
                    "enum": ["PENDING", "ANALYZING", "PLANNING", "EXECUTING", "VERIFYING", "COMPLETED", "FAILED", "CANCELLED"],
                    "description": "只看这个状态（可选）"
                },
                "limit": {
                    "type": "integer",
                    "minimum": 1,
                    "maximum": 100,
                    "description": "返回条数上限（可选，默认 10）"
                }
            }
        })
    }

    fn risk_level(&self) -> RiskLevel {
        RiskLevel::Safe
    }

    async fn execute(&self, args: Value) -> AppResult<ToolResult> {
        let limit = args["limit"].as_u64().unwrap_or(10).clamp(1, 100) as i64;

        let status = match args["status"].as_str() {
            Some(raw) => match TaskStatus::from_str(raw) {
                Some(s) => Some(s.as_str().to_string()),
                None => {
                    return Ok(ToolResult::err(
                        "INVALID_STATUS",
                        &format!("不认识的状态 {}", raw),
                    ))
                }
            },
            None => None,
        };

        let tasks = with_tasks(|manager| manager.list_recent(status.as_deref(), limit))?;
        let items: Vec<Value> = tasks.iter().map(task_brief).collect();

        Ok(ToolResult::ok(json!({
            "count": items.len(),
            "tasks": items,
        })))
    }
}

// ============================================================
// get_agent_task — 看单个计划细节
// ============================================================

pub struct GetAgentTaskTool;

#[async_trait]
impl AgentTool for GetAgentTaskTool {
    fn name(&self) -> &'static str {
        "get_agent_task"
    }

    fn description(&self) -> &'static str {
        "读一个任务计划的完整内容：每一步的状态与结果、错误记录、进度。回答「这个计划卡在哪」「第三步做了什么」时用。"
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "task_id": { "type": "string", "description": "任务 ID" }
            },
            "required": ["task_id"]
        })
    }

    fn risk_level(&self) -> RiskLevel {
        RiskLevel::Safe
    }

    async fn execute(&self, args: Value) -> AppResult<ToolResult> {
        let task_id = args["task_id"]
            .as_str()
            .filter(|s| !s.is_empty())
            .ok_or_else(|| AppError::InvalidArgument("task_id is required".to_string()))?;

        let task = match with_tasks(|manager| manager.get(task_id))? {
            Some(task) => task,
            None => return Ok(ToolResult::err("NOT_FOUND", &format!("找不到任务 {}", task_id))),
        };

        Ok(ToolResult::ok(task_detail(&task)))
    }
}

// ============================================================
// update_task_step — 推进步骤
// ============================================================

pub struct UpdateTaskStepTool;

#[async_trait]
impl AgentTool for UpdateTaskStepTool {
    fn name(&self) -> &'static str {
        "update_task_step"
    }

    fn description(&self) -> &'static str {
        "更新计划里某一步的状态。开始做某步传 status=start；做完传 completed 并给 result（写清实际发生了什么、产物在哪）；失败传 failed 并给 error。重复标记同一个状态不会重复计数。全部步骤 completed 时任务自动转 COMPLETED。"
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "task_id": { "type": "string", "description": "任务 ID" },
                "step_id": { "type": "integer", "minimum": 1, "description": "步骤编号（从 1 开始）" },
                "status": {
                    "type": "string",
                    "enum": ["start", "completed", "failed", "skipped"],
                    "description": "目标状态"
                },
                "result": { "type": "string", "description": "完成时的实际结果（status=completed 时必填）" },
                "error": { "type": "string", "description": "失败原因（status=failed 时必填）" }
            },
            "required": ["task_id", "step_id", "status"]
        })
    }

    fn risk_level(&self) -> RiskLevel {
        RiskLevel::Low
    }

    async fn execute(&self, args: Value) -> AppResult<ToolResult> {
        let task_id = args["task_id"]
            .as_str()
            .filter(|s| !s.is_empty())
            .ok_or_else(|| AppError::InvalidArgument("task_id is required".to_string()))?;
        let step_id = args["step_id"]
            .as_u64()
            .ok_or_else(|| AppError::InvalidArgument("step_id is required".to_string()))?
            as usize;
        let status = args["status"]
            .as_str()
            .ok_or_else(|| AppError::InvalidArgument("status is required".to_string()))?;

        let mut task = with_tasks(|manager| load_task(manager, task_id))?;

        let existing = task
            .plan
            .iter()
            .find(|s| s.step_id == step_id)
            .map(|s| s.status)
            .ok_or_else(|| {
                AppError::InvalidArgument(format!(
                    "任务 {} 没有第 {} 步（共 {} 步）",
                    task_id,
                    step_id,
                    task.total_steps()
                ))
            })?;

        // 幂等：同一个结论重复上报时不动账本，只如实说明"本来就是这状态"。
        // 不加这层，`complete_step` 会把 step_id 重复塞进 completed_steps，
        // 进度百分比就会算出 120% 这种东西。
        if matches!(existing, StepStatus::Completed) && status == "completed" {
            return Ok(ToolResult::ok(json!({
                "task_id": task_id,
                "step_id": step_id,
                "changed": false,
                "status": existing.as_str(),
                "progress_percent": task.progress_percent(),
                "note": "该步骤此前已标记完成，未重复计数",
            })));
        }

        match status {
            "start" => task.start_step(step_id),
            "completed" => {
                let result = args["result"]
                    .as_str()
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .ok_or_else(|| {
                        AppError::InvalidArgument(
                            "status=completed 时必须给 result：写清实际发生了什么".to_string(),
                        )
                    })?;
                task.complete_step(step_id, result);
                if task.all_steps_completed() {
                    task.set_status(TaskStatus::Completed);
                }
            }
            "failed" => {
                let error = args["error"]
                    .as_str()
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .ok_or_else(|| {
                        AppError::InvalidArgument("status=failed 时必须给 error".to_string())
                    })?;
                task.fail_step(step_id, error);
                task.set_status(TaskStatus::Failed);
            }
            "skipped" => {
                if let Some(step) = task.plan.iter_mut().find(|s| s.step_id == step_id) {
                    step.status = StepStatus::Skipped;
                }
                task.updated_at = chrono::Local::now().to_rfc3339();
                if task.all_steps_completed() {
                    task.set_status(TaskStatus::Completed);
                }
            }
            other => {
                return Ok(ToolResult::err(
                    "INVALID_STATUS",
                    &format!("不支持的状态 {}，只接受 start / completed / failed / skipped", other),
                ))
            }
        }

        with_tasks(|manager| manager.update(&task))?;

        // 回读：确认落库的是"我改的这一版"，不是内存里的自述。
        let persisted = with_tasks(|manager| load_task(manager, task_id))?;
        let persisted_step = persisted
            .plan
            .iter()
            .find(|s| s.step_id == step_id)
            .map(|s| s.status.as_str())
            .unwrap_or("missing");

        Ok(ToolResult::ok(json!({
            "task_id": task_id,
            "step_id": step_id,
            "changed": true,
            "requested": status,
            "persisted_status": persisted_step,
            "task_status": persisted.status.as_str(),
            "progress_percent": persisted.progress_percent(),
            "completed_steps": persisted.completed_count(),
            "total_steps": persisted.total_steps(),
        })))
    }
}

// ============================================================
// cancel_agent_task — 停掉
// ============================================================

pub struct CancelAgentTaskTool;

#[async_trait]
impl AgentTool for CancelAgentTaskTool {
    fn name(&self) -> &'static str {
        "cancel_agent_task"
    }

    fn description(&self) -> &'static str {
        "取消一个任务计划。用户说「别做了」「停掉那个计划」时用。只改计划状态，不会撤回已经发生的操作。"
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "task_id": { "type": "string", "description": "任务 ID" }
            },
            "required": ["task_id"]
        })
    }

    fn risk_level(&self) -> RiskLevel {
        RiskLevel::Low
    }

    async fn execute(&self, args: Value) -> AppResult<ToolResult> {
        let task_id = args["task_id"]
            .as_str()
            .filter(|s| !s.is_empty())
            .ok_or_else(|| AppError::InvalidArgument("task_id is required".to_string()))?;

        let mut task = with_tasks(|manager| load_task(manager, task_id))?;
        if task.status.is_terminal() {
            return Ok(ToolResult::ok(json!({
                "task_id": task_id,
                "changed": false,
                "status": task.status.as_str(),
                "note": "任务已是终态，无需取消",
            })));
        }

        task.set_status(TaskStatus::Cancelled);
        with_tasks(|manager| manager.update(&task))?;

        let persisted = with_tasks(|manager| load_task(manager, task_id))?;
        Ok(ToolResult::ok(json!({
            "task_id": task_id,
            "changed": true,
            "status": persisted.status.as_str(),
        })))
    }
}

// ============================================================
// resume_task_context — 接着干
// ============================================================

pub struct ResumeTaskContextTool;

#[async_trait]
impl AgentTool for ResumeTaskContextTool {
    fn name(&self) -> &'static str {
        "resume_task_context"
    }

    fn description(&self) -> &'static str {
        "读一个任务的中断上下文：哪些步骤已经做完（含当时的结果）、下一步该做什么、之前失败过什么。中断或换会话后接着做时先调这个，不要重头再来——重跑已经生效的写操作会造成重复副作用。"
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "task_id": {
                    "type": "string",
                    "description": "任务 ID（可选；不传则取当前会话最近一个未结束的任务）"
                }
            }
        })
    }

    fn risk_level(&self) -> RiskLevel {
        RiskLevel::Safe
    }

    async fn execute(&self, args: Value) -> AppResult<ToolResult> {
        let explicit_id = args["task_id"].as_str().filter(|s| !s.is_empty());

        let task = match explicit_id {
            Some(id) => match with_tasks(|manager| manager.get(id))? {
                Some(task) => task,
                None => return Ok(ToolResult::err("NOT_FOUND", &format!("找不到任务 {}", id))),
            },
            None => {
                let session_id = crate::tools::current_session();
                let found = with_tasks(|manager| manager.get_active_by_session(&session_id))?;
                match found {
                    Some(task) => task,
                    None => {
                        return Ok(ToolResult::ok(json!({
                            "found": false,
                            "note": "当前会话没有未结束的任务，可以从头开始",
                        })))
                    }
                }
            }
        };

        let done: Vec<Value> = task
            .plan
            .iter()
            .filter(|s| matches!(s.status, StepStatus::Completed | StepStatus::Skipped))
            .map(|s| {
                json!({
                    "step_id": s.step_id,
                    "action": s.action,
                    "status": s.status.as_str(),
                    "result": s.result,
                })
            })
            .collect();

        let remaining: Vec<Value> = task
            .plan
            .iter()
            .filter(|s| matches!(s.status, StepStatus::Pending | StepStatus::Executing | StepStatus::Failed))
            .map(|s| {
                json!({
                    "step_id": s.step_id,
                    "action": s.action,
                    "status": s.status.as_str(),
                    "tool_name": s.tool_name,
                    "recovery_attempts": s.recovery_attempts,
                })
            })
            .collect();

        let next = task.next_pending_step().map(|s| {
            json!({ "step_id": s.step_id, "action": s.action, "tool_name": s.tool_name })
        });

        let last_errors: Vec<Value> = task
            .errors
            .iter()
            .rev()
            .take(3)
            .map(|e| json!({ "step_id": e.step_id, "error": e.error }))
            .collect();

        Ok(ToolResult::ok(json!({
            "found": true,
            "task_id": task.id,
            "goal": task.goal,
            "status": task.status.as_str(),
            "progress_percent": task.progress_percent(),
            "done_steps": done,
            "remaining_steps": remaining,
            "next_step": next,
            "recent_errors": last_errors,
            "guidance": if remaining.is_empty() {
                "所有步骤都已处理，任务可以收尾"
            } else {
                "已完成的步骤不要重做；从 next_step 继续"
            },
        })))
    }
}

// ============================================================
// 测试
// ============================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_steps_accepts_both_shapes_and_skips_blank() {
        let raw = vec![
            json!("备份原配置"),
            json!({ "action": "改端口", "tool_name": "write_file" }),
            json!("   "),
            json!("重启服务"),
        ];
        let steps = parse_steps(&raw).expect("解析步骤");
        // 空串截断：宁可少一步，也不要建一个动作为空的步骤
        assert_eq!(steps.len(), 3);
        assert_eq!(steps[0].step_id, 1);
        assert_eq!(steps[1].tool_name.as_deref(), Some("write_file"));
        // step_id 连续，中间空串不会留下空洞
        assert_eq!(steps[2].step_id, 3);
        assert_eq!(steps[2].action, "重启服务");
    }

    #[test]
    fn parse_steps_rejects_object_without_action() {
        let raw = vec![json!({ "tool_name": "write_file" })];
        assert!(parse_steps(&raw).is_err(), "缺 action 必须报错，不能静默跳过");
    }

    /// 端到端：建计划 → 推进步骤 → 断点续做 → 取消。
    ///
    /// 全局 DB 路径是 OnceLock，只能设一次，所以整个生命周期压在一个测试里；
    /// 拆成多个测试会互相抢那个槽位。
    #[tokio::test]
    async fn task_lifecycle_persists_verifies_and_resumes() {
        let dir = std::env::temp_dir().join(format!("pcg-tasks-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("建临时目录");
        let db_path = dir.join("tasks.db");

        let db = crate::db::Database::new(&db_path).expect("打开数据库");
        db.init_schema().expect("初始化 schema");
        drop(db);

        crate::memory::set_db_path(db_path);
        crate::tools::set_current_session("sess-lifecycle");

        // --- 1. 建计划 ---
        let created = CreateTaskPlanTool
            .execute(json!({
                "goal": "把服务端口从 8080 改成 9090",
                "steps": ["备份配置文件", "改端口", "重启并确认监听"]
            }))
            .await
            .expect("create_task_plan");
        assert!(created.success, "{:?}", created.error);
        let data = created.data.expect("有数据");
        let task_id = data["task_id"].as_str().expect("task_id").to_string();
        assert_eq!(data["step_count"], 3);
        assert_eq!(data["persisted"], true, "建完必须回读确认");

        // --- 2. 推进第一步 ---
        let step1 = UpdateTaskStepTool
            .execute(json!({
                "task_id": task_id,
                "step_id": 1,
                "status": "completed",
                "result": "备份到 config.bak"
            }))
            .await
            .expect("update step 1");
        assert!(step1.success, "{:?}", step1.error);
        let d1 = step1.data.expect("有数据");
        assert_eq!(d1["persisted_status"], "completed", "落库状态必须是 completed");
        assert_eq!(d1["progress_percent"], 33);

        // 重复上报同一步：不能把 completed_steps 塞两遍（33% 变 66% 是假的）
        let again = UpdateTaskStepTool
            .execute(json!({
                "task_id": task_id,
                "step_id": 1,
                "status": "completed",
                "result": "又报了一次"
            }))
            .await
            .expect("重复上报");
        assert!(again.success);
        let d_again = again.data.expect("有数据");
        assert_eq!(d_again["changed"], false);
        assert_eq!(d_again["progress_percent"], 33, "重复上报不能抬进度");

        // --- 3. 断点续做：不传 task_id 时按会话取未结束任务 ---
        let resume = ResumeTaskContextTool
            .execute(json!({}))
            .await
            .expect("resume_task_context");
        assert!(resume.success, "{:?}", resume.error);
        let r = resume.data.expect("有数据");
        assert_eq!(r["found"], true);
        assert_eq!(r["task_id"], task_id);
        assert_eq!(r["done_steps"].as_array().map(|a| a.len()), Some(1));
        assert_eq!(r["remaining_steps"].as_array().map(|a| a.len()), Some(2));
        assert_eq!(r["next_step"]["step_id"], 2, "下一步必须是 2");

        // --- 4. completed 缺 result 必须被拒 ---
        let bad = UpdateTaskStepTool
            .execute(json!({
                "task_id": task_id,
                "step_id": 2,
                "status": "completed"
            }))
            .await;
        assert!(bad.is_err(), "completed 不给 result 应该报参数错误");

        // --- 5. 取消 ---
        let cancelled = CancelAgentTaskTool
            .execute(json!({ "task_id": task_id }))
            .await
            .expect("cancel");
        let c = cancelled.data.expect("有数据");
        assert_eq!(c["status"], "CANCELLED");

        // 终态再取消：如实说没变，不假装又取消了一次
        let twice = CancelAgentTaskTool
            .execute(json!({ "task_id": task_id }))
            .await
            .expect("cancel twice");
        assert_eq!(twice.data.expect("有数据")["changed"], false);

        // --- 6. 列表能查到 ---
        let listed = ListAgentTasksTool
            .execute(json!({ "limit": 5 }))
            .await
            .expect("list");
        let l = listed.data.expect("有数据");
        assert_eq!(l["count"], 1);
        assert_eq!(l["tasks"][0]["task_id"], task_id);

        let _ = std::fs::remove_dir_all(&dir);
    }
}
