//! 工作接管（work_takeover）工具
//!
//! 13 个工具：任务台账（建/列/查/改/续/推进/导出）+ 能力复用探针 +
//! 证据工作流（开始/更新/查状态/提升/列）。全部是 `crate::work_takeover`
//! 的薄壳——核心逻辑（台账 CRUD、证据步骤、任务包导出）在那边。
//!
//! 设计边界（对齐 LumiCore 的 work_takeover）：这些工具**只动本地台账与
//! 本地产物**，不发消息、不发布、不提交、不付款、不签名、不操作外部软件。
//! 外部动作永远走其它工具的三层闸门，由用户确认。

use async_trait::async_trait;
use serde_json::{json, Value};

use crate::error::{AppError, AppResult};
use crate::tools::{AgentTool, RiskLevel, ToolResult};
use crate::work_takeover as wt;

fn require_str<'a>(args: &'a Value, key: &str) -> AppResult<&'a str> {
    args[key]
        .as_str()
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| AppError::InvalidArgument(format!("{} is required", key)))
}

fn str_array(args: &Value, key: &str) -> Vec<String> {
    match &args[key] {
        Value::Array(arr) => arr
            .iter()
            .filter_map(|x| x.as_str().map(|s| s.trim().to_string()))
            .filter(|s| !s.is_empty())
            .collect(),
        Value::String(s) => s
            .split(|c| c == '\n' || c == ';' || c == '；')
            .map(|x| x.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect(),
        _ => Vec::new(),
    }
}

fn task_json(t: &wt::WorkTakeoverTask) -> Value {
    json!({
        "id": t.id,
        "title": t.title,
        "category": t.category,
        "source": t.source,
        "contact": t.contact,
        "summary": t.summary,
        "urgency": t.urgency,
        "priority": t.priority,
        "status": t.status,
        "next_actions": t.next_actions,
        "current_action": wt::current_action(t),
        "current_action_index": t.current_action_index,
        "draft_reply": t.draft_reply,
        "artifacts": t.artifacts,
        "allowed_now": t.allowed_now,
        "confirmation_required": t.confirmation_required,
        "blocked_by": t.blocked_by,
        "risks": t.risks,
        "result": t.result,
        "note": t.note,
        "created_at": t.created_at,
        "updated_at": t.updated_at,
    })
}

// ============================================================
// work_takeover_task_create
// ============================================================

pub struct WorkTakeoverTaskCreateTool;

#[async_trait]
impl AgentTool for WorkTakeoverTaskCreateTool {
    fn name(&self) -> &'static str {
        "work_takeover_task_create"
    }

    fn description(&self) -> &'static str {
        "创建一条可断点续做的工作接管任务（客户/店铺/账号/案件/交付/一般工作）。把当前动作、草稿、确认边界（allowed_now/confirmation_required/blocked_by）、风险、要准备的产物持久化下来，之后可随时 continue。只写本地台账，不产生外部副作用。"
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "title": { "type": "string", "description": "任务标题" },
                "category": { "type": "string", "description": "customer/store/account/legal_case/video_publish/design_delivery/general_work/personal/unknown" },
                "source": { "type": "string", "description": "manual/chat/clipboard 等来源" },
                "source_message": { "type": "string", "description": "创建任务时的原始消息或用户原话" },
                "contact": { "type": "string", "description": "联系人/客户名" },
                "summary": { "type": "string", "description": "任务摘要" },
                "urgency": { "type": "string", "description": "low/normal/high/urgent" },
                "priority": { "type": "integer", "description": "优先级 0-100" },
                "recommended_workflow": { "type": "string", "description": "建议下一步用的工作流 id/名" },
                "next_actions": { "type": "array", "description": "有序的下一步动作列表", "items": { "type": "string" } },
                "draft_reply": { "type": "string", "description": "待发回复草稿" },
                "artifacts_to_prepare": { "type": "array", "description": "要准备的材料/文件/结果", "items": { "type": "string" } },
                "allowed_now": { "type": "array", "description": "现在就能做的动作", "items": { "type": "string" } },
                "confirmation_required": { "type": "array", "description": "必须等用户确认的动作", "items": { "type": "string" } },
                "blocked_by": { "type": "array", "description": "当前阻塞", "items": { "type": "string" } },
                "risks": { "type": "array", "description": "已知风险", "items": { "type": "string" } }
            },
            "required": ["category"]
        })
    }

    fn risk_level(&self) -> RiskLevel {
        RiskLevel::Low
    }

    async fn execute(&self, args: Value) -> AppResult<ToolResult> {
        let category = args["category"].as_str().unwrap_or("general_work").to_string();
        let artifacts = str_array(&args, "artifacts_to_prepare")
            .into_iter()
            .map(|label| wt::Artifact {
                kind: "file".to_string(),
                label,
                ..Default::default()
            })
            .collect();

        let input = wt::CreateTaskInput {
            title: args["title"].as_str().unwrap_or("").trim().to_string(),
            category,
            source: args["source"].as_str().unwrap_or("manual").to_string(),
            source_message: args["source_message"].as_str().unwrap_or("").to_string(),
            contact: args["contact"].as_str().unwrap_or("").to_string(),
            summary: args["summary"].as_str().unwrap_or("").to_string(),
            urgency: args["urgency"].as_str().unwrap_or("normal").to_string(),
            priority: args["priority"].as_i64().unwrap_or(50).clamp(0, 100),
            recommended_workflow: args["recommended_workflow"].as_str().unwrap_or("").to_string(),
            next_actions: str_array(&args, "next_actions"),
            current_action_index: 0,
            draft_reply: args["draft_reply"].as_str().unwrap_or("").to_string(),
            artifacts,
            allowed_now: str_array(&args, "allowed_now"),
            confirmation_required: str_array(&args, "confirmation_required"),
            blocked_by: str_array(&args, "blocked_by"),
            risks: str_array(&args, "risks"),
            metadata: json!({}),
        };

        let task = wt::create_task(input)?;
        Ok(ToolResult::ok(json!({
            "ok": true,
            "status": "created",
            "persisted": true,
            "task": task_json(&task),
        })))
    }
}

