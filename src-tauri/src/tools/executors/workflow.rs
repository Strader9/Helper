//! 工作流工具（workflow 域）
//!
//! 14 个工具，全部是 `crate::workflow` 引擎的薄壳：定义管理、校验、
//! 运行、历史。引擎逻辑（DAG 校验、变量解析、逐步执行）在那边，
//! 这里只负责参数解析与结果组装。
//!
//! 为什么运行要同步阻塞：工作流的价值在于"跑完给我一个确定的结果"。
//! 异步返回"已启动"对用户没有信息量——他还得再问一次跑得怎么样。
//! 步骤有超时上限，不会无限挂着。

use async_trait::async_trait;
use serde_json::{json, Value};

use crate::error::{AppError, AppResult};
use crate::tools::{AgentTool, RiskLevel, ToolResult};
use crate::workflow;

fn require_str<'a>(args: &'a Value, key: &str) -> AppResult<&'a str> {
    args[key]
        .as_str()
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| AppError::InvalidArgument(format!("{} is required", key)))
}

/// 借一次数据库连接
fn conn() -> AppResult<rusqlite::Connection> {
    crate::memory::open_connection().ok_or_else(|| AppError::Internal("数据库未初始化".to_string()))
}

fn now_text() -> String {
    chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string()
}

/// 确认工作流定义存在，顺便把名字带出来（错误信息里要用）
fn require_definition(id_or_name: &str) -> AppResult<workflow::Definition> {
    // 查库失败（数据库未初始化等）也按"找不到"处理：require_definition 的
    // 语义是"给调用方一个可操作的下一步"，而不是把内部数据库错误抛给模型。
    if let Ok(Some(def)) = workflow::load_definition(id_or_name) {
        return Ok(def);
    }
    if let Ok(Some(def)) = workflow::find_by_name(id_or_name) {
        return Ok(def);
    }
    Err(AppError::InvalidArgument(format!(
        "找不到工作流「{}」（可先调用 workflow_list 查看已有流程）",
        id_or_name
    )))
}

// ============================================================
// workflow_create
// ============================================================

pub struct WorkflowCreateTool;

#[async_trait]
impl AgentTool for WorkflowCreateTool {
    fn name(&self) -> &'static str {
        "workflow_create"
    }

    fn description(&self) -> &'static str {
        "定义一个可重复执行的工作流（步骤与依赖写死，每次跑的顺序都一样）。steps 是步骤数组，每步必须给 id 与 kind：kind=tool 时给 tool 与 arguments；kind=delay 给 delay_ms；kind=notify 给 message；kind=set_var 给 var_name/var_value。步骤之间用 depends_on 指定依赖，可用 {{vars.x}} 与 {{steps.<id>.data.<字段>}} 在参数里引用变量和前序结果。"
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "name": { "type": "string", "description": "工作流名称（唯一）" },
                "description": { "type": "string", "description": "用途说明" },
                "steps": {
                    "type": "array",
                    "description": "步骤数组",
                    "items": {
                        "type": "object",
                        "properties": {
                            "id": { "type": "string", "description": "步骤标识（字母/数字/下划线/连字符）" },
                            "name": { "type": "string", "description": "人读名称" },
                            "kind": { "type": "string", "description": "tool / delay / notify / set_var，默认 tool" },
                            "tool": { "type": "string", "description": "kind=tool 时要调用的工具名" },
                            "arguments": { "type": "object", "description": "工具参数（支持 {{...}} 占位）" },
                            "depends_on": { "type": "array", "items": { "type": "string" }, "description": "依赖的步骤 id" },
                            "continue_on_error": { "type": "boolean", "description": "本步失败后是否继续，默认 false" },
                            "timeout_sec": { "type": "integer", "description": "单步超时秒数，默认 180" },
                            "delay_ms": { "type": "integer", "description": "kind=delay 的等待毫秒数" },
                            "title": { "type": "string", "description": "kind=notify 的标题" },
                            "message": { "type": "string", "description": "kind=notify 的正文" },
                            "var_name": { "type": "string", "description": "kind=set_var 的变量名" },
                            "var_value": { "description": "kind=set_var 的变量值" }
                        },
                        "required": ["id"]
                    }
                },
                "variables": { "type": "object", "description": "默认变量表，运行时可覆盖" }
            },
            "required": ["name", "steps"]
        })
    }

    fn risk_level(&self) -> RiskLevel {
        RiskLevel::Medium
    }

    async fn execute(&self, args: Value) -> AppResult<ToolResult> {
        let name = require_str(&args, "name")?.trim().to_string();
        let steps = workflow::parse_steps(&args["steps"])?;
        let problems = workflow::validate(&steps);
        if !problems.is_empty() {
            let detail = problems
                .iter()
                .map(|p| format!("[{}] {}", p.step, p.message))
                .collect::<Vec<_>>()
                .join("；");
            return Ok(ToolResult::err(
                "WORKFLOW_INVALID",
                &format!("工作流定义有问题: {}", detail),
            ));
        }

        let c = conn()?;
        let exists: Option<String> = c
            .query_row(
                "SELECT id FROM workflow_defs WHERE name = ?1",
                [&name],
                |r| r.get(0),
            )
            .ok();
        if exists.is_some() {
            return Ok(ToolResult::err(
                "WORKFLOW_EXISTS",
                &format!("已有同名工作流「{}」，改名或改用 workflow_update", name),
            ));
        }

        let id = uuid::Uuid::new_v4().to_string();
        let steps_json = serde_json::to_string(&steps)?;
        let vars = args.get("variables").cloned().unwrap_or(json!({}));
        let description = args["description"].as_str().unwrap_or("");
        c.execute(
            "INSERT INTO workflow_defs (id, name, description, steps_json, variables_json, enabled, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, 1, ?6, ?6)",
            rusqlite::params![id, name, description, steps_json, serde_json::to_string(&vars)?, now_text()],
        )?;

        // 回读确认：写库成功 ≠ 能读出来（steps_json 序列化错了照样写进去）
        let verify = workflow::load_definition(&id)?;
        let verified = verify.map(|d| d.steps.len()).unwrap_or(0);

        Ok(ToolResult::ok(json!({
            "id": id,
            "name": name,
            "step_count": steps.len(),
            "verified_step_count": verified,
            "step_ids": steps.iter().map(|s| s.id.clone()).collect::<Vec<_>>(),
        })))
    }
}

