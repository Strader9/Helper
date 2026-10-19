//! 常驻调度工具（scheduler 域）
//!
//! 这一组工具让"每天早上九点帮我汇总昨天的工作"这类需求可以被对话直接落地：
//! 模型调 `schedule_job` 写一条作业，调度器到点起一次完整 Agent 循环，
//! 结果落在 `scheduled_job_runs` 里，用户第二天问"跑了没"时有据可查。
//!
//! 与 `create_task_plan` 的区别：那是**会话内**的一次性计划，做完就结束；
//! 作业是**跨会话、跨重启**的常驻任务，所以必须落库、必须记次数、必须能停。

use async_trait::async_trait;
use serde_json::{Value, json};

use crate::error::{AppError, AppResult};
use crate::scheduler::{
    self, JobKind, ScheduledJob, delete_job, describe_schedule, find_job, insert_job,
    list_jobs, mark_due_now, normalize_schedule, recent_runs, set_enabled,
};
use crate::tools::{AgentTool, RiskLevel, ToolResult};

fn open_db() -> AppResult<rusqlite::Connection> {
    crate::memory::open_connection()
        .ok_or_else(|| AppError::Internal("数据库尚未初始化".to_string()))
}

fn job_json(job: &ScheduledJob) -> Value {
    json!({
        "id": job.id,
        "name": job.name,
        "kind": job.kind,
        "schedule": job.schedule,
        "cadence": describe_schedule(job.kind_enum(), &job.schedule),
        "goal": job.goal,
        "enabled": job.enabled,
        "next_run_at": job.next_run_at,
        "last_run_at": job.last_run_at,
        "run_count": job.run_count,
        "max_runs": job.max_runs,
        "last_status": job.last_status,
        "last_summary": job.last_summary,
        "last_error": job.last_error,
    })
}

// ============================================================
// schedule_job
// ============================================================

pub struct ScheduleJobTool;

#[async_trait]
impl AgentTool for ScheduleJobTool {
    fn name(&self) -> &'static str {
        "schedule_job"
    }

    fn description(&self) -> &'static str {
        "创建一个常驻定时任务。用户说「每天早上九点帮我…」「每 2 小时检查…」「下周一提醒我…」时用。作业到点会起一次完整 Agent 循环自动完成目标。kind=interval 时 schedule 写 30m/2h/90；kind=cron 时写五段 cron 或「每天 09:00」「每周一 08:30」；kind=once 时写 YYYY-MM-DD HH:MM。"
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "name": { "type": "string", "description": "任务名，简短可辨认，如「晨间简报」" },
                "goal": { "type": "string", "description": "交给 Agent 完整执行的目标，写清楚要产出什么" },
                "kind": {
                    "type": "string",
                    "enum": ["interval", "cron", "once"],
                    "description": "interval=固定间隔，cron=按日程，once=一次性"
                },
                "schedule": { "type": "string", "description": "配合 kind 使用：30m / 0 9 * * 1-5 / 每天 09:00 / 2026-11-01 09:00" },
                "max_runs": { "type": "integer", "description": "可选：最多执行多少次，到次数自动停用。不填表示不限" }
            },
            "required": ["name", "goal", "kind", "schedule"]
        })
    }

    fn risk_level(&self) -> RiskLevel {
        RiskLevel::Medium
    }

    async fn execute(&self, args: Value) -> AppResult<ToolResult> {
        let name = args["name"]
            .as_str()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| AppError::InvalidArgument("name is required".to_string()))?;
        let goal = args["goal"]
            .as_str()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| AppError::InvalidArgument("goal is required".to_string()))?;
        let kind_text = args["kind"]
            .as_str()
            .ok_or_else(|| AppError::InvalidArgument("kind is required".to_string()))?;
        let kind = JobKind::from_str(kind_text)
            .ok_or_else(|| AppError::InvalidArgument(format!("未知 kind：{}", kind_text)))?;
        let raw_schedule = args["schedule"]
            .as_str()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| AppError::InvalidArgument("schedule is required".to_string()))?;

        let schedule = normalize_schedule(kind, raw_schedule)
            .map_err(|e| AppError::InvalidArgument(e.to_string()))?;

        let max_runs = args["max_runs"].as_i64().filter(|v| *v > 0);

        let conn = open_db()?;

        // 同名任务不重复建：模型重试一次就多一条作业，会变成双倍执行
        if let Some(existing) = find_job(&conn, name).map_err(AppError::Internal)? {
            if existing.name == name {
                return Ok(ToolResult::ok(json!({
                    "created": false,
                    "reason": "已有同名任务，未重复创建。要改配置请先 cancel_scheduled_job 再新建，或用 run_job_now 立刻跑一次",
                    "job": job_json(&existing),
                })));
            }
        }

        let job = insert_job(
            &conn,
            scheduler::NewJob {
                name,
                kind,
                schedule: &schedule,
                goal,
                enabled: true,
                max_runs,
            },
        )
        .map_err(|e| AppError::InvalidArgument(e))?;

        Ok(ToolResult::ok(json!({
            "created": true,
            "id": job.id,
            "name": job.name,
            "kind": job.kind,
            "schedule": job.schedule,
            "cadence": describe_schedule(job.kind_enum(), &job.schedule),
            "next_run_at": job.next_run_at,
            "max_runs": job.max_runs,
        })))
    }
}