// ============================================================
// work_takeover_task_list
// ============================================================

pub struct WorkTakeoverTaskListTool;

#[async_trait]
impl AgentTool for WorkTakeoverTaskListTool {
    fn name(&self) -> &'static str {
        "work_takeover_task_list"
    }

    fn description(&self) -> &'static str {
        "列出工作接管任务。用户说\"继续上一个任务\"\"看看接管了哪些任务\"时用。可按状态/类别过滤。"
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "status": { "type": "string", "description": "queued/in_progress/waiting_confirmation/delivered/blocked/cancelled/active" },
                "category": { "type": "string", "description": "类别过滤" },
                "limit": { "type": "integer", "description": "最多返回多少条，默认 20" }
            }
        })
    }

    fn risk_level(&self) -> RiskLevel {
        RiskLevel::Safe
    }

    async fn execute(&self, args: Value) -> AppResult<ToolResult> {
        let status = args["status"].as_str();
        let category = args["category"].as_str();
        let limit = args["limit"].as_u64().unwrap_or(20).min(500) as usize;
        let tasks = wt::list_tasks(status, category, limit)?;
        Ok(ToolResult::ok(json!({
            "count": tasks.len(),
            "tasks": tasks.iter().map(task_json).collect::<Vec<_>>(),
        })))
    }
}

// ============================================================
// work_takeover_task_get
// ============================================================

pub struct WorkTakeoverTaskGetTool;

