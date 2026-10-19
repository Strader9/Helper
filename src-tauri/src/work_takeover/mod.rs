//! 工作接管（work_takeover）
//!
//! 接管用户的一件具体工作（客户、店铺、账号、案件、交付），把"当前动作 /
//! 草稿 / 确认边界 / 阻塞 / 风险 / 产物"持久化下来，随时断点续做。
//!
//! 与 `agent_tasks` / `workflow` 的分工（见 `db::schema::MIGRATION_WORK_TAKEOVER_SQL`
//! 的注释）：这里的关键区别是任务**带着确认边界**——明确"哪些现在能做、
//! 哪些要等用户点头、哪些被卡住"。这是"接管"与"我帮你临时记个清单"的分水岭：
//! 接管意味着要能替用户判断"现在能动哪一步、哪一步必须先问"。
//!
//! 只做**本地台账**与**本地产物**（任务包导出），不发消息、不发布、不提交、
//! 不付款、不签名、不操作外部软件——外部动作永远走工具层三层闸门，由用户确认。

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::error::{AppError, AppResult};

/// 一条工作接管任务。
///
/// 字段名与 LumiCore 的 work_takeover task 对齐，便于语义互认；数组字段
/// 在库里存 JSON 字符串，读出时解析。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkTakeoverTask {
    pub id: String,
    pub title: String,
    pub category: String,
    pub source: String,
    pub source_message: String,
    pub contact: String,
    pub summary: String,
    pub urgency: String,
    pub priority: i64,
    pub recommended_workflow: String,
    pub next_actions: Vec<String>,
    pub current_action_index: i64,
    pub draft_reply: String,
    pub artifacts: Vec<Artifact>,
    pub allowed_now: Vec<String>,
    pub confirmation_required: Vec<String>,
    pub blocked_by: Vec<String>,
    pub risks: Vec<String>,
    pub result: String,
    pub note: String,
    pub status: String,
    pub metadata: Value,
    pub created_at: String,
    pub updated_at: String,
}

/// 任务产物（文件 / 草稿 / 结果）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Artifact {
    #[serde(rename = "type")]
    pub kind: String,
    pub label: String,
    #[serde(default)]
    pub path: String,
    #[serde(default)]
    pub content: String,
    #[serde(default)]
    pub status: String,
}

impl Default for Artifact {
    fn default() -> Self {
        Self {
            kind: "note".to_string(),
            label: String::new(),
            path: String::new(),
            content: String::new(),
            status: "prepared".to_string(),
        }
    }
}

fn conn() -> AppResult<rusqlite::Connection> {
    crate::memory::open_connection()
        .ok_or_else(|| AppError::Internal("数据库未初始化".to_string()))
}

fn parse_str_array(v: &str) -> Vec<String> {
    serde_json::from_str(v).unwrap_or_default()
}