// ============================================================
// list_scheduled_jobs
// ============================================================

pub struct ListScheduledJobsTool;

#[async_trait]
impl AgentTool for ListScheduledJobsTool {
    fn name(&self) -> &'static str {
        "list_scheduled_jobs"
    }

    fn description(&self) -> &'static str {
        "列出所有定时任务及其最近一次执行状态。用户问「我有哪些定时任务」「昨天的任务跑了没」时用。"
    }

    fn parameters(&self) -> Value {
        json!({ "type": "object", "properties": {} })
    }

    fn risk_level(&self) -> RiskLevel {
        RiskLevel::Safe
    }

    async fn execute(&self, _args: Value) -> AppResult<ToolResult> {
        let conn = open_db()?;
        let jobs = list_jobs(&conn).map_err(|e| AppError::Internal(e))?;
        let items: Vec<Value> = jobs.iter().map(job_json).collect();

        Ok(ToolResult::ok(json!({
            "count": items.len(),
            "enabled": jobs.iter().filter(|j| j.enabled).count(),
            "jobs": items,
        })))
    }
}

// ============================================================
// cancel_scheduled_job
// ============================================================

pub struct CancelScheduledJobTool;

#[async_trait]
impl AgentTool for CancelScheduledJobTool {
    fn name(&self) -> &'static str {
        "cancel_scheduled_job"
    }

    fn description(&self) -> &'static str {
        "删除一个定时任务（连同它的运行历史）。参数可给 id 或任务名。只是暂时不跑请用 set_job_enabled。"
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "id_or_name": { "type": "string", "description": "任务 id 或任务名" }
            },
            "required": ["id_or_name"]
        })
    }

    fn risk_level(&self) -> RiskLevel {
        RiskLevel::Medium
    }

    async fn execute(&self, args: Value) -> AppResult<ToolResult> {
        let key = args["id_or_name"]
            .as_str()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| AppError::InvalidArgument("id_or_name is required".to_string()))?;

        let conn = open_db()?;
        let job = find_job(&conn, key).map_err(|e| AppError::Internal(e))?;
        let Some(job) = job else {
            return Ok(ToolResult::ok(json!({
                "deleted": false,
                "reason": format!("没有找到任务「{}」", key),
            })));
        };

        let deleted = delete_job(&conn, &job.id).map_err(|e| AppError::Internal(e))?;

        Ok(ToolResult::ok(json!({
            "deleted": deleted,
            "id": job.id,
            "name": job.name,
        })))
    }
}

// ============================================================
// set_job_enabled
// ============================================================

pub struct SetJobEnabledTool;