#[async_trait]
impl AgentTool for WorkTakeoverTaskGetTool {
    fn name(&self) -> &'static str {
        "work_takeover_task_get"
    }

    fn description(&self) -> &'static str {
        "按 id 查一条工作接管任务，含草稿、产物、动作、阻塞、确认边界。"
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": { "id": { "type": "string" } },
            "required": ["id"]
        })
    }

    fn risk_level(&self) -> RiskLevel {
        RiskLevel::Safe
    }

    async fn execute(&self, args: Value) -> AppResult<ToolResult> {
        let id = require_str(&args, "id")?;
        let task = wt::get_task(id)?
            .ok_or_else(|| AppError::InvalidArgument(format!("任务不存在: {}", id)))?;
        Ok(ToolResult::ok(json!({ "task": task_json(&task) })))
    }
}

// ============================================================
// work_takeover_task_update
// ============================================================

pub struct WorkTakeoverTaskUpdateTool;

#[async_trait]
impl AgentTool for WorkTakeoverTaskUpdateTool {
    fn name(&self) -> &'static str {
        "work_takeover_task_update"
    }

    fn description(&self) -> &'static str {
        "更新工作接管任务：状态、标题、摘要、下一步动作、草稿、产物、确认边界、阻塞、风险、结果、备注。只改本地任务状态，不产生外部副作用。"
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "id": { "type": "string" },
                "status": { "type": "string", "description": "queued/in_progress/waiting_confirmation/delivered/blocked/cancelled" },
                "title": { "type": "string" },
                "summary": { "type": "string" },
                "urgency": { "type": "string" },
                "priority": { "type": "integer" },
                "next_actions": { "type": "array", "items": { "type": "string" } },
                "current_action_index": { "type": "integer" },
                "draft_reply": { "type": "string" },
                "allowed_now": { "type": "array", "items": { "type": "string" } },
                "confirmation_required": { "type": "array", "items": { "type": "string" } },
                "blocked_by": { "type": "array", "items": { "type": "string" } },
                "risks": { "type": "array", "items": { "type": "string" } },
                "result": { "type": "string" },
                "note": { "type": "string" }
            },
            "required": ["id"]
        })
    }

    fn risk_level(&self) -> RiskLevel {
        RiskLevel::Low
    }

    async fn execute(&self, args: Value) -> AppResult<ToolResult> {
        let id = require_str(&args, "id")?.to_string();
        let patch = wt::UpdateTaskPatch {
            status: args["status"].as_str().map(|s| s.to_string()),
            title: args["title"].as_str().map(|s| s.to_string()),
            summary: args["summary"].as_str().map(|s| s.to_string()),
            urgency: args["urgency"].as_str().map(|s| s.to_string()),
            priority: args["priority"].as_i64(),
            recommended_workflow: None,
            next_actions: args["next_actions"].is_array().then(|| str_array(&args, "next_actions")),
            current_action_index: args["current_action_index"].as_i64(),
            draft_reply: args["draft_reply"].as_str().map(|s| s.to_string()),
            artifacts: None,
            allowed_now: args["allowed_now"].is_array().then(|| str_array(&args, "allowed_now")),
            confirmation_required: args["confirmation_required"].is_array().then(|| str_array(&args, "confirmation_required")),
            blocked_by: args["blocked_by"].is_array().then(|| str_array(&args, "blocked_by")),
            risks: args["risks"].is_array().then(|| str_array(&args, "risks")),
            result: args["result"].as_str().map(|s| s.to_string()),
            note: args["note"].as_str().map(|s| s.to_string()),
        };
        let task = wt::update_task(&id, patch)?
            .ok_or_else(|| AppError::InvalidArgument(format!("任务不存在: {}", id)))?;
        Ok(ToolResult::ok(json!({
            "ok": true,
            "status": "updated",
            "persisted": true,
            "task": task_json(&task),
        })))
    }
}

// ============================================================
// work_takeover_task_continue
// ============================================================

pub struct WorkTakeoverTaskContinueTool;

