//! 运维与状态工具（P3 长尾 · 本地状态 / 配置 / 用量 / 运行时工作 / 外部 CLI）。
//!
//! 这一簇工具只读/写本地账本与本地配置，不引入新的外部依赖：
//! - usage_get_summary       能力用量统计（回执账本 + 审计日志）
//! - model_configuration_*   模型连接配置的读 / 改 / 联通性自测
//! - sleep_*                 低活动（休眠）状态
//! - knowledge_*             知识库文件与覆盖率统计
//! - runtime_work_*          运行时工作（agent_tasks）的暂停 / 恢复 / 取消
//! - external_cli_*          白名单外部命令（比裸 execute_command 更收敛）

use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use rusqlite::params;
use serde_json::{json, Value};

#[cfg(windows)]
use std::os::windows::process::CommandExt;
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x08000000;

use crate::error::{AppError, AppResult};
use crate::tools::{AgentTool, RiskLevel, ToolResult};

/// 打开全局数据库（工具层拿不到 registry 实例，只能从全局路径重开）。
fn db() -> AppResult<crate::db::Database> {
    let path = crate::memory::get_db_path()
        .ok_or_else(|| AppError::Internal("数据库未初始化".to_string()))?;
    crate::db::Database::new(path)
}

/// 以 upsert 语义写入一个字符串配置（补上 sleep 这类默认清单里没有的键）。
fn upsert_setting(key: &str, value: &str) -> AppResult<()> {
    let db = db()?;
    db.connection().execute(
        "INSERT INTO settings (key, value, value_type) VALUES (?1, ?2, 'string')
         ON CONFLICT(key) DO UPDATE SET value = excluded.value, updated_at = datetime('now')",
        params![key, value],
    )?;
    Ok(())
}

/// 截断 CLI 输出时按 Unicode 字符而非 UTF-8 字节计数，避免中文或 emoji 正好
/// 落在边界上时对字符串切片产生 panic。
fn truncate_cli_output(text: String, limit: usize) -> String {
    let total_chars = text.chars().count();
    if total_chars <= limit {
        return text;
    }
    let head: String = text.chars().take(limit).collect();
    format!("{head}... (截断，共 {total_chars} 字符)")
}

// ============================================================
// 能力用量统计
// ============================================================

pub struct UsageGetSummaryTool;

#[async_trait]
impl AgentTool for UsageGetSummaryTool {
    fn name(&self) -> &'static str {
        "usage_get_summary"
    }

    fn description(&self) -> &'static str {
        "汇总本地能力用量：工具调用次数、成功/失败、按工具分组、审计事件与拒绝次数。用于回答「这段时间用了多少、用在哪」。"
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "since": { "type": "string", "description": "可选，起始时间（ISO 或 'YYYY-MM-DD'），留空统计全部" },
                "limit_tools": { "type": "integer", "description": "按工具分组返回的条数上限，默认 20" }
            },
            "additionalProperties": false
        })
    }

    fn risk_level(&self) -> RiskLevel {
        RiskLevel::Safe
    }

    async fn execute(&self, args: serde_json::Value) -> AppResult<ToolResult> {
        let db = db()?;
        let conn = db.connection();
        let limit = args["limit_tools"].as_i64().unwrap_or(20).clamp(1, 100);

        let (total, success, failed): (i64, i64, i64) = {
            let mut stmt = conn.prepare(
                "SELECT COUNT(*), COALESCE(SUM(success), 0),
                        COALESCE(SUM(CASE WHEN success = 0 THEN 1 ELSE 0 END), 0)
                 FROM tool_receipts",
            )?;
            let mut rows = stmt.query([])?;
            let row = rows.next()?.expect("至少一行");
            (row.get(0)?, row.get(1)?, row.get(2)?)
        };

        let mut by_tool = Vec::new();
        {
            let mut stmt = conn.prepare(
                "SELECT tool_name, COUNT(*), COALESCE(SUM(success), 0), COALESCE(AVG(duration_ms), 0)
                 FROM tool_receipts GROUP BY tool_name ORDER BY COUNT(*) DESC LIMIT ?1",
            )?;
            let rows = stmt.query_map(params![limit], |row| {
                Ok(json!({
                    "tool": row.get::<_, String>(0)?,
                    "calls": row.get::<_, i64>(1)?,
                    "success": row.get::<_, i64>(2)?,
                    "avg_duration_ms": row.get::<_, f64>(3)?.round() as i64,
                }))
            })?;
            for r in rows {
                by_tool.push(r?);
            }
        }

        let (audit_total, audit_denied): (i64, i64) = {
            let mut stmt = conn.prepare(
                "SELECT COUNT(*),
                        COALESCE(SUM(CASE WHEN permission_result = 'DENIED' THEN 1 ELSE 0 END), 0)
                 FROM audit_logs",
            )?;
            let mut rows = stmt.query([])?;
            let row = rows.next()?.expect("至少一行");
            (row.get(0)?, row.get(1)?)
        };

        Ok(ToolResult::ok(json!({
            "total_calls": total,
            "success": success,
            "failed": failed,
            "success_rate": if total > 0 { success as f64 / total as f64 } else { 0.0 },
            "by_tool": by_tool,
            "audit_events": audit_total,
            "audit_denied": audit_denied,
        })))
    }
}