// ============================================================
// workflow_list
// ============================================================

pub struct WorkflowListTool;

#[async_trait]
impl AgentTool for WorkflowListTool {
    fn name(&self) -> &'static str {
        "workflow_list"
    }

    fn description(&self) -> &'static str {
        "列出已定义的工作流（名称、步骤数、运行次数、上次结果）。想看某一条的具体步骤用 workflow_get。"
    }

    fn parameters(&self) -> Value {
        json!({ "type": "object", "properties": {} })
    }

    fn risk_level(&self) -> RiskLevel {
        RiskLevel::Safe
    }

    async fn execute(&self, _args: Value) -> AppResult<ToolResult> {
        let c = conn()?;
        let mut stmt = c.prepare(
            "SELECT id, name, description, steps_json, enabled, run_count, last_run_at, last_status
             FROM workflow_defs ORDER BY updated_at DESC",
        )?;
        let rows = stmt.query_map([], |row| {
            let steps_json: String = row.get(3)?;
            let steps: Value = serde_json::from_str(&steps_json).unwrap_or(json!([]));
            Ok(json!({
                "id": row.get::<_, String>(0)?,
                "name": row.get::<_, String>(1)?,
                "description": row.get::<_, String>(2)?,
                "step_count": steps.as_array().map(|a| a.len()).unwrap_or(0),
                "enabled": row.get::<_, i64>(4)? != 0,
                "run_count": row.get::<_, i64>(5)?,
                "last_run_at": row.get::<_, Option<String>>(6)?,
                "last_status": row.get::<_, Option<String>>(7)?,
            }))
        })?;
        let list: Vec<Value> = rows.filter_map(|r| r.ok()).collect();
        Ok(ToolResult::ok(json!({
            "count": list.len(),
            "workflows": list,
        })))
    }
}

// ============================================================
// workflow_get
// ============================================================

pub struct WorkflowGetTool;

#[async_trait]
impl AgentTool for WorkflowGetTool {
    fn name(&self) -> &'static str {
        "workflow_get"
    }

    fn description(&self) -> &'static str {
        "查看某个工作流的完整定义（步骤、依赖、变量）。id 可以给 UUID 或工作流名称。"
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "workflow": { "type": "string", "description": "工作流 id 或名称" }
            },
            "required": ["workflow"]
        })
    }

    fn risk_level(&self) -> RiskLevel {
        RiskLevel::Safe
    }

    async fn execute(&self, args: Value) -> AppResult<ToolResult> {
        let key = require_str(&args, "workflow")?;
        let def = require_definition(key)?;
        Ok(ToolResult::ok(json!({
            "id": def.id,
            "name": def.name,
            "description": def.description,
            "enabled": def.enabled,
            "run_count": def.run_count,
            "variables": def.variables,
            "step_count": def.steps.len(),
            "steps": def.steps,
        })))
    }
}

