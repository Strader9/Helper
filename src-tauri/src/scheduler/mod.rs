//! 常驻调度 + 无人值守自主执行
//!
//! 与 `proactive` 的分工见 `db::schema::MIGRATION_SCHEDULER_SQL` 的注释：
//! proactive 处理"越线了提醒我"，本模块处理"到点帮我把这件事做完"。
//!
//! 设计要点：
//!
//! 1. **调度只决定"什么时候轮到"**，具体怎么完成交给 Agent 循环。所以这里的
//!    执行单元是一次 `AgentRuntime::send_message`，它自带规划、工具调用与收尾。
//! 2. **无人值守 = 不弹窗**。以 `autonomous:` 开头的会话在 `ToolExecutor` 里被
//!    视为无人值守：任何需要用户确认的工具立即拒绝（而不是等 60 秒后超时拒绝），
//!    模型能读到"被拒绝：无人值守模式不允许"，从而换一条不需要确认的路径。
//! 3. **每次都留痕**。`scheduled_job_runs` 记录每轮的状态、摘要、用到的工具。
//!    用户第二天问"昨晚那个任务跑了没"，答案是查表而不是猜。
//! 4. **次数上限是硬闸门**。`max_runs` 到了就自动停用，避免一个写错的间隔任务
//!    在用户不知情时反复烧 token。

use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Datelike, Duration as ChronoDuration, Local, NaiveDate, NaiveDateTime, TimeZone, Timelike};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager};
use tokio::sync::Mutex;

use crate::agent::ChatEvent;

/// 无人值守会话的 ID 前缀。`ToolExecutor` 据此拒绝需要确认的操作。
pub const AUTONOMOUS_SESSION_PREFIX: &str = "autonomous:";

/// 调度表里统一使用的时间格式（本地时间）。
pub const TIME_FMT: &str = "%Y-%m-%d %H:%M:%S";

// ============================================================
// 时间与表达式
// ============================================================

fn fmt_time(dt: DateTime<Local>) -> String {
    dt.format(TIME_FMT).to_string()
}

fn now_string() -> String {
    fmt_time(Local::now())
}

/// 调度类型
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobKind {
    /// 固定间隔：`1800` / `30m` / `2h`
    Interval,
    /// 五段 cron：`0 9 * * 1-5`
    Cron,
    /// 一次性：`2026-10-20 09:00`
    Once,
}

impl JobKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            JobKind::Interval => "interval",
            JobKind::Cron => "cron",
            JobKind::Once => "once",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s.trim().to_lowercase().as_str() {
            "interval" | "every" | "间隔" => Some(JobKind::Interval),
            "cron" => Some(JobKind::Cron),
            "once" | "at" | "一次性" => Some(JobKind::Once),
            _ => None,
        }
    }
}

/// 把用户/模型给的间隔写法统一成秒数。
///
/// 接受 `90` / `90s` / `30m` / `2h`。纯数字按秒解释——这是最不容易误解的默认，
/// 因为 `interval` 这个类型名本身就在说"间隔多少秒"。
pub fn parse_interval_secs(raw: &str) -> Result<u64, String> {
    let text = raw.trim().to_lowercase();
    if text.is_empty() {
        return Err("间隔不能为空".to_string());
    }

    let (number_part, multiplier) = if let Some(rest) = text.strip_suffix("sec").or(text.strip_suffix('s')) {
        (rest.trim(), 1u64)
    } else if let Some(rest) = text.strip_suffix("min").or(text.strip_suffix('m')) {
        (rest.trim(), 60u64)
    } else if let Some(rest) = text.strip_suffix("hour").or(text.strip_suffix('h')) {
        (rest.trim(), 3600u64)
    } else if let Some(rest) = text.strip_suffix('d') {
        (rest.trim(), 86_400u64)
    } else {
        (text.as_str(), 1u64)
    };

    let value: u64 = number_part
        .trim()
        .parse()
        .map_err(|_| format!("无法把「{}」解析成间隔秒数，可写成 90 / 30m / 2h", raw.trim()))?;

    let secs = value
        .checked_mul(multiplier)
        .ok_or_else(|| "间隔过大".to_string())?;

    if secs < 30 {
        return Err("间隔不能小于 30 秒（太频繁只会刷屏并烧 token）".to_string());
    }
    if secs > 365 * 86_400 {
        return Err("间隔不能超过 365 天".to_string());
    }
    Ok(secs)
}

/// 人为可读的间隔描述
pub fn describe_interval(secs: u64) -> String {
    if secs % 86_400 == 0 {
        format!("每 {} 天", secs / 86_400)
    } else if secs % 3600 == 0 {
        format!("每 {} 小时", secs / 3600)
    } else if secs % 60 == 0 {
        format!("每 {} 分钟", secs / 60)
    } else {
        format!("每 {} 秒", secs)
    }
}

/// 五段 cron 规格（解析后）
#[derive(Debug, Clone)]
pub struct CronSpec {
    minutes: Vec<u32>,
    hours: Vec<u32>,
    days_of_month: Vec<u32>,
    months: Vec<u32>,
    days_of_week: Vec<u32>,
    /// 日期字段是否被限制（用于"日/周同时限制取或"的 cron 惯例）
    dom_restricted: bool,
    dow_restricted: bool,
}

fn parse_cron_field(field: &str, min: u32, max: u32, name: &str) -> Result<Vec<u32>, String> {
    let mut out: Vec<u32> = Vec::new();

    for part in field.split(',') {
        let part = part.trim();
        if part.is_empty() {
            return Err(format!("cron {} 字段中有空项", name));
        }

        let (range_part, step) = match part.split_once('/') {
            Some((range, step_text)) => {
                let step: u32 = step_text
                    .trim()
                    .parse()
                    .map_err(|_| format!("cron {} 字段的步长「{}」不是数字", name, step_text))?;
                if step == 0 {
                    return Err(format!("cron {} 字段的步长不能为 0", name));
                }
                (range.trim(), step)
            }
            None => (part, 1),
        };

        let (low, high) = if range_part == "*" {
            (min, max)
        } else if let Some((a, b)) = range_part.split_once('-') {
            let low: u32 = a
                .trim()
                .parse()
                .map_err(|_| format!("cron {} 字段的左界「{}」不合法", name, a))?;
            let high: u32 = b
                .trim()
                .parse()
                .map_err(|_| format!("cron {} 字段的右界「{}」不合法", name, b))?;
            (low, high)
        } else {
            let single: u32 = range_part
                .parse()
                .map_err(|_| format!("cron {} 字段的取值「{}」不合法", name, range_part))?;
            (single, single)
        };

        if low < min || high > max || low > high {
            return Err(format!(
                "cron {} 字段超出范围 {}..{}：{}", name, min, max, part
            ));
        }

        let mut value = low;
        while value <= high {
            out.push(value);
            value += step;
        }
    }

    out.sort_unstable();
    out.dedup();
    Ok(out)
}