// ============================================================
// 模型配置：读
// ============================================================

/// 读取时把凭据类键替换成掩码，明文永不进入工具结果。
fn mask_if_secret(key: &str, value: &str) -> String {
    if crate::db::is_secret_setting(key) {
        crate::db::mask_secret(value)
    } else {
        value.to_string()
    }
}

pub struct ModelConfigurationGetTool;

#[async_trait]
impl AgentTool for ModelConfigurationGetTool {
    fn name(&self) -> &'static str {
        "model_configuration_get"
    }

    fn description(&self) -> &'static str {
        "读取模型连接相关配置（Ollama / sidecar 网关 / agent 采样参数）。凭据类键会被掩码，不回显明文。"
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "prefix": { "type": "string", "description": "可选，键前缀过滤（如 ollama. / agent. / sidecar.）" }
            },
            "additionalProperties": false
        })
    }

    fn risk_level(&self) -> RiskLevel {
        RiskLevel::Safe
    }

    async fn execute(&self, args: serde_json::Value) -> AppResult<ToolResult> {
        let db = db()?;
        let prefix = args["prefix"].as_str().unwrap_or("");
        let items = db.get_all_settings()?;

        let filtered: Vec<Value> = items
            .iter()
            .filter(|it| prefix.is_empty() || it.key.starts_with(prefix))
            .map(|it| {
                json!({
                    "key": it.key,
                    "value": mask_if_secret(&it.key, &it.value),
                    "value_type": it.value_type,
                    "description": it.description,
                    "unit": it.unit,
                })
            })
            .collect();

        Ok(ToolResult::ok(json!({
            "count": filtered.len(),
            "settings": filtered,
        })))
    }
}

// ============================================================
// 模型配置：改
// ============================================================

/// 只允许改这一小撮键；凭据键永远不在列表里（改凭据走 Tauri 命令，不进 LLM 可调工具）。
const UPDATABLE_KEYS: &[&str] = &[
    "ollama.base_url",
    "ollama.model",
    "ollama.timeout_ms",
    "sidecar.url",
    "agent.temperature",
    "agent.num_ctx",
    "agent.num_predict",
    "agent.max_iterations",
    "agent.proactive_interval_sec",
    "agent.proactive_cooldown_sec",
];

pub struct ModelConfigurationUpdateTool;