// ============================================================
// workflow_update
// ============================================================

pub struct WorkflowUpdateTool;

#[async_trait]
impl AgentTool for WorkflowUpdateTool {
    fn name(&self) -> &'static str {
        "workflow_update"
    }

    fn description(&self) -> &'static str {
        "修改已有工作流。steps 是整体替换（不是合并），只改名字或说明时可以不给 steps。历史运行记录不会被改写。"
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "workflow": { "type": "string", "description": "工作流 id 或名称" },
                "name": { "type": "string", "description": "新名称" },
                "description": { "type": "string", "description": "新说明" },
                "steps": { "type": "array", "description": "新的步骤数组（整体替换）" },
                "variables": { "type": "object", "description": "新的默认变量表" }
            },
            "required": ["workflow"]
        })
    }

    fn risk_level(&self) -> RiskLevel {
        RiskLevel::Medium
    }

    async fn execute(&self, args: Value) -> AppResult<ToolResult> {
        let key = require_str(&args, "workflow")?;
        let def = require_definition(key)?;

        let new_name = args["name"].as_str().filter(|s| !s.trim().is_empty());
        let new_steps = match args.get("steps") {
            Some(v) if !v.is_null() => {
                let steps = workflow::parse_steps(v)?;
                let problems = workflow::validate(&steps);
                if !problems.is_empty() {
                    let detail = problems
                        .iter()
                        .map(|p| format!("[{}] {}", p.step, p.message))
                        .collect::<Vec<_>>()
                        .join("；");
                    return Ok(ToolResult::err(
                        "WORKFLOW_INVALID",
                        &format!("新定义有问题: {}", detail),
                    ));
                }
                Some(steps)
            }
            _ => None,
        };

        let c = conn()?;
        // 改名撞车要提前挡：UNIQUE 约束报出来的错误信息对模型不友好
        if let Some(name) = new_name {
            if name != def.name {
                let taken: Option<String> = c
                    .query_row(
                        "SELECT id FROM workflow_defs WHERE name = ?1 AND id != ?2",
                        rusqlite::params![name, def.id],
                        |r| r.get(0),
                    )
                    .ok();
                if taken.is_some() {
                    return Ok(ToolResult::err(
                        "WORKFLOW_EXISTS",
                        &format!("已有同名工作流「{}」", name),
                    ));
                }
            }
        }

        let steps_json = match &new_steps {
            Some(steps) => serde_json::to_string(steps)?,
            None => serde_json::to_string(&def.steps)?,
        };
        let variables_json = match args.get("variables") {
            Some(v) if !v.is_null() => serde_json::to_string(v)?,
            _ => serde_json::to_string(&def.variables)?,
        };

        c.execute(
            "UPDATE workflow_defs SET name = ?1, description = ?2, steps_json = ?3, variables_json = ?4, updated_at = ?5
             WHERE id = ?6",
            rusqlite::params![
                new_name.unwrap_or(&def.name),
                args["description"].as_str().unwrap_or(&def.description),
                steps_json,
                variables_json,
                now_text(),
                def.id
            ],
        )?;

        let after = workflow::load_definition(&def.id)?;
        Ok(ToolResult::ok(json!({
            "id": def.id,
            "name": after.as_ref().map(|d| d.name.clone()).unwrap_or_default(),
            "step_count": after.as_ref().map(|d| d.steps.len()).unwrap_or(0),
            "steps_replaced": new_steps.is_some(),
        })))
    }
}

// ============================================================
// workflow_delete
// ============================================================

pub struct WorkflowDeleteTool;

#[async_trait]
impl AgentTool for WorkflowDeleteTool {
    fn name(&self) -> &'static str {
        "workflow_delete"
    }

    fn description(&self) -> &'static str {
        "删除一个工作流定义，连带它的运行历史一起删（外键级联）。只在用户明确要求删除时使用。"
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "workflow": { "type": "string", "description": "工作流 id 或名称" },
                "confirm": { "type": "boolean", "description": "必须显式传 true 才会真正删除" }
            },
            "required": ["workflow", "confirm"]
        })
    }

    fn risk_level(&self) -> RiskLevel {
        // 删定义连带删历史，不可撤销
        RiskLevel::High
    }

    async fn execute(&self, args: Value) -> AppResult<ToolResult> {
        let key = require_str(&args, "workflow")?;
        if !args["confirm"].as_bool().unwrap_or(false) {
            return Err(AppError::InvalidArgument(
                "删除需要 confirm=true；这会同时删掉该工作流的全部运行历史".to_string(),
            ));
        }
        let def = require_definition(key)?;
        let c = conn()?;
        let runs: i64 = c
            .query_row(
                "SELECT COUNT(*) FROM workflow_runs WHERE workflow_id = ?1",
                [&def.id],
                |r| r.get(0),
            )
            .unwrap_or(0);
        c.execute("DELETE FROM workflow_steps WHERE run_id IN (SELECT id FROM workflow_runs WHERE workflow_id = ?1)", [&def.id])?;
        c.execute(
            "DELETE FROM workflow_runs WHERE workflow_id = ?1",
            [&def.id],
        )?;
        c.execute("DELETE FROM workflow_defs WHERE id = ?1", [&def.id])?;

        // 回读：删干净了才算成功
        let still = workflow::load_definition(&def.id)?;
        Ok(ToolResult::ok(json!({
            "deleted": still.is_none(),
            "name": def.name,
            "runs_deleted": runs,
        })))
    }
}