impl CronSpec {
    /// 解析标准五段 cron：分 时 日 月 周
    pub fn parse(expr: &str) -> Result<Self, String> {
        let fields: Vec<&str> = expr.split_whitespace().collect();
        if fields.len() != 5 {
            return Err(format!(
                "cron 需要 5 段（分 时 日 月 周），收到 {} 段：{}",
                fields.len(),
                expr.trim()
            ));
        }

        let days_of_week_raw = parse_cron_field(fields[4], 0, 7, "周")?;
        // 7 与 0 都表示周日，统一成 0
        let mut days_of_week: Vec<u32> = days_of_week_raw
            .into_iter()
            .map(|d| if d == 7 { 0 } else { d })
            .collect();
        days_of_week.sort_unstable();
        days_of_week.dedup();

        Ok(Self {
            minutes: parse_cron_field(fields[0], 0, 59, "分")?,
            hours: parse_cron_field(fields[1], 0, 23, "时")?,
            days_of_month: parse_cron_field(fields[2], 1, 31, "日")?,
            months: parse_cron_field(fields[3], 1, 12, "月")?,
            days_of_week,
            dom_restricted: fields[2].trim() != "*",
            dow_restricted: fields[4].trim() != "*",
        })
    }

    fn matches(&self, dt: &DateTime<Local>) -> bool {
        if !self.minutes.contains(&dt.minute())
            || !self.hours.contains(&dt.hour())
            || !self.months.contains(&dt.month())
        {
            return false;
        }

        let dom_match = self.days_of_month.contains(&dt.day());
        let dow_match = self
            .days_of_week
            .contains(&dt.weekday().num_days_from_sunday());

        // cron 惯例：日与周都被限制时取"或"，只限制一个时取"与"（即只看那一个）
        match (self.dom_restricted, self.dow_restricted) {
            (true, true) => dom_match || dow_match,
            (true, false) => dom_match,
            (false, true) => dow_match,
            (false, false) => true,
        }
    }

    /// 从 `from` 之后（不含）找下一个匹配时刻。
    ///
    /// 逐分钟前进，上限一年。一年是刻意的硬上限：像 `0 0 30 2 *` 这种
    /// 永不成立的表达式如果无限找下去，调度线程就卡死了。
    pub fn next_after(&self, from: DateTime<Local>) -> Option<DateTime<Local>> {
        let start = from
            .with_second(0)?
            .with_nanosecond(0)?;
        let mut cursor = start + ChronoDuration::minutes(1);

        for _ in 0..(366 * 24 * 60) {
            if self.matches(&cursor) {
                return Some(cursor);
            }
            cursor += ChronoDuration::minutes(1);
        }
        None
    }
}

/// 把宽松写法规范化为可存储的表达式。
///
/// `cron` 额外接受几种中文日常说法（"每天 09:00" / "每小时"），
/// 因为模型和用户更可能这么说，而不是写 `0 9 * * *`。
pub fn normalize_schedule(kind: JobKind, raw: &str) -> Result<String, String> {
    let text = raw.trim();
    if text.is_empty() {
        return Err("schedule 不能为空".to_string());
    }

    match kind {
        JobKind::Interval => {
            let secs = parse_interval_secs(text)?;
            Ok(secs.to_string())
        }
        JobKind::Cron => {
            let compact: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
            if compact.split_whitespace().count() == 5 {
                // 已经是五段 cron，解析一遍确保合法
                CronSpec::parse(&compact)?;
                return Ok(compact);
            }
            Ok(expand_cron_shorthand(&compact)?)
        }
        JobKind::Once => {
            let dt = parse_once_datetime(text)?;
            Ok(fmt_time(dt))
        }
    }
}

/// 中文日常说法 → 五段 cron
fn expand_cron_shorthand(text: &str) -> Result<String, String> {
    let normalized = text.replace('：', ":");

    if normalized == "每小时" || normalized.eq_ignore_ascii_case("hourly") {
        return Ok("0 * * * *".to_string());
    }
    if normalized == "每分钟" || normalized.eq_ignore_ascii_case("every minute") {
        return Err("每分钟太频繁，请改用 interval 类型并至少 30 秒".to_string());
    }

    // "每天 09:00" / "每日 9:00" / "每天9点"
    for prefix in ["每天", "每日", "daily"] {
        if let Some(rest) = normalized.strip_prefix(prefix) {
            let rest = rest.trim();
            let (hour, minute) = parse_hour_minute(rest)?;
            return Ok(format!("{} {} * * *", minute, hour));
        }
    }

    // "每周一 09:00"
    if let Some(rest) = normalized
        .strip_prefix("每周")
        .or_else(|| normalized.strip_prefix("每星期"))
    {
        let rest = rest.trim();
        let mut chars = rest.chars();
        let weekday_char = chars.next().ok_or_else(|| "每周后需要跟周几".to_string())?;
        let dow = match weekday_char {
            '一' => 1,
            '二' => 2,
            '三' => 3,
            '四' => 4,
            '五' => 5,
            '六' => 6,
            '日' | '天' => 0,
            _ => return Err("周几只支持 一~六 / 日".to_string()),
        };
        let time_part = chars.as_str().trim();
        let (hour, minute) = if time_part.is_empty() {
            (9, 0)
        } else {
            parse_hour_minute(time_part)?
        };
        return Ok(format!("{} {} * * {}", minute, hour, dow));
    }

    Err(
        "无法识别的 cron 写法。请直接给五段 cron（如 0 9 * * 1-5），或写成「每天 09:00」「每周一 09:00」「每小时」"
            .to_string(),
    )
}

fn parse_hour_minute(text: &str) -> Result<(u32, u32), String> {
    let cleaned = text
        .trim()
        .trim_start_matches("上午")
        .trim_start_matches("下午")
        .replace('点', ":")
        .replace('时', ":")
        .replace('分', "");

    if let Some((h, m)) = cleaned.split_once(':') {
        let hour: u32 = h.trim().parse().map_err(|_| "小时部分不是数字".to_string())?;
        let minute_text = m.trim();
        let minute: u32 = if minute_text.is_empty() {
            0
        } else {
            minute_text.parse().map_err(|_| "分钟部分不是数字".to_string())?
        };
        if hour > 23 || minute > 59 {
            return Err(format!("时间超出范围：{}", text.trim()));
        }
        return Ok((hour, minute));
    }

    let hour: u32 = cleaned.trim().parse().map_err(|_| "时间格式无法识别".to_string())?;
    if hour > 23 {
        return Err(format!("小时超出范围：{}", text.trim()));
    }
    Ok((hour, 0))
}