#[async_trait]
impl AgentTool for SetJobEnabledTool {
    fn name(&self) -> &'static str {
        "set_job_enabled"
    }

    fn description(&self) -> &'static str {
        "启用或暂停一个定时任务（保留配置与历史）。用户说「先别跑了」「可以继续跑了」时用。"
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "id_or_name": { "type": "string", "description": "任务 id 或任务名" },
                "enabled": { "type": "boolean", "description": "true=启用，false=暂停" }
            },
            "required": ["id_or_name", "enabled"]
        })
    }

    fn risk_level(&self) -> RiskLevel {
        RiskLevel::Medium
    }

    async fn execute(&self, args: Value) -> AppResult<ToolResult> {
        let key = args["id_or_name"]
            .as_str()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| AppError::InvalidArgument("id_or_name is required".to_string()))?;
        let enabled = args["enabled"]
            .as_bool()
            .ok_or_else(|| AppError::InvalidArgument("enabled is required".to_string()))?;

        let conn = open_db()?;
        let job = find_job(&conn, key)
            .map_err(|e| AppError::Internal(e))?
            .ok_or_else(|| AppError::InvalidArgument(format!("没有找到任务「{}」", key)))?;

        set_enabled(&conn, &job.id, enabled).map_err(|e| AppError::Internal(e))?;

        // 重新读一次拿到最新的 next_run_at
        let updated = scheduler::get_job(&conn, &job.id)
            .map_err(|e| AppError::Internal(e))?
            .map(|j| job_json(&j));

        Ok(ToolResult::ok(json!({
            "id": job.id,
            "name": job.name,
            "enabled": enabled,
            "job": updated,
        })))
    }
}

// ============================================================
// run_job_now
// ============================================================

pub struct RunJobNowTool;

#[async_trait]
impl AgentTool for RunJobNowTool {
    fn name(&self) -> &'static str {
        "run_job_now"
    }

    fn description(&self) -> &'static str {
        "立刻执行一次定时任务（不等下一个排期）。调度器最长在一个检查周期（默认 30 秒）内接手。"
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "id_or_name": { "type": "string", "description": "任务 id 或任务名" }
            },
            "required": ["id_or_name"]
        })
    }

    fn risk_level(&self) -> RiskLevel {
        RiskLevel::Medium
    }

    async fn execute(&self, args: Value) -> AppResult<ToolResult> {
        let key = args["id_or_name"]
            .as_str()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| AppError::InvalidArgument("id_or_name is required".to_string()))?;

        let conn = open_db()?;
        let job = find_job(&conn, key)
            .map_err(|e| AppError::Internal(e))?
            .ok_or_else(|| AppError::InvalidArgument(format!("没有找到任务「{}」", key)))?;

        if !job.enabled {
            return Ok(ToolResult::ok(json!({
                "triggered": false,
                "reason": "该任务已暂停，先 set_job_enabled 启用再执行",
                "id": job.id,
                "name": job.name,
            })));
        }

        mark_due_now(&conn, &job).map_err(|e| AppError::Internal(e))?;

        Ok(ToolResult::ok(json!({
            "triggered": true,
            "id": job.id,
            "name": job.name,
            "note": "已标记为立即到期，调度器会在下一个检查周期内执行。结果用 job_run_history 查。",
        })))
    }
}

// ============================================================
// job_run_history
// ============================================================

pub struct JobRunHistoryTool;