// ============================================================
// workflow_validate
// ============================================================

pub struct WorkflowValidateTool;

#[async_trait]
impl AgentTool for WorkflowValidateTool {
    fn name(&self) -> &'static str {
        "workflow_validate"
    }

    fn description(&self) -> &'static str {
        "校验一份工作流定义（可以不保存就检查）：id 是否合法/重复、依赖是否存在、是否有环、工具名是否存在。发现问题会一次列全。"
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "steps": { "type": "array", "description": "要检查的步骤数组" },
                "workflow": { "type": "string", "description": "也可以给已保存的工作流 id/名称来检查" }
            }
        })
    }

    fn risk_level(&self) -> RiskLevel {
        RiskLevel::Safe
    }

    async fn execute(&self, args: Value) -> AppResult<ToolResult> {
        let steps = if let Some(v) = args.get("steps") {
            workflow::parse_steps(v)?
        } else if let Some(key) = args["workflow"].as_str().filter(|s| !s.trim().is_empty()) {
            require_definition(key)?.steps
        } else {
            return Err(AppError::InvalidArgument(
                "steps 或 workflow 至少给一个".to_string(),
            ));
        };

        let mut problems: Vec<String> = workflow::validate(&steps)
            .into_iter()
            .map(|p| format!("[{}] {}", p.step, p.message))
            .collect();

        // 工具是否存在：引擎到运行期才报"工具不存在"太晚了，
        // 定义阶段就该拦下来（模型可能照着记忆里的名字瞎写）。
        let registry = crate::tools::ToolRegistry::default();
        for step in &steps {
            if step.kind == "tool" {
                if let Some(tool) = step.tool.as_deref() {
                    if !tool.trim().is_empty() && registry.get(tool.trim()).is_none() {
                        problems.push(format!(
                            "[{}] 工具「{}」不存在（可先查 capability_manifest 看有哪些工具）",
                            step.id, tool
                        ));
                    }
                }
            }
        }

        let order = workflow::topo_order(&steps).ok();
        Ok(ToolResult::ok(json!({
            "valid": problems.is_empty(),
            "step_count": steps.len(),
            "problems": problems,
            "execution_order": order.map(|o| o.iter().map(|i| steps[*i].id.clone()).collect::<Vec<_>>()),
        })))
    }
}

// ============================================================
// workflow_run
// ============================================================

pub struct WorkflowRunTool;

#[async_trait]
impl AgentTool for WorkflowRunTool {
    fn name(&self) -> &'static str {
        "workflow_run"
    }

    fn description(&self) -> &'static str {
        "立即执行一个工作流，同步等它跑完并返回每一步的结果。variables 可以覆盖定义里的默认变量。步骤里需要确认的工具会走正常权限确认；在无人值守（定时任务）场景下这类步骤会被拒绝而不是挂起等待。"
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "workflow": { "type": "string", "description": "工作流 id 或名称" },
                "variables": { "type": "object", "description": "本次运行覆盖的变量" }
            },
            "required": ["workflow"]
        })
    }

    fn risk_level(&self) -> RiskLevel {
        // 跑什么由定义决定，最坏情形会落到写文件/执行命令上
        RiskLevel::High
    }

    async fn execute(&self, args: Value) -> AppResult<ToolResult> {
        let key = require_str(&args, "workflow")?;
        let def = require_definition(key)?;
        if !def.enabled {
            return Ok(ToolResult::err(
                "WORKFLOW_DISABLED",
                &format!("工作流「{}」已停用，先启用再运行", def.name),
            ));
        }

        let session = crate::tools::current_session();
        let vars = args.get("variables").cloned().unwrap_or(json!({}));
        let outcome = workflow::run_definition(&def, &session, "manual", vars).await?;

        Ok(ToolResult::ok(json!({
            "workflow": def.name,
            "run_id": outcome.run_id,
            "status": outcome.status,
            "ok_count": outcome.ok_count,
            "fail_count": outcome.fail_count,
            "summary": outcome.summary,
            "steps": outcome.steps,
        })))
    }
}