#[async_trait]
impl AgentTool for ModelConfigurationUpdateTool {
    fn name(&self) -> &'static str {
        "model_configuration_update"
    }

    fn description(&self) -> &'static str {
        "更新一个模型/agent 配置键（仅白名单键，含取值范围校验）。凭据键不能通过此工具修改。"
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "key": { "type": "string", "description": "配置键，如 ollama.model / agent.temperature" },
                "value": { "type": "string", "description": "新值" }
            },
            "required": ["key", "value"],
            "additionalProperties": false
        })
    }

    fn risk_level(&self) -> RiskLevel {
        RiskLevel::Medium
    }

    async fn execute(&self, args: serde_json::Value) -> AppResult<ToolResult> {
        let key = args["key"].as_str().ok_or_else(|| AppError::InvalidArgument("key is required".into()))?;
        let value = args["value"].as_str().ok_or_else(|| AppError::InvalidArgument("value is required".into()))?;

        if !UPDATABLE_KEYS.contains(&key) {
            return Ok(ToolResult::err(
                "KEY_NOT_UPDATABLE",
                &format!("键「{}」不在可更新白名单内（凭据类键请走设置界面）", key),
            ));
        }

        let db = db()?;
        // 数值键做范围校验
        if let Some((vt, lo, hi, _unit)) = db.get_setting_bounds(key)? {
            if vt == "number" {
                let parsed: f64 = value.parse().map_err(|_| {
                    AppError::InvalidArgument(format!("键「{}」要求数值，得到「{}」", key, value))
                })?;
                if let Some(lo) = lo {
                    if parsed < lo {
                        return Ok(ToolResult::err("VALUE_OUT_OF_RANGE", &format!("值 {} 低于下界 {}", parsed, lo)));
                    }
                }
                if let Some(hi) = hi {
                    if parsed > hi {
                        return Ok(ToolResult::err("VALUE_OUT_OF_RANGE", &format!("值 {} 高于上界 {}", parsed, hi)));
                    }
                }
            }
        }

        db.update_setting(key, value)?;
        Ok(ToolResult::ok(json!({
            "ok": true,
            "key": key,
            "value": value,
        })))
    }
}

// ============================================================
// 模型配置：联通性自测
// ============================================================

pub struct ModelConfigurationTestTool;

#[async_trait]
impl AgentTool for ModelConfigurationTestTool {
    fn name(&self) -> &'static str {
        "model_configuration_test"
    }

    fn description(&self) -> &'static str {
        "自测模型连接：ollama（列出可用模型、核对默认模型是否已拉取）或 sidecar 网关（探活）。"
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "provider": { "type": "string", "description": "ollama 或 sidecar，默认 ollama" }
            },
            "additionalProperties": false
        })
    }

    fn risk_level(&self) -> RiskLevel {
        RiskLevel::Low
    }

    async fn execute(&self, args: serde_json::Value) -> AppResult<ToolResult> {
        let provider = args["provider"].as_str().unwrap_or("ollama");
        let db = db()?;

        match provider {
            "sidecar" => {
                let base = db.get_setting("sidecar.url")?.unwrap_or_else(|| "http://127.0.0.1:4770".into());
                let url = base.trim_end_matches('/').to_string() + "/health";
                let started = Instant::now();
                let resp = reqwest::Client::builder()
                    .timeout(Duration::from_secs(5))
                    .build()
                    .map_err(|e| AppError::Internal(format!("构造客户端失败: {}", e)))?
                    .get(&url)
                    .send()
                    .await;
                let ms = started.elapsed().as_millis() as i64;
                match resp {
                    Ok(r) => Ok(ToolResult::ok(json!({
                        "provider": "sidecar",
                        "reachable": true,
                        "status_code": r.status().as_u16(),
                        "latency_ms": ms,
                    }))),
                    Err(e) => Ok(ToolResult::ok(json!({
                        "provider": "sidecar",
                        "reachable": false,
                        "error": e.to_string(),
                        "latency_ms": ms,
                    }))),
                }
            }
            _ => {
                let base = db.get_setting("ollama.base_url")?.unwrap_or_else(|| "http://localhost:11434".into());
                let model = db.get_setting("ollama.model")?.unwrap_or_else(|| "qwen3:8b".into());
                let timeout = db.get_setting("ollama.timeout_ms")?.and_then(|s| s.parse().ok()).unwrap_or(5000u64);

                let client = crate::llm::OllamaClient::new(base.clone(), timeout);
                let reachable = client.is_available().await;
                let models = if reachable { client.list_models().await.unwrap_or_default() } else { vec![] };
                let model_present = models.iter().any(|m| m.name == model || m.model == model);

                Ok(ToolResult::ok(json!({
                    "provider": "ollama",
                    "base_url": base,
                    "reachable": reachable,
                    "default_model": model,
                    "model_present": model_present,
                    "model_count": models.len(),
                    "models": models.iter().map(|m| m.name.clone()).collect::<Vec<_>>(),
                })))
            }
        }
    }
}