#[cfg(test)]
fn as_str_array(value: &Value) -> Vec<String> {
    match value {
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

fn row_to_task(row: &rusqlite::Row) -> rusqlite::Result<WorkTakeoverTask> {
    // 索引严格对齐 select_cols() 的列顺序（与 schema 列序一致）：
    // 0 id, 1 title, 2 category, 3 source, 4 source_message, 5 contact,
    // 6 summary, 7 urgency, 8 priority, 9 recommended_workflow,
    // 10 next_actions_json, 11 current_action_index, 12 draft_reply,
    // 13 artifacts_json, 14 allowed_now_json, 15 confirmation_required_json,
    // 16 blocked_by_json, 17 risks_json, 18 result, 19 note, 20 status,
    // 21 metadata_json, 22 created_at, 23 updated_at
    let next_actions: String = row.get(10)?;
    let artifacts: String = row.get(13)?;
    let allowed_now: String = row.get(14)?;
    let confirmation_required: String = row.get(15)?;
    let blocked_by: String = row.get(16)?;
    let risks: String = row.get(17)?;
    let metadata: String = row.get(21)?;
    Ok(WorkTakeoverTask {
        id: row.get(0)?,
        title: row.get(1)?,
        category: row.get(2)?,
        source: row.get(3)?,
        source_message: row.get(4)?,
        contact: row.get(5)?,
        summary: row.get(6)?,
        urgency: row.get(7)?,
        priority: row.get(8)?,
        recommended_workflow: row.get(9)?,
        next_actions: parse_str_array(&next_actions),
        current_action_index: row.get(11)?,
        draft_reply: row.get(12)?,
        artifacts: serde_json::from_str(&artifacts).unwrap_or_default(),
        allowed_now: parse_str_array(&allowed_now),
        confirmation_required: parse_str_array(&confirmation_required),
        blocked_by: parse_str_array(&blocked_by),
        risks: parse_str_array(&risks),
        result: row.get(18)?,
        note: row.get(19)?,
        status: row.get(20)?,
        metadata: serde_json::from_str(&metadata).unwrap_or_else(|_| json!({})),
        created_at: row.get(22)?,
        updated_at: row.get(23)?,
    })
}

/// 建一条任务。
pub fn create_task(input: CreateTaskInput) -> AppResult<WorkTakeoverTask> {
    let c = conn()?;
    let id = uuid::Uuid::new_v4().to_string();
    c.execute(
        "INSERT INTO work_takeover_tasks
         (id, title, category, source, source_message, contact, summary, urgency, priority,
          recommended_workflow, next_actions_json, current_action_index, draft_reply,
          artifacts_json, allowed_now_json, confirmation_required_json, blocked_by_json,
          risks_json, result, note, status, metadata_json)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, '', '', 'queued', ?19)",
        rusqlite::params![
            id,
            input.title,
            input.category,
            input.source,
            input.source_message,
            input.contact,
            input.summary,
            input.urgency,
            input.priority,
            input.recommended_workflow,
            serde_json::to_string(&input.next_actions)?,
            input.current_action_index,
            input.draft_reply,
            serde_json::to_string(&input.artifacts)?,
            serde_json::to_string(&input.allowed_now)?,
            serde_json::to_string(&input.confirmation_required)?,
            serde_json::to_string(&input.blocked_by)?,
            serde_json::to_string(&input.risks)?,
            serde_json::to_string(&input.metadata)?,
        ],
    )?;
    get_task(&id)?.ok_or_else(|| AppError::Internal("任务创建后读回失败".to_string()))
}

/// 建任务入参。
pub struct CreateTaskInput {
    pub title: String,
    pub category: String,
    pub source: String,
    pub source_message: String,
    pub contact: String,
    pub summary: String,
    pub urgency: String,
    pub priority: i64,
    pub recommended_workflow: String,
    pub next_actions: Vec<String>,
    pub current_action_index: i64,
    pub draft_reply: String,
    pub artifacts: Vec<Artifact>,
    pub allowed_now: Vec<String>,
    pub confirmation_required: Vec<String>,
    pub blocked_by: Vec<String>,
    pub risks: Vec<String>,
    pub metadata: Value,
}

impl Default for CreateTaskInput {
    fn default() -> Self {
        Self {
            title: String::new(),
            category: "general_work".to_string(),
            source: "manual".to_string(),
            source_message: String::new(),
            contact: String::new(),
            summary: String::new(),
            urgency: "normal".to_string(),
            priority: 50,
            recommended_workflow: String::new(),
            next_actions: Vec::new(),
            current_action_index: 0,
            draft_reply: String::new(),
            artifacts: Vec::new(),
            allowed_now: Vec::new(),
            confirmation_required: Vec::new(),
            blocked_by: Vec::new(),
            risks: Vec::new(),
            metadata: json!({}),
        }
    }
}

fn select_cols() -> &'static str {
    "id, title, category, source, source_message, contact, summary, urgency, priority,
     recommended_workflow, next_actions_json, current_action_index, draft_reply, artifacts_json,
     allowed_now_json, confirmation_required_json, blocked_by_json, risks_json, result,
     note, status, metadata_json, created_at, updated_at"
}

/// 按 id 查一条任务。
pub fn get_task(id: &str) -> AppResult<Option<WorkTakeoverTask>> {
    let c = conn()?;
    let mut stmt = c.prepare(&format!(
        "SELECT {} FROM work_takeover_tasks WHERE id = ?1",
        select_cols()
    ))?;
    let mut rows = stmt.query([id])?;
    if let Some(row) = rows.next()? {
        Ok(Some(row_to_task(row)?))
    } else {
        Ok(None)
    }
}