// ============================================================
// workflow_runs
// ============================================================

pub struct WorkflowRunsTool;

#[async_trait]
impl AgentTool for WorkflowRunsTool {
    fn name(&self) -> &'static str {
        "workflow_runs"
    }

    fn description(&self) -> &'static str {
        "查工作流的运行历史（最近几次、状态、成功/失败步数）。想知道某次具体每步干了什么用 workflow_run_detail。"
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "workflow": { "type": "string", "description": "工作流 id 或名称；省略则列全部工作流的运行" },
                "limit": { "type": "integer", "description": "返回条数，默认 20" }
            }
        })
    }

    fn risk_level(&self) -> RiskLevel {
        RiskLevel::Safe
    }

    async fn execute(&self, args: Value) -> AppResult<ToolResult> {
        let limit = args["limit"].as_u64().unwrap_or(20).min(200) as i64;
        let c = conn()?;
        let filter_id = match args["workflow"].as_str().filter(|s| !s.trim().is_empty()) {
            Some(key) => Some(require_definition(key)?.id),
            None => None,
        };

        let (sql, params): (&str, Vec<Box<dyn rusqlite::ToSql>>) = match &filter_id {
            Some(id) => (
                "SELECT id, workflow_id, workflow_name, trigger_kind, status, started_at, finished_at,
                        step_count, ok_count, fail_count, summary, error
                 FROM workflow_runs WHERE workflow_id = ?1 ORDER BY started_at DESC LIMIT ?2",
                vec![Box::new(id.clone()), Box::new(limit)],
            ),
            None => (
                "SELECT id, workflow_id, workflow_name, trigger_kind, status, started_at, finished_at,
                        step_count, ok_count, fail_count, summary, error
                 FROM workflow_runs ORDER BY started_at DESC LIMIT ?1",
                vec![Box::new(limit)],
            ),
        };
        let mut stmt = c.prepare(sql)?;
        let refs: Vec<&dyn rusqlite::ToSql> = params.iter().map(|b| b.as_ref()).collect();
        let rows = stmt.query_map(refs.as_slice(), |row| {
            Ok(json!({
                "run_id": row.get::<_, String>(0)?,
                "workflow_id": row.get::<_, String>(1)?,
                "workflow_name": row.get::<_, String>(2)?,
                "trigger": row.get::<_, String>(3)?,
                "status": row.get::<_, String>(4)?,
                "started_at": row.get::<_, String>(5)?,
                "finished_at": row.get::<_, Option<String>>(6)?,
                "step_count": row.get::<_, i64>(7)?,
                "ok_count": row.get::<_, i64>(8)?,
                "fail_count": row.get::<_, i64>(9)?,
                "summary": row.get::<_, Option<String>>(10)?,
                "error": row.get::<_, Option<String>>(11)?,
            }))
        })?;
        let list: Vec<Value> = rows.filter_map(|r| r.ok()).collect();
        Ok(ToolResult::ok(json!({
            "count": list.len(),
            "runs": list,
        })))
    }
}

// ============================================================
// workflow_run_detail
// ============================================================

pub struct WorkflowRunDetailTool;

