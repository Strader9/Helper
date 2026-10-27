//! 本地日历事件台账（P3）。
//!
//! 与 `create_calendar_event`（生成 .ics 文件导入 Outlook/系统日历）不同，
//! 这一簇工具维护一份**助手自己的本地事件台账**（SQLite `calendar_events` 表），
//! 用于「提醒我明天下午开会」这类需求的持久化与查询，不依赖 Outlook COM。
//!
//! 时间统一存本地时间字符串 `YYYY-MM-DD HH:MM:SS`，按 `date('now','localtime')` 过滤。

use async_trait::async_trait;
use rusqlite::params;
use serde_json::{json, Value};

use crate::error::{AppError, AppResult};
use crate::tools::{AgentTool, RiskLevel, ToolResult};

fn db() -> AppResult<crate::db::Database> {
    let path = crate::memory::get_db_path()
        .ok_or_else(|| AppError::Internal("数据库未初始化".to_string()))?;
    crate::db::Database::new(path)
}

/// 把用户输入的时间归一化成 `YYYY-MM-DD HH:MM:SS`。支持 `YYYY-MM-DD`、`YYYY-MM-DD HH:MM`、`YYYY-MM-DD HH:MM:SS`。
fn normalize_dt(raw: &str) -> Option<String> {
    let s = raw.trim();
    for fmt in ["%Y-%m-%d %H:%M:%S", "%Y-%m-%d %H:%M", "%Y-%m-%d"] {
        if let Ok(dt) = chrono::NaiveDateTime::parse_from_str(s, fmt) {
            return Some(dt.format("%Y-%m-%d %H:%M:%S").to_string());
        }
    }
    None
}

fn row_to_event(row: &rusqlite::Row) -> rusqlite::Result<Value> {
    Ok(json!({
        "id": row.get::<_, String>(0)?,
        "title": row.get::<_, String>(1)?,
        "start_at": row.get::<_, String>(2)?,
        "end_at": row.get::<_, Option<String>>(3)?,
        "location": row.get::<_, Option<String>>(4)?,
        "description": row.get::<_, Option<String>>(5)?,
        "reminder_minutes": row.get::<_, i64>(6)?,
        "source": row.get::<_, String>(7)?,
    }))
}

const EVENT_COLS: &str =
    "id, title, start_at, end_at, location, description, reminder_minutes, source";

fn collect(sql: &str, params: &[&dyn rusqlite::ToSql]) -> AppResult<Vec<Value>> {
    let db = db()?;
    let conn = db.connection();
    let mut stmt = conn.prepare(sql)?;
    let rows = stmt.query_map(params, row_to_event)?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

pub struct CalendarCreateTool;

#[async_trait]
impl AgentTool for CalendarCreateTool {
    fn name(&self) -> &'static str {
        "calendar_create"
    }

    fn description(&self) -> &'static str {
        "在本地事件台账里新建一条日历事件。时间按本机时区理解，格式 YYYY-MM-DD HH:MM（时分可省）。"
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "title": { "type": "string", "description": "事件标题" },
                "start_at": { "type": "string", "description": "开始时间，如 2026-10-20 15:00" },
                "end_at": { "type": "string", "description": "结束时间（可选，默认开始后 1 小时）" },
                "location": { "type": "string", "description": "地点（可选）" },
                "description": { "type": "string", "description": "备注（可选）" },
                "reminder_minutes": { "type": "integer", "description": "提前提醒分钟数，默认 0" }
            },
            "required": ["title", "start_at"],
            "additionalProperties": false
        })
    }

    fn risk_level(&self) -> RiskLevel {
        RiskLevel::Low
    }

    async fn execute(&self, args: serde_json::Value) -> AppResult<ToolResult> {
        let title = args["title"]
            .as_str()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| AppError::InvalidArgument("title is required".into()))?;
        let start_raw = args["start_at"]
            .as_str()
            .ok_or_else(|| AppError::InvalidArgument("start_at is required".into()))?;
        let start = normalize_dt(start_raw).ok_or_else(|| {
            AppError::InvalidArgument(format!("start_at 时间格式无法解析：{}", start_raw))
        })?;

        let end = match args["end_at"]
            .as_str()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            Some(s) => normalize_dt(s).ok_or_else(|| {
                AppError::InvalidArgument(format!("end_at 时间格式无法解析：{}", s))
            })?,
            None => {
                let st =
                    chrono::NaiveDateTime::parse_from_str(&start, "%Y-%m-%d %H:%M:%S").unwrap();
                (st + chrono::Duration::hours(1))
                    .format("%Y-%m-%d %H:%M:%S")
                    .to_string()
            }
        };

        let location = args["location"].as_str().map(str::trim).map(str::to_string);
        let description = args["description"]
            .as_str()
            .map(str::trim)
            .map(str::to_string);
        let reminder = args["reminder_minutes"]
            .as_i64()
            .unwrap_or(0)
            .clamp(0, 7 * 24 * 60);

        let id = uuid::Uuid::new_v4().to_string();
        let db = db()?;
        db.connection().execute(
            "INSERT INTO calendar_events (id, title, start_at, end_at, location, description, reminder_minutes, source)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 'manual')",
            params![id, title, start, end, location, description, reminder],
        )?;

        Ok(ToolResult::ok(
            json!({ "ok": true, "id": id, "title": title, "start_at": start, "end_at": end }),
        ))
    }
}