/// 列任务（可选按状态 / 类别过滤）。
pub fn list_tasks(status: Option<&str>, category: Option<&str>, limit: usize) -> AppResult<Vec<WorkTakeoverTask>> {
    let c = conn()?;
    let mut sql = format!(
        "SELECT {} FROM work_takeover_tasks WHERE 1=1",
        select_cols()
    );
    let mut params: Vec<String> = Vec::new();
    if let Some(s) = status.filter(|s| !s.is_empty() && *s != "active") {
        sql.push_str(" AND status = ?");
        params.push(s.to_string());
    } else if status.filter(|s| *s == "active").is_some() {
        sql.push_str(" AND status IN ('queued','in_progress','waiting_confirmation')");
    }
    if let Some(cat) = category.filter(|c| !c.is_empty()) {
        sql.push_str(" AND category = ?");
        params.push(cat.to_string());
    }
    sql.push_str(" ORDER BY priority DESC, updated_at DESC LIMIT ?");
    params.push(limit.min(500).to_string());

    let mut stmt = c.prepare(&sql)?;
    let mut rows = stmt.query(rusqlite::params_from_iter(params.iter()))?;
    let mut out = Vec::new();
    while let Some(row) = rows.next()? {
        out.push(row_to_task(row)?);
    }
    Ok(out)
}

/// 更新任务字段。返回更新后的任务。
pub fn update_task(id: &str, patch: UpdateTaskPatch) -> AppResult<Option<WorkTakeoverTask>> {
    let c = conn()?;
    let mut sets: Vec<String> = Vec::new();
    let mut params: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();

    macro_rules! push_str {
        ($field:expr, $opt:expr) => {
            if let Some(v) = $opt {
                sets.push(format!("{} = ?", $field));
                params.push(Box::new(v.to_string()));
            }
        };
    }
    macro_rules! push_arr {
        ($field:expr, $opt:expr) => {
            if let Some(v) = $opt {
                sets.push(format!("{} = ?", $field));
                params.push(Box::new(serde_json::to_string(&v).unwrap_or_else(|_| "[]".to_string())));
            }
        };
    }

    push_str!("status", patch.status);
    push_str!("title", patch.title);
    push_str!("summary", patch.summary);
    push_str!("urgency", patch.urgency);
    if let Some(p) = patch.priority {
        sets.push("priority = ?".to_string());
        params.push(Box::new(p));
    }
    push_str!("recommended_workflow", patch.recommended_workflow);
    push_arr!("next_actions_json", patch.next_actions);
    if let Some(i) = patch.current_action_index {
        sets.push("current_action_index = ?".to_string());
        params.push(Box::new(i));
    }
    push_str!("draft_reply", patch.draft_reply);
    push_arr!("artifacts_json", patch.artifacts);
    push_arr!("allowed_now_json", patch.allowed_now);
    push_arr!("confirmation_required_json", patch.confirmation_required);
    push_arr!("blocked_by_json", patch.blocked_by);
    push_arr!("risks_json", patch.risks);
    push_str!("result", patch.result);
    push_str!("note", patch.note);

    if sets.is_empty() {
        return get_task(id);
    }

    sets.push("updated_at = datetime('now')".to_string());
    let sql = format!(
        "UPDATE work_takeover_tasks SET {} WHERE id = ?",
        sets.join(", ")
    );
    params.push(Box::new(id.to_string()));
    c.execute(&sql, rusqlite::params_from_iter(params.iter()))?;
    get_task(id)
}

/// 更新任务字段。
#[derive(Default)]
pub struct UpdateTaskPatch {
    pub status: Option<String>,
    pub title: Option<String>,
    pub summary: Option<String>,
    pub urgency: Option<String>,
    pub priority: Option<i64>,
    pub recommended_workflow: Option<String>,
    pub next_actions: Option<Vec<String>>,
    pub current_action_index: Option<i64>,
    pub draft_reply: Option<String>,
    pub artifacts: Option<Vec<Artifact>>,
    pub allowed_now: Option<Vec<String>>,
    pub confirmation_required: Option<Vec<String>>,
    pub blocked_by: Option<Vec<String>>,
    pub risks: Option<Vec<String>>,
    pub result: Option<String>,
    pub note: Option<String>,
}

/// 继续任务：优先取指定 id，否则取最高优先级的活动任务。
pub fn continue_task(id: Option<&str>) -> AppResult<Option<WorkTakeoverTask>> {
    if let Some(id) = id {
        return get_task(id);
    }
    let active = list_tasks(Some("active"), None, 1)?;
    Ok(active.into_iter().next())
}

/// 当前动作（next_actions 里的第 current_action_index 项）。
pub fn current_action(task: &WorkTakeoverTask) -> Option<String> {
    let idx = task.current_action_index.max(0) as usize;
    task.next_actions.get(idx).cloned()
}