#[async_trait]
impl AgentTool for WorkflowRunDetailTool {
    fn name(&self) -> &'static str {
        "workflow_run_detail"
    }

    fn description(&self) -> &'static str {
        "查某一次运行的逐步明细：每步是什么、成功还是失败、返回了什么、错误是什么。排查「上次为什么没跑通」用这个。"
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "run_id": { "type": "string", "description": "运行 id（从 workflow_runs 拿）" }
            },
            "required": ["run_id"]
        })
    }

    fn risk_level(&self) -> RiskLevel {
        RiskLevel::Safe
    }

    async fn execute(&self, args: Value) -> AppResult<ToolResult> {
        let run_id = require_str(&args, "run_id")?;
        let c = conn()?;
        let run = c
            .query_row(
                "SELECT workflow_name, trigger_kind, status, started_at, finished_at, step_count, ok_count, fail_count, summary, error
                 FROM workflow_runs WHERE id = ?1",
                [run_id],
                |row| {
                    Ok(json!({
                        "workflow_name": row.get::<_, String>(0)?,
                        "trigger": row.get::<_, String>(1)?,
                        "status": row.get::<_, String>(2)?,
                        "started_at": row.get::<_, String>(3)?,
                        "finished_at": row.get::<_, Option<String>>(4)?,
                        "step_count": row.get::<_, i64>(5)?,
                        "ok_count": row.get::<_, i64>(6)?,
                        "fail_count": row.get::<_, i64>(7)?,
                        "summary": row.get::<_, Option<String>>(8)?,
                        "error": row.get::<_, Option<String>>(9)?,
                    }))
                },
            )
            .map_err(|_| AppError::InvalidArgument(format!("找不到运行记录 {}", run_id)))?;

        let mut stmt = c.prepare(
            "SELECT step_id, name, kind, seq, status, started_at, finished_at, output_json, error
             FROM workflow_steps WHERE run_id = ?1 ORDER BY seq ASC",
        )?;
        let rows = stmt.query_map([run_id], |row| {
            let output_json: Option<String> = row.get(7)?;
            Ok(json!({
                "step": row.get::<_, String>(0)?,
                "name": row.get::<_, String>(1)?,
                "kind": row.get::<_, String>(2)?,
                "seq": row.get::<_, i64>(3)?,
                "status": row.get::<_, String>(4)?,
                "started_at": row.get::<_, Option<String>>(5)?,
                "finished_at": row.get::<_, Option<String>>(6)?,
                "output": output_json.and_then(|s| serde_json::from_str::<Value>(&s).ok()),
                "error": row.get::<_, Option<String>>(8)?,
            }))
        })?;
        let steps: Vec<Value> = rows.filter_map(|r| r.ok()).collect();

        Ok(ToolResult::ok(json!({
            "run_id": run_id,
            "run": run,
            "steps": steps,
        })))
    }
}

// ============================================================
// workflow_set_enabled
// ============================================================

pub struct WorkflowSetEnabledTool;

#[async_trait]
impl AgentTool for WorkflowSetEnabledTool {
    fn name(&self) -> &'static str {
        "workflow_set_enabled"
    }

    fn description(&self) -> &'static str {
        "启用或停用一个工作流。停用后 workflow_run 会拒绝执行（定义与历史都保留）。"
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "workflow": { "type": "string", "description": "工作流 id 或名称" },
                "enabled": { "type": "boolean", "description": "true 启用 / false 停用" }
            },
            "required": ["workflow", "enabled"]
        })
    }

    fn risk_level(&self) -> RiskLevel {
        RiskLevel::Low
    }

    async fn execute(&self, args: Value) -> AppResult<ToolResult> {
        let key = require_str(&args, "workflow")?;
        let enabled = args["enabled"]
            .as_bool()
            .ok_or_else(|| AppError::InvalidArgument("enabled 必须是 true/false".to_string()))?;
        let def = require_definition(key)?;
        let c = conn()?;
        c.execute(
            "UPDATE workflow_defs SET enabled = ?1, updated_at = ?2 WHERE id = ?3",
            rusqlite::params![if enabled { 1 } else { 0 }, now_text(), def.id],
        )?;
        let after = workflow::load_definition(&def.id)?;
        Ok(ToolResult::ok(json!({
            "id": def.id,
            "name": def.name,
            "enabled": after.map(|d| d.enabled).unwrap_or(enabled),
        })))
    }
}

// ============================================================
// workflow_clone
// ============================================================

pub struct WorkflowCloneTool;

#[async_trait]
impl AgentTool for WorkflowCloneTool {
    fn name(&self) -> &'static str {
        "workflow_clone"
    }

    fn description(&self) -> &'static str {
        "复制一个工作流为新的一份（步骤、依赖、变量都照抄，运行历史不带过去）。想改一份而不动原来那个时用它。"
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "workflow": { "type": "string", "description": "源工作流 id 或名称" },
                "name": { "type": "string", "description": "新工作流名称" }
            },
            "required": ["workflow", "name"]
        })
    }

    fn risk_level(&self) -> RiskLevel {
        RiskLevel::Medium
    }

    async fn execute(&self, args: Value) -> AppResult<ToolResult> {
        let key = require_str(&args, "workflow")?;
        let new_name = require_str(&args, "name")?.trim().to_string();
        let def = require_definition(key)?;
        let c = conn()?;
        let taken: Option<String> = c
            .query_row(
                "SELECT id FROM workflow_defs WHERE name = ?1",
                [&new_name],
                |r| r.get(0),
            )
            .ok();
        if taken.is_some() {
            return Ok(ToolResult::err(
                "WORKFLOW_EXISTS",
                &format!("已有同名工作流「{}」", new_name),
            ));
        }

        let id = uuid::Uuid::new_v4().to_string();
        c.execute(
            "INSERT INTO workflow_defs (id, name, description, steps_json, variables_json, enabled, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7)",
            rusqlite::params![
                id,
                new_name,
                format!("{}（复制自 {}）", def.description, def.name),
                serde_json::to_string(&def.steps)?,
                serde_json::to_string(&def.variables)?,
                if def.enabled { 1 } else { 0 },
                now_text()
            ],
        )?;

        Ok(ToolResult::ok(json!({
            "id": id,
            "name": new_name,
            "source": def.name,
            "step_count": def.steps.len(),
        })))
    }
}