// ============================================================
// 低活动（休眠）状态
// ============================================================

pub struct SleepStatusTool;

#[async_trait]
impl AgentTool for SleepStatusTool {
    fn name(&self) -> &'static str {
        "sleep_status"
    }

    fn description(&self) -> &'static str {
        "查询当前低活动（休眠）状态：是否休眠、计划何时恢复。"
    }

    fn parameters(&self) -> serde_json::Value {
        json!({ "type": "object", "properties": {}, "additionalProperties": false })
    }

    fn risk_level(&self) -> RiskLevel {
        RiskLevel::Safe
    }

    async fn execute(&self, _args: serde_json::Value) -> AppResult<ToolResult> {
        let db = db()?;
        let mode = db.get_setting("agent.sleep_mode")?.unwrap_or_else(|| "off".into());
        let until = db.get_setting("agent.sleep_until")?.unwrap_or_default();
        Ok(ToolResult::ok(json!({
            "mode": mode,
            "sleep_until": until,
        })))
    }
}

pub struct SleepCycleTool;

#[async_trait]
impl AgentTool for SleepCycleTool {
    fn name(&self) -> &'static str {
        "sleep_cycle"
    }

    fn description(&self) -> &'static str {
        "进入或退出低活动（休眠）状态。进入时可指定分钟数，到期自动视为可恢复。"
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "mode": { "type": "string", "description": "on 或 off" },
                "minutes": { "type": "integer", "description": "休眠时长（分钟），默认 30，仅 mode=on 时生效" }
            },
            "required": ["mode"],
            "additionalProperties": false
        })
    }

    fn risk_level(&self) -> RiskLevel {
        RiskLevel::Medium
    }

    async fn execute(&self, args: serde_json::Value) -> AppResult<ToolResult> {
        let mode = args["mode"].as_str().ok_or_else(|| AppError::InvalidArgument("mode is required".into()))?;
        if mode != "on" && mode != "off" {
            return Ok(ToolResult::err("INVALID_MODE", "mode 只能是 on 或 off"));
        }
        let until = if mode == "on" {
            let minutes = args["minutes"].as_i64().unwrap_or(30).clamp(1, 1440);
            let deadline = chrono::Local::now() + chrono::Duration::minutes(minutes);
            deadline.format("%Y-%m-%d %H:%M:%S").to_string()
        } else {
            String::new()
        };

        upsert_setting("agent.sleep_mode", mode)?;
        upsert_setting("agent.sleep_until", &until)?;

        Ok(ToolResult::ok(json!({ "ok": true, "mode": mode, "sleep_until": until })))
    }
}

// ============================================================
// 知识库统计
// ============================================================

pub struct KnowledgeFileStatsTool;