fn parse_once_datetime(text: &str) -> Result<DateTime<Local>, String> {
    let trimmed = text.trim();

    if let Ok(dt) = DateTime::parse_from_rfc3339(trimmed) {
        return Ok(dt.with_timezone(&Local));
    }

    for format in ["%Y-%m-%d %H:%M:%S", "%Y-%m-%d %H:%M", "%Y/%m/%d %H:%M"] {
        if let Ok(naive) = NaiveDateTime::parse_from_str(trimmed, format) {
            return Local
                .from_local_datetime(&naive)
                .single()
                .ok_or_else(|| "该时刻在本地时区不存在（夏令时跳变）".to_string());
        }
    }

    if let Ok(date) = NaiveDate::parse_from_str(trimmed, "%Y-%m-%d") {
        let naive = date
            .and_hms_opt(9, 0, 0)
            .ok_or_else(|| "日期无法转换".to_string())?;
        return Local
            .from_local_datetime(&naive)
            .single()
            .ok_or_else(|| "该时刻在本地时区不存在（夏令时跳变）".to_string());
    }

    Err(format!(
        "无法解析执行时间「{}」，请用 YYYY-MM-DD HH:MM",
        trimmed
    ))
}

/// 计算 `from` 之后的下一次执行时刻。返回 `None` 表示不会再有下一次。
pub fn next_run_after(
    kind: JobKind,
    schedule: &str,
    from: DateTime<Local>,
) -> Result<Option<DateTime<Local>>, String> {
    match kind {
        JobKind::Interval => {
            let secs = parse_interval_secs(schedule)?;
            Ok(Some(from + ChronoDuration::seconds(secs as i64)))
        }
        JobKind::Cron => {
            let spec = CronSpec::parse(schedule)?;
            Ok(spec.next_after(from))
        }
        JobKind::Once => {
            let target = parse_once_datetime(schedule)?;
            if target > from {
                Ok(Some(target))
            } else {
                Ok(None)
            }
        }
    }
}

/// 人话描述一个作业的节奏
pub fn describe_schedule(kind: JobKind, schedule: &str) -> String {
    match kind {
        JobKind::Interval => parse_interval_secs(schedule)
            .map(describe_interval)
            .unwrap_or_else(|_| format!("间隔 {}", schedule)),
        JobKind::Cron => format!("按日程 {}", schedule),
        JobKind::Once => format!("于 {} 执行一次", schedule),
    }
}

// ============================================================
// 数据模型
// ============================================================