#[async_trait]
impl AgentTool for WorkTakeoverTaskContinueTool {
    fn name(&self) -> &'static str {
        "work_takeover_task_continue"
    }

    fn description(&self) -> &'static str {
        "继续一条工作接管任务。不传 id 时继续最高优先级的活动任务。返回当前动作、草稿、确认边界；不执行外部副作用。"
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": { "id": { "type": "string", "description": "可选，任务 id" } }
        })
    }

    fn risk_level(&self) -> RiskLevel {
        RiskLevel::Safe
    }

    async fn execute(&self, args: Value) -> AppResult<ToolResult> {
        let id = args["id"].as_str();
        let task = wt::continue_task(id)?
            .ok_or_else(|| AppError::InvalidArgument("没有活动的工作接管任务".to_string()))?;
        Ok(ToolResult::ok(json!({
            "ok": true,
            "status": "continued",
            "persisted": true,
            "task": task_json(&task),
        })))
    }
}

// ============================================================
// work_takeover_task_advance
// ============================================================

pub struct WorkTakeoverTaskAdvanceTool;

#[async_trait]
impl AgentTool for WorkTakeoverTaskAdvanceTool {
    fn name(&self) -> &'static str {
        "work_takeover_task_advance"
    }

    fn description(&self) -> &'static str {
        "把工作接管任务推进一个动作（current_action_index +1，状态转 in_progress）。只改本地台账，不执行外部副作用。"
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": { "id": { "type": "string" } },
            "required": ["id"]
        })
    }

    fn risk_level(&self) -> RiskLevel {
        RiskLevel::Low
    }

    async fn execute(&self, args: Value) -> AppResult<ToolResult> {
        let id = require_str(&args, "id")?;
        let task = wt::advance_task(id)?
            .ok_or_else(|| AppError::InvalidArgument(format!("任务不存在: {}", id)))?;
        Ok(ToolResult::ok(json!({
            "ok": true,
            "status": "advanced",
            "persisted": true,
            "task": task_json(&task),
        })))
    }
}

// ============================================================
// work_takeover_task_export_packet
// ============================================================

pub struct WorkTakeoverTaskExportPacketTool;

#[async_trait]
impl AgentTool for WorkTakeoverTaskExportPacketTool {
    fn name(&self) -> &'static str {
        "work_takeover_task_export_packet"
    }

    fn description(&self) -> &'static str {
        "把工作接管任务导出为本地文件包（task.json 完整状态 + summary.md 人读摘要）。只落本地文件，不发/不发布/不提交。"
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "id": { "type": "string" },
                "output_directory": { "type": "string", "description": "输出目录，默认桌面" }
            },
            "required": ["id"]
        })
    }

    fn risk_level(&self) -> RiskLevel {
        RiskLevel::Low
    }

    async fn execute(&self, args: Value) -> AppResult<ToolResult> {
        let id = require_str(&args, "id")?;
        let task = wt::get_task(id)?
            .ok_or_else(|| AppError::InvalidArgument(format!("任务不存在: {}", id)))?;
        let output_dir = args["output_directory"].as_str().unwrap_or("");
        let output_dir = if output_dir.is_empty() {
            dirs::desktop_dir()
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_else(|| std::env::temp_dir().to_string_lossy().to_string())
        } else {
            output_dir.to_string()
        };
        let folder = wt::export_packet(&task, &output_dir)?;
        Ok(ToolResult::ok(json!({
            "ok": true,
            "status": "exported",
            "folder_path": folder,
            "task_id": task.id,
        })))
    }
}

// ============================================================
// work_takeover_capability_reuse_probe
// ============================================================

pub struct WorkTakeoverCapabilityReuseProbeTool;

#[async_trait]
impl AgentTool for WorkTakeoverCapabilityReuseProbeTool {
    fn name(&self) -> &'static str {
        "work_takeover_capability_reuse_probe"
    }

    fn description(&self) -> &'static str {
        "探测某个工具能力是否可复用：查它的契约（副作用类别、落点、可逆性、风险、验证策略、是否需要确认）。接管任务前用它判断某个能力能不能直接复用。"
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": { "tool": { "type": "string" } },
            "required": ["tool"]
        })
    }

    fn risk_level(&self) -> RiskLevel {
        RiskLevel::Safe
    }

    async fn execute(&self, args: Value) -> AppResult<ToolResult> {
        let tool = require_str(&args, "tool")?;
        match wt::probe_capability(tool) {
            Some(info) => Ok(ToolResult::ok(json!({ "available": true, "capability": info }))),
            None => Ok(ToolResult::err(
                "CAPABILITY_UNKNOWN",
                &format!("工具「{}」未注册或没有契约，无法复用", tool),
            )),
        }
    }
}