#[async_trait]
impl AgentTool for KnowledgeFileStatsTool {
    fn name(&self) -> &'static str {
        "knowledge_file_stats"
    }

    fn description(&self) -> &'static str {
        "统计本地知识库：文档数、总字节、总分块、总字符，以及按扩展名分布。"
    }

    fn parameters(&self) -> serde_json::Value {
        json!({ "type": "object", "properties": {}, "additionalProperties": false })
    }

    fn risk_level(&self) -> RiskLevel {
        RiskLevel::Safe
    }

    async fn execute(&self, _args: serde_json::Value) -> AppResult<ToolResult> {
        let db = db()?;
        let conn = db.connection();

        let (docs, bytes, chunks, chars): (i64, i64, i64, i64) = {
            let mut stmt = conn.prepare(
                "SELECT (SELECT COUNT(*) FROM knowledge_documents),
                        (SELECT COALESCE(SUM(bytes), 0) FROM knowledge_documents),
                        (SELECT COALESCE(SUM(chunk_count), 0) FROM knowledge_documents),
                        (SELECT COALESCE(SUM(chars), 0) FROM knowledge_chunks)",
            )?;
            let mut rows = stmt.query([])?;
            let row = rows.next()?.expect("至少一行");
            (row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)
        };

        let mut by_ext = Vec::new();
        {
            let mut stmt = conn.prepare(
                "SELECT COALESCE(ext, '?'), COUNT(*), COALESCE(SUM(bytes), 0)
                 FROM knowledge_documents GROUP BY ext ORDER BY COUNT(*) DESC LIMIT 20",
            )?;
            let rows = stmt.query_map([], |row| {
                Ok(json!({
                    "ext": row.get::<_, String>(0)?,
                    "documents": row.get::<_, i64>(1)?,
                    "bytes": row.get::<_, i64>(2)?,
                }))
            })?;
            for r in rows {
                by_ext.push(r?);
            }
        }

        Ok(ToolResult::ok(json!({
            "documents": docs,
            "total_bytes": bytes,
            "chunks": chunks,
            "total_chars": chars,
            "by_ext": by_ext,
        })))
    }
}

pub struct KnowledgeCoverageReportTool;

#[async_trait]
impl AgentTool for KnowledgeCoverageReportTool {
    fn name(&self) -> &'static str {
        "knowledge_coverage_report"
    }

    fn description(&self) -> &'static str {
        "报告知识库索引覆盖情况：已索引但零分块的文档（漏索引 / 空文件）、字符数分层分布。"
    }

    fn parameters(&self) -> serde_json::Value {
        json!({ "type": "object", "properties": {}, "additionalProperties": false })
    }

    fn risk_level(&self) -> RiskLevel {
        RiskLevel::Safe
    }

    async fn execute(&self, _args: serde_json::Value) -> AppResult<ToolResult> {
        let db = db()?;
        let conn = db.connection();

        let mut empty = Vec::new();
        {
            let mut stmt = conn.prepare(
                "SELECT id, path, bytes FROM knowledge_documents
                 WHERE chunk_count = 0 ORDER BY indexed_at DESC LIMIT 50",
            )?;
            let rows = stmt.query_map([], |row| {
                Ok(json!({
                    "id": row.get::<_, String>(0)?,
                    "path": row.get::<_, String>(1)?,
                    "bytes": row.get::<_, i64>(2)?,
                }))
            })?;
            for r in rows {
                empty.push(r?);
            }
        }

        let mut tiers = Vec::new();
        {
            let mut stmt = conn.prepare(
                "SELECT CASE
                            WHEN chunk_count = 0 THEN '0'
                            WHEN chunk_count <= 10 THEN '1-10'
                            WHEN chunk_count <= 100 THEN '11-100'
                            ELSE '100+' END AS tier,
                        COUNT(*)
                 FROM knowledge_documents GROUP BY tier ORDER BY tier",
            )?;
            let rows = stmt.query_map([], |row| {
                Ok(json!({ "tier": row.get::<_, String>(0)?, "documents": row.get::<_, i64>(1)? }))
            })?;
            for r in rows {
                tiers.push(r?);
            }
        }

        Ok(ToolResult::ok(json!({
            "empty_or_unindexed": empty,
            "empty_count": empty.len(),
            "chunk_tiers": tiers,
        })))
    }
}

// ============================================================
// 运行时工作（agent_tasks 的暂停 / 恢复 / 取消）
// ============================================================

fn active_tasks() -> AppResult<Vec<Value>> {
    let db = db()?;
    let conn = db.connection();
    let mut stmt = conn.prepare(
        "SELECT id, session_id, goal, status, current_step, updated_at
         FROM agent_tasks WHERE status NOT IN ('DONE', 'CANCELLED', 'FAILED')
         ORDER BY updated_at DESC LIMIT 50",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok(json!({
            "id": row.get::<_, String>(0)?,
            "session_id": row.get::<_, String>(1)?,
            "goal": row.get::<_, String>(2)?,
            "status": row.get::<_, String>(3)?,
            "current_step": row.get::<_, i64>(4)?,
            "updated_at": row.get::<_, String>(5)?,
        }))
    })?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