pub struct CalendarTodayTool;

#[async_trait]
impl AgentTool for CalendarTodayTool {
    fn name(&self) -> &'static str {
        "calendar_today"
    }

    fn description(&self) -> &'static str {
        "列出本地事件台账里今天的全部事件（按开始时间排序）。"
    }

    fn parameters(&self) -> serde_json::Value {
        json!({ "type": "object", "properties": {}, "additionalProperties": false })
    }

    fn risk_level(&self) -> RiskLevel {
        RiskLevel::Safe
    }

    async fn execute(&self, _args: serde_json::Value) -> AppResult<ToolResult> {
        let sql = format!(
            "SELECT {} FROM calendar_events WHERE substr(start_at,1,10) = date('now','localtime') ORDER BY start_at",
            EVENT_COLS
        );
        let items = collect(&sql, &[])?;
        Ok(ToolResult::ok(
            json!({ "count": items.len(), "items": items }),
        ))
    }
}

pub struct UpcomingEventsTool;

#[async_trait]
impl AgentTool for UpcomingEventsTool {
    fn name(&self) -> &'static str {
        "upcoming_events"
    }

    fn description(&self) -> &'static str {
        "列出本地事件台账里未来 N 天（默认 7，上限 30）内的事件（按开始时间排序）。"
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": { "days": { "type": "integer", "description": "向前看的天数，默认 7，上限 30" } },
            "additionalProperties": false
        })
    }

    fn risk_level(&self) -> RiskLevel {
        RiskLevel::Safe
    }

    async fn execute(&self, args: serde_json::Value) -> AppResult<ToolResult> {
        let days = args["days"].as_i64().unwrap_or(7).clamp(1, 30);
        let sql = format!(
            "SELECT {} FROM calendar_events
             WHERE start_at >= datetime('now','localtime')
               AND start_at <= datetime('now','localtime', '+{} days')
             ORDER BY start_at",
            EVENT_COLS, days
        );
        let items = collect(&sql, &[])?;
        Ok(ToolResult::ok(
            json!({ "days": days, "count": items.len(), "items": items }),
        ))
    }
}

pub struct CalendarModifyTool;