/// 推进一个动作：current_action_index +1，状态保持 in_progress。
pub fn advance_task(id: &str) -> AppResult<Option<WorkTakeoverTask>> {
    let task = get_task(id)?.ok_or_else(|| AppError::InvalidArgument(format!("任务不存在: {}", id)))?;
    let next_index = (task.current_action_index + 1).min(task.next_actions.len().max(1) as i64 - 1);
    let status = if task.status == "queued" { "in_progress".to_string() } else { task.status.clone() };
    update_task(
        id,
        UpdateTaskPatch {
            current_action_index: Some(next_index),
            status: Some(status),
            ..Default::default()
        },
    )
}

// ============================================================
// 证据工作流
// ============================================================

/// 一条证据工作流步骤。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvidenceEntry {
    pub workflow_id: String,
    pub task_id: String,
    pub step_id: String,
    pub status: String,
    pub evidence: Value,
    pub updated_at: String,
}

/// 开始一个证据工作流（给任务挂一条证据步骤）。
pub fn evidence_start(task_id: &str, step_id: &str, evidence: Value) -> AppResult<EvidenceEntry> {
    let c = conn()?;
    let workflow_id = uuid::Uuid::new_v4().to_string();
    c.execute(
        "INSERT INTO work_takeover_evidence (task_id, workflow_id, step_id, status, evidence_json)
         VALUES (?1, ?2, ?3, 'active', ?4)",
        rusqlite::params![task_id, workflow_id, step_id, serde_json::to_string(&evidence)?],
    )?;
    Ok(EvidenceEntry {
        workflow_id,
        task_id: task_id.to_string(),
        step_id: step_id.to_string(),
        status: "active".to_string(),
        evidence,
        updated_at: String::new(),
    })
}

/// 推进 / 更新证据步骤状态。
pub fn evidence_update(workflow_id: &str, status: &str, evidence: Value) -> AppResult<Option<EvidenceEntry>> {
    let c = conn()?;
    c.execute(
        "UPDATE work_takeover_evidence SET status = ?1, evidence_json = ?2, updated_at = datetime('now')
         WHERE workflow_id = ?3",
        rusqlite::params![status, serde_json::to_string(&evidence)?, workflow_id],
    )?;
    evidence_status(workflow_id)
}

/// 查证据工作流状态。
pub fn evidence_status(workflow_id: &str) -> AppResult<Option<EvidenceEntry>> {
    let c = conn()?;
    let mut stmt = c.prepare(
        "SELECT task_id, workflow_id, step_id, status, evidence_json, updated_at
         FROM work_takeover_evidence WHERE workflow_id = ?1",
    )?;
    let mut rows = stmt.query([workflow_id])?;
    if let Some(row) = rows.next()? {
        let evidence: String = row.get(4)?;
        Ok(Some(EvidenceEntry {
            task_id: row.get(0)?,
            workflow_id: row.get(1)?,
            step_id: row.get(2)?,
            status: row.get(3)?,
            evidence: serde_json::from_str(&evidence).unwrap_or(json!({})),
            updated_at: row.get(5)?,
        }))
    } else {
        Ok(None)
    }
}

/// 列某个任务的所有证据步骤。
pub fn evidence_for_task(task_id: &str) -> AppResult<Vec<EvidenceEntry>> {
    let c = conn()?;
    let mut stmt = c.prepare(
        "SELECT task_id, workflow_id, step_id, status, evidence_json, updated_at
         FROM work_takeover_evidence WHERE task_id = ?1 ORDER BY id",
    )?;
    let mut rows = stmt.query([task_id])?;
    let mut out = Vec::new();
    while let Some(row) = rows.next()? {
        let evidence: String = row.get(4)?;
        out.push(EvidenceEntry {
            task_id: row.get(0)?,
            workflow_id: row.get(1)?,
            step_id: row.get(2)?,
            status: row.get(3)?,
            evidence: serde_json::from_str(&evidence).unwrap_or(json!({})),
            updated_at: row.get(5)?,
        });
    }
    Ok(out)
}

/// 提升证据：把步骤状态设为 promoted，表示"这条证据已纳入任务结论"。
pub fn evidence_promote(workflow_id: &str) -> AppResult<Option<EvidenceEntry>> {
    evidence_update(workflow_id, "promoted", json!({ "promoted": true }))
}

// ============================================================
// 能力复用探针
// ============================================================