fn set_task_status(task_id: &str, status: &str) -> AppResult<i64> {
    let db = db()?;
    let n = db.connection().execute(
        "UPDATE agent_tasks SET status = ?1, updated_at = datetime('now') WHERE id = ?2",
        params![status, task_id],
    )?;
    Ok(n as i64)
}

pub struct RuntimeWorkStatusTool;

#[async_trait]
impl AgentTool for RuntimeWorkStatusTool {
    fn name(&self) -> &'static str {
        "runtime_work_status"
    }

    fn description(&self) -> &'static str {
        "列出仍在进行的运行时工作（进行中 / 已暂停的任务）。"
    }

    fn parameters(&self) -> serde_json::Value {
        json!({ "type": "object", "properties": {}, "additionalProperties": false })
    }

    fn risk_level(&self) -> RiskLevel {
        RiskLevel::Safe
    }

    async fn execute(&self, _args: serde_json::Value) -> AppResult<ToolResult> {
        let tasks = active_tasks()?;
        Ok(ToolResult::ok(json!({ "count": tasks.len(), "tasks": tasks })))
    }
}

pub struct RuntimeWorkPauseTool;

#[async_trait]
impl AgentTool for RuntimeWorkPauseTool {
    fn name(&self) -> &'static str {
        "runtime_work_pause"
    }

    fn description(&self) -> &'static str {
        "暂停一项运行时工作（按任务 id）。"
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": { "task_id": { "type": "string", "description": "任务 id" } },
            "required": ["task_id"],
            "additionalProperties": false
        })
    }

    fn risk_level(&self) -> RiskLevel {
        RiskLevel::Medium
    }

    async fn execute(&self, args: serde_json::Value) -> AppResult<ToolResult> {
        let task_id = args["task_id"].as_str().ok_or_else(|| AppError::InvalidArgument("task_id is required".into()))?;
        let n = set_task_status(task_id, "PAUSED")?;
        Ok(ToolResult::ok(json!({ "ok": n > 0, "affected": n, "task_id": task_id, "status": "PAUSED" })))
    }
}

pub struct RuntimeWorkResumeTool;

#[async_trait]
impl AgentTool for RuntimeWorkResumeTool {
    fn name(&self) -> &'static str {
        "runtime_work_resume"
    }

    fn description(&self) -> &'static str {
        "恢复一项已暂停的运行时工作（按任务 id）。"
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": { "task_id": { "type": "string", "description": "任务 id" } },
            "required": ["task_id"],
            "additionalProperties": false
        })
    }

    fn risk_level(&self) -> RiskLevel {
        RiskLevel::Medium
    }

    async fn execute(&self, args: serde_json::Value) -> AppResult<ToolResult> {
        let task_id = args["task_id"].as_str().ok_or_else(|| AppError::InvalidArgument("task_id is required".into()))?;
        let n = set_task_status(task_id, "PENDING")?;
        Ok(ToolResult::ok(json!({ "ok": n > 0, "affected": n, "task_id": task_id, "status": "PENDING" })))
    }
}

pub struct RuntimeWorkCancelTool;

#[async_trait]
impl AgentTool for RuntimeWorkCancelTool {
    fn name(&self) -> &'static str {
        "runtime_work_cancel"
    }

    fn description(&self) -> &'static str {
        "取消一项运行时工作（按任务 id，置为 CANCELLED）。"
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": { "task_id": { "type": "string", "description": "任务 id" } },
            "required": ["task_id"],
            "additionalProperties": false
        })
    }

    fn risk_level(&self) -> RiskLevel {
        RiskLevel::Medium
    }

    async fn execute(&self, args: serde_json::Value) -> AppResult<ToolResult> {
        let task_id = args["task_id"].as_str().ok_or_else(|| AppError::InvalidArgument("task_id is required".into()))?;
        let n = set_task_status(task_id, "CANCELLED")?;
        Ok(ToolResult::ok(json!({ "ok": n > 0, "affected": n, "task_id": task_id, "status": "CANCELLED" })))
    }
}