// ============================================================
// workflow_export / workflow_import
// ============================================================

pub struct WorkflowExportTool;

#[async_trait]
impl AgentTool for WorkflowExportTool {
    fn name(&self) -> &'static str {
        "workflow_export"
    }

    fn description(&self) -> &'static str {
        "把工作流定义导出成 JSON 文件（或直接返回 JSON）。用于备份、跨机器迁移、或贴给别人。"
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "workflow": { "type": "string", "description": "工作流 id 或名称；省略则导出全部" },
                "path": { "type": "string", "description": "输出 .json 路径；省略则只在结果里返回 JSON" }
            }
        })
    }

    fn risk_level(&self) -> RiskLevel {
        RiskLevel::Low
    }

    async fn execute(&self, args: Value) -> AppResult<ToolResult> {
        let payload = match args["workflow"].as_str().filter(|s| !s.trim().is_empty()) {
            Some(key) => {
                let def = require_definition(key)?;
                json!([{
                    "name": def.name,
                    "description": def.description,
                    "variables": def.variables,
                    "steps": def.steps,
                }])
            }
            None => {
                let c = conn()?;
                let mut stmt = c.prepare(
                    "SELECT name, description, steps_json, variables_json FROM workflow_defs ORDER BY name",
                )?;
                let rows = stmt.query_map([], |row| {
                    let steps_json: String = row.get(2)?;
                    let vars_json: String = row.get(3)?;
                    Ok(json!({
                        "name": row.get::<_, String>(0)?,
                        "description": row.get::<_, String>(1)?,
                        "steps": serde_json::from_str::<Value>(&steps_json).unwrap_or(json!([])),
                        "variables": serde_json::from_str::<Value>(&vars_json).unwrap_or(json!({})),
                    }))
                })?;
                Value::Array(rows.filter_map(|r| r.ok()).collect())
            }
        };

        let count = payload.as_array().map(|a| a.len()).unwrap_or(0);
        let text = serde_json::to_string_pretty(&payload)?;

        if let Some(path) = args["path"].as_str().filter(|s| !s.trim().is_empty()) {
            if !path.to_lowercase().ends_with(".json") {
                return Err(AppError::InvalidArgument(
                    "导出路径应以 .json 结尾".to_string(),
                ));
            }
            if let Some(parent) = std::path::Path::new(path).parent() {
                std::fs::create_dir_all(parent).ok();
            }
            std::fs::write(path, &text)?;
            let bytes = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
            return Ok(ToolResult::ok(json!({
                "path": path,
                "count": count,
                "bytes": bytes,
            })));
        }

        Ok(ToolResult::ok(json!({
            "count": count,
            "workflows": payload,
        })))
    }
}

pub struct WorkflowImportTool;