#[async_trait]
impl AgentTool for CalendarModifyTool {
    fn name(&self) -> &'static str {
        "calendar_modify"
    }

    fn description(&self) -> &'static str {
        "修改本地事件台账里的一条事件（按 id，只更新传入的字段）。"
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "id": { "type": "string", "description": "事件 id" },
                "title": { "type": "string", "description": "新标题（可选）" },
                "start_at": { "type": "string", "description": "新开始时间（可选）" },
                "end_at": { "type": "string", "description": "新结束时间（可选）" },
                "location": { "type": "string", "description": "新地点（可选）" },
                "description": { "type": "string", "description": "新备注（可选）" },
                "reminder_minutes": { "type": "integer", "description": "新提醒分钟数（可选）" }
            },
            "required": ["id"],
            "additionalProperties": false
        })
    }

    fn risk_level(&self) -> RiskLevel {
        RiskLevel::Medium
    }

    async fn execute(&self, args: serde_json::Value) -> AppResult<ToolResult> {
        let id = args["id"]
            .as_str()
            .ok_or_else(|| AppError::InvalidArgument("id is required".into()))?;
        let mut sets: Vec<String> = Vec::new();
        let mut vals: Vec<String> = Vec::new();

        if let Some(v) = args["title"]
            .as_str()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            sets.push("title = ?".into());
            vals.push(v.to_string());
        }
        if let Some(raw) = args["start_at"].as_str() {
            let norm = normalize_dt(raw).ok_or_else(|| {
                AppError::InvalidArgument(format!("start_at 时间格式无法解析：{}", raw))
            })?;
            sets.push("start_at = ?".into());
            vals.push(norm);
        }
        if let Some(raw) = args["end_at"].as_str() {
            let norm = normalize_dt(raw).ok_or_else(|| {
                AppError::InvalidArgument(format!("end_at 时间格式无法解析：{}", raw))
            })?;
            sets.push("end_at = ?".into());
            vals.push(norm);
        }
        if let Some(v) = args["location"].as_str() {
            sets.push("location = ?".into());
            vals.push(v.to_string());
        }
        if let Some(v) = args["description"].as_str() {
            sets.push("description = ?".into());
            vals.push(v.to_string());
        }
        if let Some(v) = args["reminder_minutes"].as_i64() {
            sets.push("reminder_minutes = ?".into());
            vals.push(v.clamp(0, 7 * 24 * 60).to_string());
        }

        if sets.is_empty() {
            return Ok(ToolResult::err("NO_FIELDS", "没有提供任何要更新的字段"));
        }

        let db = db()?;
        let sql = format!(
            "UPDATE calendar_events SET {}, updated_at = datetime('now') WHERE id = ?",
            sets.join(", ")
        );
        let mut stmt = db.connection().prepare(&sql)?;
        // 所有值都是 String（时间已归一化），id 追加在末尾对应 WHERE id = ?
        let mut vals = vals;
        vals.push(id.to_string());
        let refs: Vec<&dyn rusqlite::ToSql> =
            vals.iter().map(|v| v as &dyn rusqlite::ToSql).collect();
        let n = stmt.execute(refs.as_slice())?;

        Ok(ToolResult::ok(
            json!({ "ok": n > 0, "affected": n, "id": id }),
        ))
    }
}

pub struct CalendarDeleteTool;

#[async_trait]
impl AgentTool for CalendarDeleteTool {
    fn name(&self) -> &'static str {
        "calendar_delete"
    }

    fn description(&self) -> &'static str {
        "删除本地事件台账里的一条事件（按 id）。"
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": { "id": { "type": "string", "description": "事件 id" } },
            "required": ["id"],
            "additionalProperties": false
        })
    }

    fn risk_level(&self) -> RiskLevel {
        RiskLevel::Medium
    }

    async fn execute(&self, args: serde_json::Value) -> AppResult<ToolResult> {
        let id = args["id"]
            .as_str()
            .ok_or_else(|| AppError::InvalidArgument("id is required".into()))?;
        let db = db()?;
        let n = db
            .connection()
            .execute("DELETE FROM calendar_events WHERE id = ?1", params![id])?;
        Ok(ToolResult::ok(
            json!({ "ok": n > 0, "affected": n, "id": id }),
        ))
    }
}