// ============================================================
// 外部 CLI（白名单）
// ============================================================

/// 白名单：只允许这些无交互、无持久副作用的只读/工具型命令。
const CLI_ALLOWLIST: &[&str] = &["git", "python", "node", "npm", "ffmpeg", "ffprobe", "where", "echo"];

fn last_run() -> AppResult<Value> {
    let db = db()?;
    let raw = db.get_setting("external_cli.last_run")?.unwrap_or_else(|| "{}".into());
    Ok(serde_json::from_str(&raw).unwrap_or_else(|_| json!({})))
}

fn save_last_run(v: &Value) -> AppResult<()> {
    upsert_setting("external_cli.last_run", &v.to_string())
}

/// Terminate a command that belongs to this invocation. On Windows, a CLI may
/// leave descendants behind (for example, an interpreter launching a helper),
/// so terminate the tree rooted at the child PID rather than only its parent.
#[cfg(windows)]
fn terminate_cli_process_tree(child: &mut std::process::Child) {
    let pid = child.id().to_string();
    let _ = Command::new("taskkill")
        .args(["/F", "/T", "/PID", pid.as_str()])
        .creation_flags(CREATE_NO_WINDOW)
        .output();
    let _ = child.kill();
}

#[cfg(not(windows))]
fn terminate_cli_process_tree(child: &mut std::process::Child) {
    let _ = child.kill();
}

/// 带超时的外部命令执行：读输出在独立线程，超时终止该命令的进程树，避免死锁与进程泄漏。
fn run_cli(program: &str, args: &[String], timeout_ms: u64) -> AppResult<(Option<i32>, String, String, bool)> {
    use std::io::Read;

    let mut cmd = Command::new(program);
    cmd.args(args);
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
    #[cfg(windows)]
    cmd.creation_flags(CREATE_NO_WINDOW);

    let mut child = cmd
        .spawn()
        .map_err(|e| AppError::InvalidArgument(format!("启动 {} 失败: {}", program, e)))?;

    let mut out = child.stdout.take().expect("stdout piped");
    let mut err = child.stderr.take().expect("stderr piped");

    let out_thread = std::thread::spawn(move || {
        let mut b = Vec::new();
        let _ = out.read_to_end(&mut b);
        b
    });
    let err_thread = std::thread::spawn(move || {
        let mut b = Vec::new();
        let _ = err.read_to_end(&mut b);
        b
    });

    let deadline = Instant::now() + Duration::from_millis(timeout_ms);
    let (code, timed_out) = loop {
        match child.try_wait()? {
            Some(s) => break (s.code(), false),
            None => {
                if Instant::now() >= deadline {
                    terminate_cli_process_tree(&mut child);
                    let _ = child.wait();
                    break (None, true);
                }
                std::thread::sleep(Duration::from_millis(20));
            }
        }
    };

    let out_bytes = out_thread.join().unwrap_or_default();
    let err_bytes = err_thread.join().unwrap_or_default();

    Ok((
        code,
        String::from_utf8_lossy(&out_bytes).to_string(),
        String::from_utf8_lossy(&err_bytes).to_string(),
        timed_out,
    ))
}

pub struct ExternalCliRunTool;