// ============================================================
// work_takeover_evidence_start
// ============================================================

pub struct WorkTakeoverEvidenceStartTool;

#[async_trait]
impl AgentTool for WorkTakeoverEvidenceStartTool {
    fn name(&self) -> &'static str {
        "work_takeover_evidence_start"
    }

    fn description(&self) -> &'static str {
        "给工作接管任务开始一条证据步骤（关联任务 + 步骤 id + 证据 JSON），用于把工具回执/结果挂到任务上形成可审计证据链。"
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "task_id": { "type": "string" },
                "step_id": { "type": "string" },
                "evidence": { "type": "object", "description": "证据内容（回执 id、结果摘要等）" }
            },
            "required": ["task_id", "step_id"]
        })
    }

    fn risk_level(&self) -> RiskLevel {
        RiskLevel::Low
    }

    async fn execute(&self, args: Value) -> AppResult<ToolResult> {
        let task_id = require_str(&args, "task_id")?;
        let step_id = require_str(&args, "step_id")?;
        let evidence = args["evidence"].clone();
        let entry = wt::evidence_start(task_id, step_id, evidence)?;
        Ok(ToolResult::ok(json!({
            "ok": true,
            "workflow_id": entry.workflow_id,
            "task_id": entry.task_id,
            "step_id": entry.step_id,
            "status": entry.status,
        })))
    }
}

// ============================================================
// work_takeover_evidence_update
// ============================================================

pub struct WorkTakeoverEvidenceUpdateTool;

#[async_trait]
impl AgentTool for WorkTakeoverEvidenceUpdateTool {
    fn name(&self) -> &'static str {
        "work_takeover_evidence_update"
    }

    fn description(&self) -> &'static str {
        "更新证据步骤状态（active/verified/promoted 等）与证据内容。"
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "workflow_id": { "type": "string" },
                "status": { "type": "string", "description": "active/verified/promoted/failed" },
                "evidence": { "type": "object" }
            },
            "required": ["workflow_id", "status"]
        })
    }

    fn risk_level(&self) -> RiskLevel {
        RiskLevel::Low
    }

    async fn execute(&self, args: Value) -> AppResult<ToolResult> {
        let workflow_id = require_str(&args, "workflow_id")?;
        let status = require_str(&args, "status")?;
        let evidence = args["evidence"].clone();
        let entry = wt::evidence_update(workflow_id, status, evidence)?
            .ok_or_else(|| AppError::InvalidArgument(format!("证据工作流不存在: {}", workflow_id)))?;
        Ok(ToolResult::ok(json!({
            "ok": true,
            "workflow_id": entry.workflow_id,
            "status": entry.status,
        })))
    }
}

// ============================================================
// work_takeover_evidence_status
// ============================================================

pub struct WorkTakeoverEvidenceStatusTool;

#[async_trait]
impl AgentTool for WorkTakeoverEvidenceStatusTool {
    fn name(&self) -> &'static str {
        "work_takeover_evidence_status"
    }

    fn description(&self) -> &'static str {
        "查证据工作流状态。"
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": { "workflow_id": { "type": "string" } },
            "required": ["workflow_id"]
        })
    }

    fn risk_level(&self) -> RiskLevel {
        RiskLevel::Safe
    }

    async fn execute(&self, args: Value) -> AppResult<ToolResult> {
        let workflow_id = require_str(&args, "workflow_id")?;
        let entry = wt::evidence_status(workflow_id)?
            .ok_or_else(|| AppError::InvalidArgument(format!("证据工作流不存在: {}", workflow_id)))?;
        Ok(ToolResult::ok(json!({
            "workflow_id": entry.workflow_id,
            "task_id": entry.task_id,
            "step_id": entry.step_id,
            "status": entry.status,
            "evidence": entry.evidence,
            "updated_at": entry.updated_at,
        })))
    }
}