#[derive(Debug, Clone, Serialize)]
pub struct ScheduledJob {
    pub id: String,
    pub name: String,
    pub kind: String,
    pub schedule: String,
    pub goal: String,
    pub enabled: bool,
    pub next_run_at: Option<String>,
    pub last_run_at: Option<String>,
    pub run_count: i64,
    pub max_runs: Option<i64>,
    pub last_status: Option<String>,
    pub last_summary: Option<String>,
    pub last_error: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

impl ScheduledJob {
    pub fn kind_enum(&self) -> JobKind {
        JobKind::from_str(&self.kind).unwrap_or(JobKind::Interval)
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct JobRun {
    pub id: String,
    pub job_id: String,
    pub session_id: Option<String>,
    pub started_at: String,
    pub finished_at: Option<String>,
    pub status: String,
    pub summary: Option<String>,
    pub error: Option<String>,
    pub steps: i64,
    pub tools_used: Vec<String>,
}

const JOB_COLUMNS: &str = "id, name, kind, schedule, goal, enabled, next_run_at, last_run_at, \
                           run_count, max_runs, last_status, last_summary, last_error, \
                           created_at, updated_at";

fn row_to_job(row: &rusqlite::Row<'_>) -> rusqlite::Result<ScheduledJob> {
    Ok(ScheduledJob {
        id: row.get(0)?,
        name: row.get(1)?,
        kind: row.get(2)?,
        schedule: row.get(3)?,
        goal: row.get(4)?,
        enabled: row.get::<_, i64>(5)? != 0,
        next_run_at: row.get(6)?,
        last_run_at: row.get(7)?,
        run_count: row.get(8)?,
        max_runs: row.get(9)?,
        last_status: row.get(10)?,
        last_summary: row.get(11)?,
        last_error: row.get(12)?,
        created_at: row.get(13)?,
        updated_at: row.get(14)?,
    })
}

// ============================================================
// 数据库操作
// ============================================================

pub fn list_jobs(conn: &rusqlite::Connection) -> Result<Vec<ScheduledJob>, String> {
    let sql = format!(
        "SELECT {} FROM scheduled_jobs ORDER BY created_at DESC",
        JOB_COLUMNS
    );
    let mut stmt = conn.prepare(&sql).map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map([], |row| row_to_job(row))
        .map_err(|e| e.to_string())?;
    rows.collect::<Result<Vec<_>, _>>().map_err(|e| e.to_string())
}

pub fn get_job(conn: &rusqlite::Connection, id: &str) -> Result<Option<ScheduledJob>, String> {
    let sql = format!("SELECT {} FROM scheduled_jobs WHERE id = ?1", JOB_COLUMNS);
    let mut stmt = conn.prepare(&sql).map_err(|e| e.to_string())?;
    let mut rows = stmt.query([id]).map_err(|e| e.to_string())?;
    match rows.next().map_err(|e| e.to_string())? {
        Some(row) => Ok(Some(row_to_job(row).map_err(|e| e.to_string())?)),
        None => Ok(None),
    }
}

/// 按 id 或名称找作业：模型经常只记得名字，不记得 uuid。
pub fn find_job(conn: &rusqlite::Connection, id_or_name: &str) -> Result<Option<ScheduledJob>, String> {
    let key = id_or_name.trim();
    if key.is_empty() {
        return Ok(None);
    }
    if let Some(job) = get_job(conn, key)? {
        return Ok(Some(job));
    }

    let sql = format!(
        "SELECT {} FROM scheduled_jobs WHERE name = ?1 ORDER BY created_at DESC LIMIT 1",
        JOB_COLUMNS
    );
    let mut stmt = conn.prepare(&sql).map_err(|e| e.to_string())?;
    let mut rows = stmt.query([key]).map_err(|e| e.to_string())?;
    match rows.next().map_err(|e| e.to_string())? {
        Some(row) => Ok(Some(row_to_job(row).map_err(|e| e.to_string())?)),
        None => Ok(None),
    }
}

pub struct NewJob<'a> {
    pub name: &'a str,
    pub kind: JobKind,
    pub schedule: &'a str,
    pub goal: &'a str,
    pub enabled: bool,
    pub max_runs: Option<i64>,
}

/// 插入作业并算出首次执行时刻。
pub fn insert_job(conn: &rusqlite::Connection, job: NewJob<'_>) -> Result<ScheduledJob, String> {
    let id = uuid::Uuid::new_v4().to_string();
    let now = Local::now();
    let now_text = fmt_time(now);

    let next = match next_run_after(job.kind, job.schedule, now)? {
        Some(dt) => Some(fmt_time(dt)),
        None => {
            if job.kind == JobKind::Once {
                return Err("这个时间已经过去了，作业不会有下一次执行".to_string());
            }
            None
        }
    };

    conn.execute(
        "INSERT INTO scheduled_jobs \
         (id, name, kind, schedule, goal, enabled, next_run_at, run_count, max_runs, created_at, updated_at) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 0, ?8, ?9, ?9)",
        rusqlite::params![
            id,
            job.name.trim(),
            job.kind.as_str(),
            job.schedule,
            job.goal.trim(),
            if job.enabled { 1i64 } else { 0i64 },
            next,
            job.max_runs,
            now_text,
        ],
    )
    .map_err(|e| e.to_string())?;

    get_job(conn, &id)?.ok_or_else(|| "作业写入后读不回来".to_string())
}

pub fn delete_job(conn: &rusqlite::Connection, id: &str) -> Result<bool, String> {
    let affected = conn
        .execute("DELETE FROM scheduled_jobs WHERE id = ?1", [id])
        .map_err(|e| e.to_string())?;
    Ok(affected > 0)
}

pub fn set_enabled(conn: &rusqlite::Connection, id: &str, enabled: bool) -> Result<(), String> {
    conn.execute(
        "UPDATE scheduled_jobs SET enabled = ?2, updated_at = ?3 WHERE id = ?1",
        rusqlite::params![id, enabled as i64, now_string()],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

/// 把作业标记为"立即到期"，下一次 tick 就会执行。
///
/// 工具层拿不到 `AppHandle`，所以不能直接就地跑 Agent；改成提前到期是
/// 唯一既能立刻响应、又不把调度逻辑复制一份的做法。最长等一个 tick。
pub fn mark_due_now(conn: &rusqlite::Connection, job: &ScheduledJob) -> Result<(), String> {
    conn.execute(
        "UPDATE scheduled_jobs SET next_run_at = ?2, updated_at = ?3 WHERE id = ?1",
        rusqlite::params![job.id, now_string(), now_string()],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

/// 取出所有到期的作业
pub fn due_jobs(conn: &rusqlite::Connection, now: &str) -> Result<Vec<ScheduledJob>, String> {
    let sql = format!(
        "SELECT {} FROM scheduled_jobs \
         WHERE enabled = 1 AND next_run_at IS NOT NULL AND next_run_at <= ?1 \
         ORDER BY next_run_at ASC LIMIT 20",
        JOB_COLUMNS
    );
    let mut stmt = conn.prepare(&sql).map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map([now], |row| row_to_job(row))
        .map_err(|e| e.to_string())?;
    rows.collect::<Result<Vec<_>, _>>().map_err(|e| e.to_string())
}

/// 给无人值守会话建一行会话记录（chat_messages 有外键，必须先有会话）。
pub fn ensure_session(conn: &rusqlite::Connection, session_id: &str, title: &str) -> Result<(), String> {
    let now = now_string();
    conn.execute(
        "INSERT OR IGNORE INTO chat_sessions (id, title, created_at, updated_at, message_count, is_pinned, is_archived) \
         VALUES (?1, ?2, ?3, ?3, 0, 0, 0)",
        rusqlite::params![session_id, title, now],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

pub fn start_run(
    conn: &rusqlite::Connection,
    job_id: &str,
    session_id: &str,
) -> Result<String, String> {
    let id = uuid::Uuid::new_v4().to_string();
    conn.execute(
        "INSERT INTO scheduled_job_runs (id, job_id, session_id, started_at, status) \
         VALUES (?1, ?2, ?3, ?4, 'running')",
        rusqlite::params![id, job_id, session_id, now_string()],
    )
    .map_err(|e| e.to_string())?;
    Ok(id)
}

pub struct RunOutcome {
    pub status: String,
    pub summary: Option<String>,
    pub error: Option<String>,
    pub steps: i64,
    pub tools_used: Vec<String>,
}

/// 收尾：写运行记录 + 更新作业状态与下一次执行时间。
pub fn finish_run(
    conn: &rusqlite::Connection,
    job: &ScheduledJob,
    run_id: &str,
    outcome: &RunOutcome,
) -> Result<(), String> {
    let now = Local::now();
    let now_text = fmt_time(now);

    let tools_joined = if outcome.tools_used.is_empty() {
        None
    } else {
        Some(outcome.tools_used.join(", "))
    };

    conn.execute(
        "UPDATE scheduled_job_runs SET finished_at = ?2, status = ?3, summary = ?4, \
         error = ?5, steps = ?6, tools_used = ?7 WHERE id = ?1",
        rusqlite::params![
            run_id,
            now_text,
            outcome.status,
            outcome.summary,
            outcome.error,
            outcome.steps,
            tools_joined,
        ],
    )
    .map_err(|e| e.to_string())?;

    let run_count = job.run_count + 1;
    let mut next = next_run_after(job.kind_enum(), &job.schedule, now)?;
    let mut enabled = job.enabled;

    // 一次性任务跑过就结束。
    //
    // 不能只看 `next_run_after` 是否返回时间：一次性作业的时间点如果在"现在"
    // 之后（比如任务被提前执行），算出来的下一次仍是那个未来时刻，于是它会
    // 在原本排定的时间再跑一遍——"只跑一次"的语义就破了。
    if job.kind_enum() == JobKind::Once {
        next = None;
    }

    // 次数上限：到了就停用，而不是继续跑
    if let Some(limit) = job.max_runs {
        if run_count >= limit {
            enabled = false;
        }
    }
    // 算不出下一次也停用，避免每轮都被当成到期
    if next.is_none() {
        enabled = false;
    }

    conn.execute(
        "UPDATE scheduled_jobs SET enabled = ?2, next_run_at = ?3, last_run_at = ?4, \
         run_count = ?5, last_status = ?6, last_summary = ?7, last_error = ?8, updated_at = ?4 \
         WHERE id = ?1",
        rusqlite::params![
            job.id,
            enabled as i64,
            next.map(fmt_time),
            now_text,
            run_count,
            outcome.status,
            outcome.summary,
            outcome.error,
        ],
    )
    .map_err(|e| e.to_string())?;

    Ok(())
}

pub fn recent_runs(
    conn: &rusqlite::Connection,
    job_id: Option<&str>,
    limit: i64,
) -> Result<Vec<JobRun>, String> {
    let base = "SELECT id, job_id, session_id, started_at, finished_at, status, summary, error, \
                steps, tools_used FROM scheduled_job_runs";
    let (sql, param): (String, Option<String>) = match job_id {
        Some(id) => (
            format!("{} WHERE job_id = ?1 ORDER BY started_at DESC LIMIT {}", base, limit.clamp(1, 200)),
            Some(id.to_string()),
        ),
        None => (
            format!("{} ORDER BY started_at DESC LIMIT {}", base, limit.clamp(1, 200)),
            None,
        ),
    };

    let mut stmt = conn.prepare(&sql).map_err(|e| e.to_string())?;
    let map_row = |row: &rusqlite::Row<'_>| -> rusqlite::Result<JobRun> {
        let tools: Option<String> = row.get(9)?;
        Ok(JobRun {
            id: row.get(0)?,
            job_id: row.get(1)?,
            session_id: row.get(2)?,
            started_at: row.get(3)?,
            finished_at: row.get(4)?,
            status: row.get(5)?,
            summary: row.get(6)?,
            error: row.get(7)?,
            steps: row.get(8)?,
            tools_used: tools
                .map(|text| {
                    text.split(',')
                        .map(str::trim)
                        .filter(|s| !s.is_empty())
                        .map(str::to_string)
                        .collect()
                })
                .unwrap_or_default(),
        })
    };

    let rows = match param {
        Some(id) => stmt.query_map([id], map_row).map_err(|e| e.to_string())?,
        None => stmt.query_map([], map_row).map_err(|e| e.to_string())?,
    };

    rows.collect::<Result<Vec<_>, _>>().map_err(|e| e.to_string())
}

// ============================================================
// 调度器
// ============================================================

#[derive(Debug, Clone, Serialize)]
pub struct SchedulerStatus {
    pub running: bool,
    pub ticks: u64,
    pub runs: u64,
    pub last_tick: Option<String>,
    pub in_flight: Vec<String>,
}

pub struct Scheduler {
    app_handle: AppHandle,
    running: Arc<AtomicBool>,
    ticks: Arc<AtomicU64>,
    runs: Arc<AtomicU64>,
    last_tick: Arc<Mutex<Option<String>>>,
    in_flight: Arc<Mutex<HashSet<String>>>,
}

impl Scheduler {
    pub fn new(app_handle: AppHandle) -> Self {
        Self {
            app_handle,
            running: Arc::new(AtomicBool::new(false)),
            ticks: Arc::new(AtomicU64::new(0)),
            runs: Arc::new(AtomicU64::new(0)),
            last_tick: Arc::new(Mutex::new(None)),
            in_flight: Arc::new(Mutex::new(HashSet::new())),
        }
    }

    pub fn start(&self) {
        if self.running.swap(true, Ordering::SeqCst) {
            return;
        }

        let app_handle = self.app_handle.clone();
        let running = self.running.clone();
        let ticks = self.ticks.clone();
        let runs = self.runs.clone();
        let last_tick = self.last_tick.clone();
        let in_flight = self.in_flight.clone();

        tauri::async_runtime::spawn(async move {
            eprintln!("[Scheduler] started");

            while running.load(Ordering::SeqCst) {
                // 每轮现读配置：用户在设置页改完，不用重启就生效
                let enabled = {
                    let state = app_handle.state::<crate::AppState>();
                    // try_lock 拿不到就按"启用"处理：调度器本身不该因为一次
                    // 锁竞争而整体停摆，真要停用是用户显式改配置。
                    let resolved = match state.db.try_lock() {
                        Ok(db) => db
                            .get_setting("agent.scheduler_enabled")
                            .ok()
                            .flatten()
                            .map(|v| !matches!(v.trim().to_lowercase().as_str(), "false" | "0" | "off"))
                            .unwrap_or(true),
                        Err(_) => true,
                    };
                    resolved
                };

                let tick_secs = {
                    let state = app_handle.state::<crate::AppState>();
                    let resolved = match state.db.try_lock() {
                        Ok(db) => db
                            .get_setting("agent.scheduler_tick_sec")
                            .ok()
                            .flatten()
                            .and_then(|v| v.trim().parse::<u64>().ok())
                            .map(|v| v.clamp(5, 300))
                            .unwrap_or(30),
                        Err(_) => 30,
                    };
                    resolved
                };

                tokio::time::sleep(Duration::from_secs(tick_secs)).await;
                if !running.load(Ordering::SeqCst) {
                    break;
                }
                if !enabled {
                    continue;
                }

                ticks.fetch_add(1, Ordering::Relaxed);
                let now = now_string();
                *last_tick.lock().await = Some(now.clone());

                let due = {
                    let state = app_handle.state::<crate::AppState>();
                    let db = state.db.lock().await;
                    due_jobs(db.connection(), &now).unwrap_or_default()
                };

                for job in due {
                    if in_flight.lock().await.contains(&job.id) {
                        continue;
                    }
                    in_flight.lock().await.insert(job.id.clone());

                    let app = app_handle.clone();
                    let in_flight = in_flight.clone();
                    let runs = runs.clone();

                    tauri::async_runtime::spawn(async move {
                        if let Err(e) = run_job(&app, &job).await {
                            eprintln!("[Scheduler] 作业 '{}' 执行异常: {}", job.name, e);
                        }
                        in_flight.lock().await.remove(&job.id);
                        runs.fetch_add(1, Ordering::Relaxed);
                    });
                }
            }

            eprintln!("[Scheduler] stopped");
        });
    }

    pub fn stop(&self) {
        self.running.store(false, Ordering::SeqCst);
    }

    pub async fn status(&self) -> SchedulerStatus {
        SchedulerStatus {
            running: self.running.load(Ordering::SeqCst),
            ticks: self.ticks.load(Ordering::Relaxed),
            runs: self.runs.load(Ordering::Relaxed),
            last_tick: self.last_tick.lock().await.clone(),
            in_flight: self.in_flight.lock().await.iter().cloned().collect(),
        }
    }
}

/// 无人值守的提示词前缀。
///
/// 必须显式告诉模型"没人看着"：否则它会写出"我这就打开弹窗请你确认"这种
/// 在无人值守场景下等价于"什么都做不了"的回复，用户第二天只看到一句道歉。
pub fn build_autonomous_goal(job: &ScheduledJob) -> String {
    format!(
        "【无人值守定时任务】{name}（{cadence}）\n\
         你正在后台自动执行，没有人在旁边确认操作。规则：\n\
         1. 只能用不需要确认的工具；需要确认的操作会被直接拒绝，请换一条能走通的路。\n\
         2. 不要反问、不要请求许可、不要输出「是否需要我…」。\n\
         3. 完成后用一段话汇报结论：做了什么、结果如何、有没有未完成项。\n\
         4. 如果确实做不到，直接说清卡在哪一步，不要编造完成。\n\n\
         任务目标：{goal}",
        name = job.name,
        cadence = describe_schedule(job.kind_enum(), &job.schedule),
        goal = job.goal,
    )
}

/// 执行一个作业：跑一次完整 Agent 循环，落库，必要时通知。
async fn run_job(app_handle: &AppHandle, job: &ScheduledJob) -> Result<(), String> {
    let state = app_handle.try_state::<crate::AppState>();
    let state = match state {
        Some(s) => s,
        None => return Err("AppState 不可用".to_string()),
    };

    let session_id = format!("{}{}", AUTONOMOUS_SESSION_PREFIX, job.id);

    // 1. 备好会话与运行记录
    let run_id = {
        let db = state.db.lock().await;
        ensure_session(db.connection(), &session_id, &format!("定时任务：{}", job.name))?;
        start_run(db.connection(), &job.id, &session_id)?
    };

    // 2. 发起 Agent 循环
    let goal = build_autonomous_goal(job);
    let started = tokio::time::Instant::now();
    let timeout_secs = {
        let db = state.db.lock().await;
        db.get_setting("agent.scheduler_job_timeout_sec")
            .ok()
            .flatten()
            .and_then(|v| v.trim().parse::<u64>().ok())
            .map(|v| v.clamp(60, 1800))
            .unwrap_or(600)
    };

    let run = {
        let agent = state.agent.lock().await;
        agent
            .send_message(&session_id, &goal, None, None, &state.db)
            .await
    };

    let outcome = match run {
        Ok(agent_run) => drain_agent_run(agent_run.events, timeout_secs).await,
        Err(e) => RunOutcome {
            status: "failed".to_string(),
            summary: None,
            error: Some(format!("发起失败：{}", e)),
            steps: 0,
            tools_used: Vec::new(),
        },
    };

    let duration_ms = started.elapsed().as_millis() as i64;

    // 3. 落库
    {
        let db = state.db.lock().await;
        finish_run(db.connection(), job, &run_id, &outcome)?;
    }

    eprintln!(
        "[Scheduler] '{}' {}（{}ms，{} 步）",
        job.name,
        outcome.status,
        duration_ms,
        outcome.steps
    );

    // 4. 只在失败时弹系统通知：成功的任务天天弹等于训练用户忽略通知
    if outcome.status != "ok" {
        notify_failure(app_handle, job, &outcome);
    }

    let _ = app_handle.emit(
        "scheduler:job_finished",
        serde_json::json!({
            "job_id": job.id,
            "job_name": job.name,
            "run_id": run_id,
            "status": outcome.status,
            "summary": outcome.summary,
            "error": outcome.error,
            "steps": outcome.steps,
            "tools_used": outcome.tools_used,
            "duration_ms": duration_ms,
            "timestamp": chrono::Local::now().to_rfc3339(),
        }),
    );

    Ok(())
}

fn notify_failure(app_handle: &AppHandle, job: &ScheduledJob, outcome: &RunOutcome) {
    use tauri_plugin_notification::NotificationExt;

    let message = outcome
        .error
        .clone()
        .or_else(|| outcome.summary.clone())
        .unwrap_or_else(|| "未产出结果".to_string());

    let _ = app_handle
        .notification()
        .builder()
        .title(format!("定时任务未完成：{}", job.name))
        .body(truncate_chars(&message, 160))
        .show();
}

/// 把 Agent 事件流抽干，得到可存库的摘要。
///
/// 超时是必须的：无人值守的循环没人点"取消"，一个卡住的调用会让作业永远占着
/// in_flight，后续每轮都被跳过，看起来像是"调度器挂了"。
pub(crate) async fn drain_agent_run(
    mut events: tokio::sync::mpsc::Receiver<ChatEvent>,
    timeout_secs: u64,
) -> RunOutcome {    let deadline = tokio::time::Instant::now() + Duration::from_secs(timeout_secs);
    let mut text = String::new();
    let mut tools: Vec<String> = Vec::new();
    let mut error: Option<String> = None;
    let mut done = false;

    loop {
        match tokio::time::timeout_at(deadline, events.recv()).await {
            Ok(Some(ChatEvent::Text(chunk))) => text.push_str(&chunk),
            Ok(Some(ChatEvent::ToolCall { name, .. })) => {
                if !tools.iter().any(|t| t == &name) {
                    tools.push(name);
                }
            }
            Ok(Some(ChatEvent::Error(e))) => error = Some(e),
            Ok(Some(ChatEvent::Done)) => {
                done = true;
                break;
            }
            // 其余事件（Thinking / ToolResult / TaskUpdate / ...）不参与摘要
            Ok(Some(_)) => {}
            Ok(None) => break,
            Err(_) => {
                error = Some(format!("执行超过 {} 秒，已中止", timeout_secs));
                break;
            }
        }
    }

    let status = if error.is_some() {
        "failed"
    } else if done {
        "ok"
    } else {
        // 通道关了却没有 Done：属于异常收尾，不能记成功
        if error.is_none() {
            error = Some("事件流意外结束，未收到完成信号".to_string());
        }
        "failed"
    };

    let summary = if text.trim().is_empty() {
        None
    } else {
        Some(truncate_chars(text.trim(), 4000))
    };

    RunOutcome {
        status: status.to_string(),
        summary,
        error,
        steps: tools.len() as i64,
        tools_used: tools,
    }
}

pub(crate) fn truncate_chars(text: &str, limit: usize) -> String {
    if text.chars().count() <= limit {
        return text.to_string();
    }
    let mut out: String = text.chars().take(limit).collect();
    out.push('…');
    out
}

// ============================================================
// 测试
// ============================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn at(text: &str) -> DateTime<Local> {
        Local
            .from_local_datetime(
                &NaiveDateTime::parse_from_str(text, "%Y-%m-%d %H:%M:%S").expect("测试时间"),
            )
            .single()
            .expect("本地时间")
    }

    #[test]
    fn interval_accepts_plain_seconds_minutes_hours() {
        assert_eq!(parse_interval_secs("90").unwrap(), 90);
        assert_eq!(parse_interval_secs("30m").unwrap(), 1800);
        assert_eq!(parse_interval_secs("2h").unwrap(), 7200);
        assert_eq!(parse_interval_secs("1d").unwrap(), 86_400);
    }

    #[test]
    fn interval_rejects_too_frequent_and_too_rare() {
        // 5 秒一次等于烧 token，直接拒
        assert!(parse_interval_secs("5").is_err());
        // 400 天超出上限
        assert!(parse_interval_secs("400d").is_err());
        assert!(parse_interval_secs("abc").is_err());
    }

    #[test]
    fn cron_parses_ranges_and_steps() {
        let spec = CronSpec::parse("*/15 9-18 * * 1-5").expect("合法 cron");
        assert_eq!(spec.minutes, vec![0, 15, 30, 45]);
        assert_eq!(spec.hours.len(), 10);
        assert_eq!(spec.days_of_week, vec![1, 2, 3, 4, 5]);
    }

    #[test]
    fn cron_rejects_malformed_expression() {
        assert!(CronSpec::parse("0 9 * *").is_err());
        assert!(CronSpec::parse("60 9 * * *").is_err());
        assert!(CronSpec::parse("0 9 * * 9").is_err());
    }

    #[test]
    fn cron_sunday_can_be_0_or_7() {
        let a = CronSpec::parse("0 9 * * 0").unwrap();
        let b = CronSpec::parse("0 9 * * 7").unwrap();
        assert_eq!(a.days_of_week, b.days_of_week);
    }

    #[test]
    fn next_cron_matches_same_day_later_time() {
        let spec = CronSpec::parse("0 9 * * *").unwrap();
        let next = spec.next_after(at("2026-10-19 08:30:00")).unwrap();
        assert_eq!(next.format("%Y-%m-%d %H:%M").to_string(), "2026-10-19 09:00");
    }

    #[test]
    fn next_cron_rolls_to_tomorrow_when_time_passed() {
        let spec = CronSpec::parse("0 9 * * *").unwrap();
        let next = spec.next_after(at("2026-10-19 10:00:00")).unwrap();
        assert_eq!(next.format("%Y-%m-%d %H:%M").to_string(), "2026-10-20 09:00");
    }

    #[test]
    fn weekday_only_cron_skips_weekend() {
        // 2026-10-23 是周五，只要工作日 09:00 → 下一次是 10-26（周一）
        let spec = CronSpec::parse("0 9 * * 1-5").unwrap();
        let next = spec.next_after(at("2026-10-23 10:00:00")).unwrap();
        assert_eq!(next.format("%Y-%m-%d %H:%M").to_string(), "2026-10-26 09:00");
    }

    #[test]
    fn impossible_cron_returns_none_instead_of_looping_forever() {
        // 2 月 30 日永远不存在
        let spec = CronSpec::parse("0 0 30 2 *").unwrap();
        assert!(spec.next_after(at("2026-01-01 00:00:00")).is_none());
    }

    #[test]
    fn shorthand_daily_and_weekly_expand() {
        assert_eq!(normalize_schedule(JobKind::Cron, "每天 09:00").unwrap(), "0 9 * * *");
        assert_eq!(normalize_schedule(JobKind::Cron, "每天 9点").unwrap(), "0 9 * * *");
        assert_eq!(normalize_schedule(JobKind::Cron, "每周一 08:30").unwrap(), "30 8 * * 1");
        assert_eq!(normalize_schedule(JobKind::Cron, "每小时").unwrap(), "0 * * * *");
        assert_eq!(normalize_schedule(JobKind::Cron, "0 9 * * 1-5").unwrap(), "0 9 * * 1-5");
    }

    #[test]
    fn once_accepts_datetime_and_date_only() {
        let normalized = normalize_schedule(JobKind::Once, "2026-12-01 07:30").unwrap();
        assert_eq!(normalized, "2026-12-01 07:30:00");

        // 只给日期时默认早上 9 点，这是"那天提醒我"最合理的默认
        let date_only = normalize_schedule(JobKind::Once, "2026-12-01").unwrap();
        assert_eq!(date_only, "2026-12-01 09:00:00");
    }

    #[test]
    fn once_in_the_past_has_no_next_run() {
        let from = at("2026-10-19 12:00:00");
        let next = next_run_after(JobKind::Once, "2026-10-19 11:00:00", from).unwrap();
        assert!(next.is_none());
    }

    #[test]
    fn interval_next_run_is_relative_to_now() {
        let from = at("2026-10-19 12:00:00");
        let next = next_run_after(JobKind::Interval, "3600", from).unwrap().unwrap();
        assert_eq!(next.format("%H:%M").to_string(), "13:00");
    }

    #[test]
    fn autonomous_goal_states_that_nobody_is_watching() {
        let job = ScheduledJob {
            id: "j1".to_string(),
            name: "晨间简报".to_string(),
            kind: "cron".to_string(),
            schedule: "0 9 * * *".to_string(),
            goal: "汇总昨天的待办".to_string(),
            enabled: true,
            next_run_at: None,
            last_run_at: None,
            run_count: 0,
            max_runs: None,
            last_status: None,
            last_summary: None,
            last_error: None,
            created_at: "2026-10-19 12:00:00".to_string(),
            updated_at: "2026-10-19 12:00:00".to_string(),
        };
        let goal = build_autonomous_goal(&job);
        assert!(goal.contains("无人值守"));
        assert!(goal.contains("汇总昨天的待办"));
        assert!(goal.contains("不要反问"));
    }

    #[test]
    fn truncate_keeps_char_boundary() {
        // 中文按字符截断，不能切出半个 UTF-8 字节
        let text = "一二三四五";
        assert_eq!(truncate_chars(text, 3), "一二三…");
        assert_eq!(truncate_chars(text, 10), "一二三四五");
    }

    // ------------------------------------------------------------
    // 事件流抽取：无人值守的"结果"全靠这里从事件里捞出来
    // ------------------------------------------------------------

    #[tokio::test]
    async fn drain_collects_summary_and_tool_steps() {
        let (tx, rx) = tokio::sync::mpsc::channel(8);
        tx.send(ChatEvent::Thinking).await.unwrap();
        tx.send(ChatEvent::ToolCall {
            name: "disk_info".to_string(),
            arguments: serde_json::json!({}),
        })
        .await
        .unwrap();
        tx.send(ChatEvent::ToolResult {
            name: "disk_info".to_string(),
            success: true,
            content: "ok".to_string(),
        })
        .await
        .unwrap();
        // 同名工具调两次只算一步
        tx.send(ChatEvent::ToolCall {
            name: "disk_info".to_string(),
            arguments: serde_json::json!({}),
        })
        .await
        .unwrap();
        tx.send(ChatEvent::Text("磁盘还剩 12GB".to_string())).await.unwrap();
        tx.send(ChatEvent::Done).await.unwrap();
        drop(tx);

        let outcome = drain_agent_run(rx, 5).await;
        assert_eq!(outcome.status, "ok");
        assert_eq!(outcome.summary.as_deref(), Some("磁盘还剩 12GB"));
        assert_eq!(outcome.steps, 1);
        assert_eq!(outcome.tools_used, vec!["disk_info".to_string()]);
        assert!(outcome.error.is_none());
    }

    #[tokio::test]
    async fn drain_treats_error_event_as_failure() {
        let (tx, rx) = tokio::sync::mpsc::channel(4);
        tx.send(ChatEvent::Error("模型不可用".to_string())).await.unwrap();
        drop(tx);

        let outcome = drain_agent_run(rx, 5).await;
        assert_eq!(outcome.status, "failed");
        assert_eq!(outcome.error.as_deref(), Some("模型不可用"));
    }

    #[tokio::test]
    async fn drain_treats_silent_close_as_failure() {
        // 通道关闭但没收到 Done：不能记成功，否则用户以为任务做完了
        let (tx, rx) = tokio::sync::mpsc::channel::<ChatEvent>(1);
        drop(tx);

        let outcome = drain_agent_run(rx, 5).await;
        assert_eq!(outcome.status, "failed");
        assert!(outcome.error.is_some(), "必须说明异常收尾的原因");
    }

    #[tokio::test]
    async fn drain_times_out_instead_of_hanging_forever() {
        let (tx, rx) = tokio::sync::mpsc::channel::<ChatEvent>(1);
        // 一个永不发送的通道 + 0 秒超时：必须立刻返回失败而不是卡住
        let outcome = drain_agent_run(rx, 0).await;
        assert_eq!(outcome.status, "failed");
        assert!(
            outcome.error.as_deref().unwrap_or("").contains("超"),
            "应说明是超时中止"
        );
        drop(tx);
    }

    // ------------------------------------------------------------
    // 落库回环：建作业 → 到期 → 执行 → 续排/停用
    // ------------------------------------------------------------

    fn fresh_db(tag: &str) -> (crate::db::Database, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!(
            "pcg-sched-test-{}-{}",
            tag,
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("创建临时目录");
        let db = crate::db::Database::new(&dir.join("t.db")).expect("打开数据库");
        db.init_schema().expect("初始化 schema");
        (db, dir)
    }

    #[test]
    fn interval_job_runs_then_reschedules() {
        let (db, dir) = fresh_db("interval");
        let conn = db.connection();

        let job = insert_job(
            conn,
            NewJob {
                name: "半小时巡检",
                kind: JobKind::Interval,
                schedule: "1800",
                goal: "看一眼磁盘占用",
                enabled: true,
                max_runs: None,
            },
        )
        .expect("建作业");

        // 刚建好时不该到期
        assert!(
            due_jobs(conn, &now_string()).expect("查到期").is_empty(),
            "next_run_at 在 30 分钟后，此刻不该被取走"
        );

        // 手动提前到期 → 应被取走
        mark_due_now(conn, &job).expect("标记到期");
        let due = due_jobs(conn, &now_string()).expect("查到期");
        assert_eq!(due.len(), 1, "标记到期后应被调度器取到");
        assert_eq!(due[0].id, job.id);

        // 执行完 → 续排下一次
        let run_id = start_run(conn, &job.id, "autonomous:test").expect("写运行记录");
        finish_run(
            conn,
            &job,
            &run_id,
            &RunOutcome {
                status: "ok".to_string(),
                summary: Some("磁盘还剩 12GB".to_string()),
                error: None,
                steps: 2,
                tools_used: vec!["disk_info".to_string(), "read_file".to_string()],
            },
        )
        .expect("收尾");

        let after = get_job(conn, &job.id).expect("读回").expect("存在");
        assert_eq!(after.run_count, 1);
        assert_eq!(after.last_status.as_deref(), Some("ok"));
        assert!(after.enabled, "间隔任务不该跑完就停");
        assert!(
            after.next_run_at.as_deref().unwrap_or("") > now_string().as_str(),
            "下一次执行时间应被推到未来"
        );

        let runs = recent_runs(conn, Some(&job.id), 10).expect("查历史");
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].status, "ok");
        assert_eq!(runs[0].steps, 2);
        assert_eq!(runs[0].tools_used, vec!["disk_info", "read_file"]);
        assert!(runs[0].finished_at.is_some(), "完成时间必须落库");

        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn one_shot_job_disables_itself_after_running() {
        let (db, dir) = fresh_db("once");
        let conn = db.connection();
        let future = fmt_time(Local::now() + ChronoDuration::hours(1));

        let job = insert_job(
            conn,
            NewJob {
                name: "一小时后提醒",
                kind: JobKind::Once,
                schedule: &future,
                goal: "提醒我开会",
                enabled: true,
                max_runs: None,
            },
        )
        .expect("建一次性作业");

        mark_due_now(conn, &job).expect("标记到期");
        let run_id = start_run(conn, &job.id, "autonomous:test").expect("写运行记录");
        finish_run(conn, &job, &run_id, &RunOutcome {
            status: "ok".to_string(),
            summary: Some("已提醒".to_string()),
            error: None,
            steps: 0,
            tools_used: vec![],
        })
        .expect("收尾");

        let after = get_job(conn, &job.id).expect("读回").expect("存在");
        assert!(!after.enabled, "一次性任务跑完必须自动停用");
        assert!(after.next_run_at.is_none(), "没有下一次就不该留排期");

        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn max_runs_is_a_hard_stop() {
        let (db, dir) = fresh_db("maxruns");
        let conn = db.connection();

        let job = insert_job(
            conn,
            NewJob {
                name: "跑三次就停",
                kind: JobKind::Interval,
                schedule: "3600",
                goal: "随便做点什么",
                enabled: true,
                max_runs: Some(1),
            },
        )
        .expect("建作业");

        mark_due_now(conn, &job).expect("标记到期");
        let run_id = start_run(conn, &job.id, "autonomous:test").expect("写运行记录");
        finish_run(conn, &job, &run_id, &RunOutcome {
            status: "ok".to_string(),
            summary: None,
            error: None,
            steps: 0,
            tools_used: vec![],
        })
        .expect("收尾");

        let after = get_job(conn, &job.id).expect("读回").expect("存在");
        assert_eq!(after.run_count, 1);
        assert!(!after.enabled, "达到 max_runs 必须停用，避免一直烧 token");

        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn failed_run_is_recorded_with_error_and_keeps_job_alive() {
        let (db, dir) = fresh_db("failed");
        let conn = db.connection();

        let job = insert_job(
            conn,
            NewJob {
                name: "会失败的任务",
                kind: JobKind::Interval,
                schedule: "3600",
                goal: "做一件做不到的事",
                enabled: true,
                max_runs: None,
            },
        )
        .expect("建作业");

        mark_due_now(conn, &job).expect("标记到期");
        let run_id = start_run(conn, &job.id, "autonomous:test").expect("写运行记录");
        finish_run(conn, &job, &run_id, &RunOutcome {
            status: "failed".to_string(),
            summary: None,
            error: Some("模型不可用".to_string()),
            steps: 1,
            tools_used: vec!["client_state".to_string()],
        })
        .expect("收尾");

        let after = get_job(conn, &job.id).expect("读回").expect("存在");
        assert_eq!(after.last_status.as_deref(), Some("failed"));
        assert_eq!(after.last_error.as_deref(), Some("模型不可用"));
        assert!(after.enabled, "单次失败不该停用任务，下一轮应继续尝试");

        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn deleting_job_cascades_runs() {
        let (db, dir) = fresh_db("cascade");
        let conn = db.connection();

        let job = insert_job(
            conn,
            NewJob {
                name: "待删除",
                kind: JobKind::Interval,
                schedule: "3600",
                goal: "x",
                enabled: true,
                max_runs: None,
            },
        )
        .expect("建作业");
        start_run(conn, &job.id, "autonomous:test").expect("写运行记录");

        assert!(delete_job(conn, &job.id).expect("删除"));
        let runs = recent_runs(conn, Some(&job.id), 10).expect("查历史");
        assert!(runs.is_empty(), "删任务应连带清掉运行历史（外键 ON DELETE CASCADE）");

        let _ = std::fs::remove_dir_all(dir);
    }
}