#[async_trait]
impl AgentTool for ExternalCliRunTool {
    fn name(&self) -> &'static str {
        "external_cli_run"
    }

    fn description(&self) -> &'static str {
        "运行一个白名单外部命令（git/python/node/npm/ffmpeg/ffprobe 等），带超时与输出截断。比 execute_command 更收敛、可审计。"
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "command": { "type": "string", "description": "白名单命令名，如 git / python / node" },
                "args": { "type": "array", "items": { "type": "string" }, "description": "命令参数" },
                "timeout_ms": { "type": "integer", "description": "超时毫秒，默认 30000" }
            },
            "required": ["command"],
            "additionalProperties": false
        })
    }

    fn risk_level(&self) -> RiskLevel {
        RiskLevel::Medium
    }

    async fn execute(&self, args: serde_json::Value) -> AppResult<ToolResult> {
        let program = args["command"].as_str().ok_or_else(|| AppError::InvalidArgument("command is required".into()))?;
        if !CLI_ALLOWLIST.contains(&program) {
            return Ok(ToolResult::err(
                "COMMAND_NOT_ALLOWLISTED",
                &format!("命令「{}」不在白名单内（可选: {}）", program, CLI_ALLOWLIST.join(", ")),
            ));
        }
        let program_owned = program.to_string();
        let cmd_args: Vec<String> = args["args"]
            .as_array()
            .map(|a| a.iter().filter_map(|v| v.as_str().map(|s| s.to_string())).collect())
            .unwrap_or_default();
        let timeout_ms = args["timeout_ms"].as_u64().unwrap_or(30000).clamp(1000, 120000);

        let (code, stdout, stderr, timed_out) = tokio::task::spawn_blocking(move || {
            run_cli(&program_owned, &cmd_args, timeout_ms)
        })
        .await
        .map_err(|e| AppError::Internal(format!("执行任务失败: {}", e)))??;

        let result = json!({
            "ok": !timed_out && code == Some(0),
            "command": program,
            "exit_code": code,
            "timed_out": timed_out,
            "stdout": truncate_cli_output(stdout, 4000),
            "stderr": truncate_cli_output(stderr, 4000),
        });
        save_last_run(&result)?;

        Ok(ToolResult::ok(result))
    }
}

#[cfg(test)]
mod tests {
    use super::{run_cli, truncate_cli_output};

    #[test]
    fn truncate_cli_output_keeps_utf8_boundaries() {
        let input = "中".repeat(4001);
        let output = truncate_cli_output(input, 4000);

        assert!(output.starts_with(&"中".repeat(4000)));
        assert!(output.ends_with("... (截断，共 4001 字符)"));
    }

    #[cfg(windows)]
    #[test]
    fn run_cli_timeout_stops_a_process_tree() {
        let args = vec!["/c".to_string(), "ping 127.0.0.1 -n 20 > nul".to_string()];
        let started = std::time::Instant::now();
        let (_, _, _, timed_out) = run_cli("cmd", &args, 50).expect("CLI should start");

        assert!(timed_out);
        assert!(started.elapsed() < std::time::Duration::from_secs(3));
    }
}

pub struct ExternalCliStatusTool;

#[async_trait]
impl AgentTool for ExternalCliStatusTool {
    fn name(&self) -> &'static str {
        "external_cli_status"
    }

    fn description(&self) -> &'static str {
        "查看上一次外部命令的简要状态（命令、退出码、是否超时）。"
    }

    fn parameters(&self) -> serde_json::Value {
        json!({ "type": "object", "properties": {}, "additionalProperties": false })
    }

    fn risk_level(&self) -> RiskLevel {
        RiskLevel::Safe
    }

    async fn execute(&self, _args: serde_json::Value) -> AppResult<ToolResult> {
        let last = last_run()?;
        if last.is_null() || last.as_object().map(|o| o.is_empty()).unwrap_or(true) {
            return Ok(ToolResult::ok(json!({ "has_run": false })));
        }
        Ok(ToolResult::ok(json!({
            "has_run": true,
            "command": last.get("command"),
            "exit_code": last.get("exit_code"),
            "timed_out": last.get("timed_out"),
        })))
    }
}

pub struct ExternalCliGetRunTool;

#[async_trait]
impl AgentTool for ExternalCliGetRunTool {
    fn name(&self) -> &'static str {
        "external_cli_get_run"
    }

    fn description(&self) -> &'static str {
        "取回上一次外部命令的完整输出（stdout / stderr）。"
    }

    fn parameters(&self) -> serde_json::Value {
        json!({ "type": "object", "properties": {}, "additionalProperties": false })
    }

    fn risk_level(&self) -> RiskLevel {
        RiskLevel::Safe
    }

    async fn execute(&self, _args: serde_json::Value) -> AppResult<ToolResult> {
        let last = last_run()?;
        Ok(ToolResult::ok(json!({ "last_run": last })))
    }
}