/// 探测某个工具能力是否可复用：查它有没有契约、风险等级、副作用类别。
///
/// 返回 None 表示"该工具名不存在/未注册"；Some 里带契约的静态属性，
/// 供"接管"时判断某个能力能不能直接复用、还是需要用户确认。
pub fn probe_capability(tool_name: &str) -> Option<Value> {
    let contract = crate::tools::contract::contract_for(tool_name);
    let contract = contract?;
    Some(json!({
        "tool": contract.tool,
        "capability_id": contract.tool,
        "side_effect": contract.side_effect.as_str(),
        "scope": contract.scope,
        "reversible": contract.reversible,
        "verification_strategy": contract.verification.as_str(),
        "requires_confirmation": contract.side_effect.is_material(),
    }))
}

// ============================================================
// 任务包导出
// ============================================================

/// 把任务导出为本地文件包（JSON + 可读摘要）。
///
/// 落一个文件夹 `<输出目录>/work_takeover_<task_id前8位>/`，内含
/// `task.json`（完整状态）与 `summary.md`（人可读摘要）。不碰外部系统。
pub fn export_packet(task: &WorkTakeoverTask, output_dir: &str) -> AppResult<String> {
    let short = &task.id[..task.id.len().min(8)];
    let folder = std::path::Path::new(output_dir).join(format!("work_takeover_{}", short));
    std::fs::create_dir_all(&folder)?;
    std::fs::write(folder.join("task.json"), serde_json::to_string_pretty(task)?)?;

    let mut md = String::new();
    md.push_str(&format!("# 工作接管任务：{}\n\n", task.title));
    md.push_str(&format!("- 类别：{}\n", task.category));
    md.push_str(&format!("- 状态：{}\n", task.status));
    md.push_str(&format!("- 紧急度：{} / 优先级：{}\n", task.urgency, task.priority));
    md.push_str(&format!("- 来源：{}（{}）\n\n", task.source, task.contact));
    if !task.summary.is_empty() {
        md.push_str(&format!("## 摘要\n{}\n\n", task.summary));
    }
    if !task.next_actions.is_empty() {
        md.push_str("## 下一步动作\n");
        for (i, a) in task.next_actions.iter().enumerate() {
            let mark = if i as i64 == task.current_action_index { " →" } else { "" };
            md.push_str(&format!("{}. {}{}\n", i + 1, a, mark));
        }
        md.push('\n');
    }
    if !task.confirmation_required.is_empty() {
        md.push_str(&format!("## 需要确认\n{}\n\n", task.confirmation_required.join("、")));
    }
    if !task.blocked_by.is_empty() {
        md.push_str(&format!("## 阻塞\n{}\n\n", task.blocked_by.join("、")));
    }
    if !task.draft_reply.is_empty() {
        md.push_str(&format!("## 草稿\n{}\n\n", task.draft_reply));
    }
    std::fs::write(folder.join("summary.md"), md)?;
    Ok(folder.to_string_lossy().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn array_parser_handles_json_and_newlines() {
        assert_eq!(as_str_array(&json!(["a", "b"])), vec!["a", "b"]);
        assert_eq!(as_str_array(&json!("a\nb;c")), vec!["a", "b", "c"]);
        assert_eq!(as_str_array(&json!("  ")), Vec::<String>::new());
    }

    #[test]
    fn current_action_respects_index() {
        let mut task = WorkTakeoverTask {
            id: "x".into(),
            next_actions: vec!["a".into(), "b".into()],
            current_action_index: 1,
            ..test_task()
        };
        assert_eq!(current_action(&task).as_deref(), Some("b"));
        task.current_action_index = 0;
        assert_eq!(current_action(&task).as_deref(), Some("a"));
    }

    #[test]
    fn probe_unknown_tool_returns_none() {
        assert!(probe_capability("no_such_tool_xyz").is_none());
    }

    fn test_task() -> WorkTakeoverTask {
        WorkTakeoverTask {
            id: "x".into(),
            title: String::new(),
            category: "general_work".into(),
            source: "manual".into(),
            source_message: String::new(),
            contact: String::new(),
            summary: String::new(),
            urgency: "normal".into(),
            priority: 50,
            recommended_workflow: String::new(),
            next_actions: Vec::new(),
            current_action_index: 0,
            draft_reply: String::new(),
            artifacts: Vec::new(),
            allowed_now: Vec::new(),
            confirmation_required: Vec::new(),
            blocked_by: Vec::new(),
            risks: Vec::new(),
            result: String::new(),
            note: String::new(),
            status: "queued".into(),
            metadata: json!({}),
            created_at: String::new(),
            updated_at: String::new(),
        }
    }
}