#[async_trait]
impl AgentTool for JobRunHistoryTool {
    fn name(&self) -> &'static str {
        "job_run_history"
    }

    fn description(&self) -> &'static str {
        "查定时任务的执行历史（状态、摘要、用到的工具、耗时）。用户问「昨晚跑出了什么结果」时用。不传 id_or_name 则查全部任务的最近记录。"
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "id_or_name": { "type": "string", "description": "可选：任务 id 或任务名" },
                "limit": { "type": "integer", "description": "返回条数，默认 10，最多 50" }
            }
        })
    }

    fn risk_level(&self) -> RiskLevel {
        RiskLevel::Safe
    }

    async fn execute(&self, args: Value) -> AppResult<ToolResult> {
        let limit = args["limit"].as_i64().unwrap_or(10).clamp(1, 50);
        let conn = open_db()?;

        let job_id = match args["id_or_name"].as_str().map(str::trim).filter(|s| !s.is_empty()) {
            Some(key) => match find_job(&conn, key).map_err(|e| AppError::Internal(e))? {
                Some(job) => Some(job.id),
                None => {
                    return Ok(ToolResult::ok(json!({
                        "count": 0,
                        "runs": [],
                        "reason": format!("没有找到任务「{}」", key),
                    })))
                }
            },
            None => None,
        };

        let runs = recent_runs(&conn, job_id.as_deref(), limit).map_err(|e| AppError::Internal(e))?;

        Ok(ToolResult::ok(json!({
            "count": runs.len(),
            "runs": runs,
        })))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scheduler::{CronSpec, JobKind, next_run_after, parse_interval_secs};

    #[test]
    fn tool_names_are_stable() {
        // 名字是对外契约（模型按名调用），改动必须是有意识的
        assert_eq!(ScheduleJobTool.name(), "schedule_job");
        assert_eq!(ListScheduledJobsTool.name(), "list_scheduled_jobs");
        assert_eq!(CancelScheduledJobTool.name(), "cancel_scheduled_job");
        assert_eq!(SetJobEnabledTool.name(), "set_job_enabled");
        assert_eq!(RunJobNowTool.name(), "run_job_now");
        assert_eq!(JobRunHistoryTool.name(), "job_run_history");
    }

    #[test]
    fn risk_levels_match_authority() {
        // 读操作不该弹窗；写作业/删作业会带来持续性授权，必须确认
        assert_eq!(ListScheduledJobsTool.risk_level(), RiskLevel::Safe);
        assert_eq!(JobRunHistoryTool.risk_level(), RiskLevel::Safe);
        assert_eq!(ScheduleJobTool.risk_level(), RiskLevel::Medium);
        assert_eq!(CancelScheduledJobTool.risk_level(), RiskLevel::Medium);
        assert_eq!(SetJobEnabledTool.risk_level(), RiskLevel::Medium);
        assert_eq!(RunJobNowTool.risk_level(), RiskLevel::Medium);
    }

    #[test]
    fn parameters_require_core_fields() {
        let schema = ScheduleJobTool.parameters();
        let required = schema["required"].as_array().expect("required 数组");
        for field in ["name", "goal", "kind", "schedule"] {
            assert!(
                required.iter().any(|v| v.as_str() == Some(field)),
                "schedule_job 的必填项缺少 {}",
                field
            );
        }
    }

    #[test]
    fn job_kind_round_trip() {
        for kind in [JobKind::Interval, JobKind::Cron, JobKind::Once] {
            assert_eq!(JobKind::from_str(kind.as_str()), Some(kind));
        }
        assert_eq!(JobKind::from_str("nonsense"), None);
    }

    #[test]
    fn interval_schedule_survives_normalization() {
        let normalized = normalize_schedule(JobKind::Interval, "30m").unwrap();
        assert_eq!(normalized, "1800");
        assert_eq!(parse_interval_secs(&normalized).unwrap(), 1800);
    }

    #[test]
    fn cron_schedule_is_accepted_verbatim() {
        let normalized = normalize_schedule(JobKind::Cron, "0 9 * * 1-5").unwrap();
        assert_eq!(normalized, "0 9 * * 1-5");
        assert!(CronSpec::parse(&normalized).is_ok());
    }

    #[test]
    fn once_schedule_reports_next_run_in_future() {
        let now = chrono::Local::now();
        let future = (now + chrono::Duration::days(2)).format("%Y-%m-%d %H:%M").to_string();
        let normalized = normalize_schedule(JobKind::Once, &future).unwrap();
        let next = next_run_after(JobKind::Once, &normalized, now).unwrap();
        assert!(next.is_some(), "未来时刻应当有下一次执行");
    }
}