// ============================================================
// work_takeover_evidence_promote
// ============================================================

pub struct WorkTakeoverEvidencePromoteTool;

#[async_trait]
impl AgentTool for WorkTakeoverEvidencePromoteTool {
    fn name(&self) -> &'static str {
        "work_takeover_evidence_promote"
    }

    fn description(&self) -> &'static str {
        "把证据步骤提升为已采纳（promoted），表示这条证据已纳入任务结论。"
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": { "workflow_id": { "type": "string" } },
            "required": ["workflow_id"]
        })
    }

    fn risk_level(&self) -> RiskLevel {
        RiskLevel::Low
    }

    async fn execute(&self, args: Value) -> AppResult<ToolResult> {
        let workflow_id = require_str(&args, "workflow_id")?;
        let entry = wt::evidence_promote(workflow_id)?
            .ok_or_else(|| AppError::InvalidArgument(format!("证据工作流不存在: {}", workflow_id)))?;
        Ok(ToolResult::ok(json!({
            "ok": true,
            "workflow_id": entry.workflow_id,
            "status": entry.status,
        })))
    }
}

// ============================================================
// work_takeover_evidence_list
// ============================================================

pub struct WorkTakeoverEvidenceListTool;

#[async_trait]
impl AgentTool for WorkTakeoverEvidenceListTool {
    fn name(&self) -> &'static str {
        "work_takeover_evidence_list"
    }

    fn description(&self) -> &'static str {
        "列出某个工作接管任务的所有证据步骤。"
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": { "task_id": { "type": "string" } },
            "required": ["task_id"]
        })
    }

    fn risk_level(&self) -> RiskLevel {
        RiskLevel::Safe
    }

    async fn execute(&self, args: Value) -> AppResult<ToolResult> {
        let task_id = require_str(&args, "task_id")?;
        let entries = wt::evidence_for_task(task_id)?;
        Ok(ToolResult::ok(json!({
            "count": entries.len(),
            "evidence": entries.iter().map(|e| json!({
                "workflow_id": e.workflow_id,
                "step_id": e.step_id,
                "status": e.status,
                "evidence": e.evidence,
                "updated_at": e.updated_at,
            })).collect::<Vec<_>>(),
        })))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn task_json_includes_confirmation_boundary() {
        let t = wt::WorkTakeoverTask {
            id: "t1".into(),
            title: "标题".into(),
            category: "general_work".into(),
            source: "manual".into(),
            source_message: String::new(),
            contact: String::new(),
            summary: String::new(),
            urgency: "normal".into(),
            priority: 50,
            recommended_workflow: String::new(),
            next_actions: vec!["a".into()],
            current_action_index: 0,
            draft_reply: String::new(),
            artifacts: Vec::new(),
            allowed_now: vec!["a".into()],
            confirmation_required: vec!["b".into()],
            blocked_by: Vec::new(),
            risks: Vec::new(),
            result: String::new(),
            note: String::new(),
            status: "queued".into(),
            metadata: json!({}),
            created_at: String::new(),
            updated_at: String::new(),
        };
        let j = task_json(&t);
        assert_eq!(j["current_action"], "a");
        assert_eq!(j["allowed_now"][0], "a");
        assert_eq!(j["confirmation_required"][0], "b");
    }

    #[test]
    fn str_array_handles_string_and_array() {
        assert_eq!(str_array(&json!({"x": ["a","b"]}), "x"), vec!["a", "b"]);
        assert_eq!(str_array(&json!({"x": "a\nb"}), "x"), vec!["a", "b"]);
        assert_eq!(str_array(&json!({"x": 1}), "x"), Vec::<String>::new());
    }
}