#[async_trait]
impl AgentTool for WorkflowImportTool {
    fn name(&self) -> &'static str {
        "workflow_import"
    }

    fn description(&self) -> &'static str {
        "从 JSON 文件（或直接给的 JSON）导入工作流定义。可以一次导入多个。重名时默认跳过，overwrite=true 则覆盖同名定义。"
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "要导入的 .json 文件路径" },
                "data": { "description": "也可以直接给 JSON 数组（与 path 二选一）" },
                "overwrite": { "type": "boolean", "description": "同名时是否覆盖，默认 false" }
            }
        })
    }

    fn risk_level(&self) -> RiskLevel {
        RiskLevel::Medium
    }

    async fn execute(&self, args: Value) -> AppResult<ToolResult> {
        let payload: Value =
            if let Some(path) = args["path"].as_str().filter(|s| !s.trim().is_empty()) {
                let text = std::fs::read_to_string(path)
                    .map_err(|e| AppError::InvalidArgument(format!("读取导入文件失败: {}", e)))?;
                serde_json::from_str(&text)
                    .map_err(|e| AppError::InvalidArgument(format!("JSON 解析失败: {}", e)))?
            } else if let Some(data) = args.get("data") {
                data.clone()
            } else {
                return Err(AppError::InvalidArgument(
                    "path 或 data 至少给一个".to_string(),
                ));
            };

        let items: Vec<Value> = match payload {
            Value::Array(arr) => arr,
            Value::Object(_) => vec![payload],
            _ => {
                return Err(AppError::InvalidArgument(
                    "导入内容必须是对象或对象数组".to_string(),
                ))
            }
        };
        if items.is_empty() {
            return Err(AppError::InvalidArgument("导入内容为空".to_string()));
        }

        let overwrite = args["overwrite"].as_bool().unwrap_or(false);
        let mut imported: Vec<String> = Vec::new();
        let mut skipped: Vec<String> = Vec::new();
        let mut failed: Vec<Value> = Vec::new();

        for item in &items {
            let name = item["name"].as_str().unwrap_or("").trim().to_string();
            if name.is_empty() {
                failed.push(json!({ "reason": "缺少 name" }));
                continue;
            }
            let steps = match workflow::parse_steps(&item["steps"]) {
                Ok(s) => s,
                Err(e) => {
                    failed.push(json!({ "name": name, "reason": e.to_string() }));
                    continue;
                }
            };
            let problems = workflow::validate(&steps);
            if !problems.is_empty() {
                failed.push(json!({
                    "name": name,
                    "reason": problems.iter().map(|p| format!("[{}] {}", p.step, p.message)).collect::<Vec<_>>().join("；"),
                }));
                continue;
            }

            let existing = workflow::find_by_name(&name)?;
            match existing {
                Some(_) if !overwrite => {
                    skipped.push(name);
                }
                Some(def) => {
                    let c = conn()?;
                    c.execute(
                        "UPDATE workflow_defs SET description = ?1, steps_json = ?2, variables_json = ?3, updated_at = ?4 WHERE id = ?5",
                        rusqlite::params![
                            item["description"].as_str().unwrap_or(""),
                            serde_json::to_string(&steps)?,
                            serde_json::to_string(item.get("variables").unwrap_or(&json!({})))?,
                            now_text(),
                            def.id
                        ],
                    )?;
                    imported.push(name);
                }
                None => {
                    let c = conn()?;
                    c.execute(
                        "INSERT INTO workflow_defs (id, name, description, steps_json, variables_json, enabled, created_at, updated_at)
                         VALUES (?1, ?2, ?3, ?4, ?5, 1, ?6, ?6)",
                        rusqlite::params![
                            uuid::Uuid::new_v4().to_string(),
                            name,
                            item["description"].as_str().unwrap_or(""),
                            serde_json::to_string(&steps)?,
                            serde_json::to_string(item.get("variables").unwrap_or(&json!({})))?,
                            now_text()
                        ],
                    )?;
                    imported.push(name);
                }
            }
        }

        Ok(ToolResult::ok(json!({
            "imported": imported,
            "imported_count": imported.len(),
            "skipped": skipped,
            "skipped_count": skipped.len(),
            "failed": failed,
            "failed_count": failed.len(),
        })))
    }
}

// ============================================================
// workflow_templates
// ============================================================

pub struct WorkflowTemplatesTool;

#[async_trait]
impl AgentTool for WorkflowTemplatesTool {
    fn name(&self) -> &'static str {
        "workflow_templates"
    }

    fn description(&self) -> &'static str {
        "列出内置的工作流模板（备份、整理下载目录、项目体检）。用户不知道工作流怎么用的时候，先给模板再改两个参数即可。"
    }

    fn parameters(&self) -> Value {
        json!({ "type": "object", "properties": {} })
    }

    fn risk_level(&self) -> RiskLevel {
        RiskLevel::Safe
    }

    async fn execute(&self, _args: Value) -> AppResult<ToolResult> {
        let list = workflow::templates();
        Ok(ToolResult::ok(json!({
            "count": list.as_array().map(|a| a.len()).unwrap_or(0),
            "templates": list,
        })))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn require_definition_says_what_to_do_next() {
        let err = require_definition("不存在的流程").unwrap_err().to_string();
        assert!(
            err.contains("workflow_list"),
            "错误信息应给出下一步: {}",
            err
        );
    }

    #[test]
    fn create_rejects_cycle_before_touching_db() {
        // 校验发生在写库之前：环定义不该在库里留下半条脏数据
        let steps = workflow::parse_steps(&json!([
            { "id": "a", "kind": "tool", "tool": "read_file", "depends_on": ["b"] },
            { "id": "b", "kind": "tool", "tool": "read_file", "depends_on": ["a"] }
        ]))
        .unwrap();
        let problems = workflow::validate(&steps);
        assert!(!problems.is_empty());
    }
}
