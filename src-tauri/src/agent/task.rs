//! Agent 任务状态管理
//!
//! 定义 Task 状态机、Task 数据结构、TaskManager。
//! 每个用户请求创建一个 Task，持久化到 SQLite，支持中断后恢复。

use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};

use crate::error::AppResult;

// ============================================================
// 状态枚举
// ============================================================

/// Task 状态
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum TaskStatus {
    /// 刚创建，等待执行
    Pending,
    /// 分析用户意图
    Analyzing,
    /// 生成执行计划
    Planning,
    /// 执行中
    Executing,
    /// 验证结果
    Verifying,
    /// 已完成
    Completed,
    /// 执行失败
    Failed,
    /// 已取消
    Cancelled,
}

impl TaskStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            TaskStatus::Pending => "PENDING",
            TaskStatus::Analyzing => "ANALYZING",
            TaskStatus::Planning => "PLANNING",
            TaskStatus::Executing => "EXECUTING",
            TaskStatus::Verifying => "VERIFYING",
            TaskStatus::Completed => "COMPLETED",
            TaskStatus::Failed => "FAILED",
            TaskStatus::Cancelled => "CANCELLED",
        }
    }

    /// 是否为终态
    pub fn is_terminal(&self) -> bool {
        matches!(self, TaskStatus::Completed | TaskStatus::Failed | TaskStatus::Cancelled)
    }

    /// 是否可执行
    pub fn is_active(&self) -> bool {
        !self.is_terminal()
    }
}

impl std::str::FromStr for TaskStatus {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "PENDING" => Ok(TaskStatus::Pending),
            "ANALYZING" => Ok(TaskStatus::Analyzing),
            "PLANNING" => Ok(TaskStatus::Planning),
            "EXECUTING" => Ok(TaskStatus::Executing),
            "VERIFYING" => Ok(TaskStatus::Verifying),
            "COMPLETED" => Ok(TaskStatus::Completed),
            "FAILED" => Ok(TaskStatus::Failed),
            "CANCELLED" => Ok(TaskStatus::Cancelled),
            _ => Err(format!("Unknown task status: {}", s)),
        }
    }
}

// ============================================================
// 步骤枚举与结构
// ============================================================

/// 单步状态
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum StepStatus {
    /// 等待执行
    Pending,
    /// 执行中
    Executing,
    /// 已完成
    Completed,
    /// 失败
    Failed,
    /// 跳过
    Skipped,
}

impl StepStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            StepStatus::Pending => "pending",
            StepStatus::Executing => "executing",
            StepStatus::Completed => "completed",
            StepStatus::Failed => "failed",
            StepStatus::Skipped => "skipped",
        }
    }
}

/// 执行步骤
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskStep {
    pub step_id: usize,
    pub action: String,
    pub status: StepStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_arguments: Option<serde_json::Value>,
    /// V21: 恢复尝试次数（持久化，崩溃后不丢失）
    #[serde(default)]
    pub recovery_attempts: usize,
    /// V21: 最后一次恢复策略名称
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_recovery_strategy: Option<String>,
}

impl TaskStep {
    pub fn new(step_id: usize, action: &str) -> Self {
        Self {
            step_id,
            action: action.to_string(),
            status: StepStatus::Pending,
            result: None,
            tool_name: None,
            tool_arguments: None,
            recovery_attempts: 0,
            last_recovery_strategy: None,
        }
    }

    /// V21: 记录一次恢复尝试
    pub fn record_recovery(&mut self, strategy: &str) {
        self.recovery_attempts += 1;
        self.last_recovery_strategy = Some(strategy.to_string());
    }
}

// ============================================================
// 观察与错误记录
// ============================================================

/// 观察记录
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Observation {
    pub step_id: usize,
    pub result: String,
    pub timestamp: String,
}

/// 错误记录
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskError {
    pub step_id: usize,
    pub error: String,
    pub recovery_attempts: usize,
    pub timestamp: String,
}

// ============================================================
// Task 结构体
// ============================================================

/// Agent 任务
///
/// 一个 Task 对应一次用户请求，包含完整执行上下文。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Task {
    pub id: String,
    pub session_id: String,
    pub goal: String,
    pub status: TaskStatus,
    pub plan: Vec<TaskStep>,
    pub current_step: usize,
    pub observations: Vec<Observation>,
    pub errors: Vec<TaskError>,
    pub completed_steps: Vec<usize>,
    pub created_at: String,
    pub updated_at: String,
    /// 是否简单任务（跳过 PLAN 阶段）
    #[serde(default)]
    pub is_simple: bool,
    /// 最终总结
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
}

impl Task {
    /// 创建新任务
    pub fn new(session_id: &str, goal: &str) -> Self {
        let now = chrono::Local::now().to_rfc3339();
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            session_id: session_id.to_string(),
            goal: goal.to_string(),
            status: TaskStatus::Pending,
            plan: Vec::new(),
            current_step: 0,
            observations: Vec::new(),
            errors: Vec::new(),
            completed_steps: Vec::new(),
            created_at: now.clone(),
            updated_at: now,
            is_simple: false,
            summary: None,
        }
    }

    /// 更新状态
    pub fn set_status(&mut self, status: TaskStatus) {
        self.status = status;
        self.updated_at = chrono::Local::now().to_rfc3339();
    }

    /// 设置计划
    pub fn set_plan(&mut self, steps: Vec<TaskStep>) {
        self.plan = steps;
        self.updated_at = chrono::Local::now().to_rfc3339();
    }

    /// 标记当前步骤为执行中
    pub fn start_step(&mut self, step_id: usize) {
        self.current_step = step_id;
        if let Some(step) = self.plan.iter_mut().find(|s| s.step_id == step_id) {
            step.status = StepStatus::Executing;
        }
        self.set_status(TaskStatus::Executing);
    }

    /// 完成当前步骤
    pub fn complete_step(&mut self, step_id: usize, result: &str) {
        if let Some(step) = self.plan.iter_mut().find(|s| s.step_id == step_id) {
            step.status = StepStatus::Completed;
            step.result = Some(result.to_string());
        }
        self.completed_steps.push(step_id);
        self.observations.push(Observation {
            step_id,
            result: result.to_string(),
            timestamp: chrono::Local::now().to_rfc3339(),
        });
        self.updated_at = chrono::Local::now().to_rfc3339();
    }

    /// 标记步骤失败
    pub fn fail_step(&mut self, step_id: usize, error: &str) {
        if let Some(step) = self.plan.iter_mut().find(|s| s.step_id == step_id) {
            step.status = StepStatus::Failed;
            step.result = Some(format!("错误: {}", error));
        }
        self.errors.push(TaskError {
            step_id,
            error: error.to_string(),
            recovery_attempts: 0,
            timestamp: chrono::Local::now().to_rfc3339(),
        });
        self.updated_at = chrono::Local::now().to_rfc3339();
    }

    /// 获取总步数
    pub fn total_steps(&self) -> usize {
        self.plan.len()
    }

    /// 获取已完成步数
    pub fn completed_count(&self) -> usize {
        self.completed_steps.len()
    }

    /// 获取当前步骤（如果存在）
    pub fn current_step_ref(&self) -> Option<&TaskStep> {
        self.plan.iter().find(|s| s.step_id == self.current_step)
    }

    /// 获取进度百分比
    pub fn progress_percent(&self) -> u8 {
        if self.plan.is_empty() {
            return 0;
        }
        let completed = self.completed_count();
        ((completed as f32 / self.plan.len() as f32) * 100.0) as u8
    }

    /// 是否所有步骤都已完成
    pub fn all_steps_completed(&self) -> bool {
        self.plan.iter().all(|s| matches!(s.status, StepStatus::Completed | StepStatus::Skipped))
    }

    /// 获取下一步（未完成的第一个步骤）
    pub fn next_pending_step(&self) -> Option<&TaskStep> {
        self.plan.iter().find(|s| matches!(s.status, StepStatus::Pending))
    }
}

// ============================================================
// TaskManager
// ============================================================

/// 任务管理器
///
/// 负责 Task 的创建、更新、查询、持久化。
pub struct TaskManager<'a> {
    conn: &'a Connection,
}

impl<'a> TaskManager<'a> {
    pub fn new(conn: &'a Connection) -> Self {
        Self { conn }
    }

    /// 创建新任务
    pub fn create(&self, session_id: &str, goal: &str) -> AppResult<Task> {
        let task = Task::new(session_id, goal);
        let plan_json = serde_json::to_string(&task.plan)?;
        let observations_json = serde_json::to_string(&task.observations)?;
        let errors_json = serde_json::to_string(&task.errors)?;
        let completed_json = serde_json::to_string(&task.completed_steps)?;

        self.conn.execute(
            "INSERT INTO agent_tasks (
                id, session_id, goal, status, plan_json, current_step,
                observations_json, errors_json, completed_steps_json,
                is_simple, summary, created_at, updated_at
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
            params![
                &task.id,
                &task.session_id,
                &task.goal,
                task.status.as_str(),
                &plan_json,
                task.current_step as i64,
                &observations_json,
                &errors_json,
                &completed_json,
                task.is_simple as i64,
                &task.summary,
                &task.created_at,
                &task.updated_at,
            ],
        )?;

        Ok(task)
    }

    /// 更新任务状态
    pub fn update(&self, task: &Task) -> AppResult<()> {
        let plan_json = serde_json::to_string(&task.plan)?;
        let observations_json = serde_json::to_string(&task.observations)?;
        let errors_json = serde_json::to_string(&task.errors)?;
        let completed_json = serde_json::to_string(&task.completed_steps)?;

        self.conn.execute(
            "UPDATE agent_tasks SET
                status = ?2,
                plan_json = ?3,
                current_step = ?4,
                observations_json = ?5,
                errors_json = ?6,
                completed_steps_json = ?7,
                is_simple = ?8,
                summary = ?9,
                updated_at = ?10
            WHERE id = ?1",
            params![
                &task.id,
                task.status.as_str(),
                &plan_json,
                task.current_step as i64,
                &observations_json,
                &errors_json,
                &completed_json,
                task.is_simple as i64,
                &task.summary,
                &task.updated_at,
            ],
        )?;

        Ok(())
    }

    /// 根据 ID 获取任务
    pub fn get(&self, id: &str) -> AppResult<Option<Task>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, session_id, goal, status, plan_json, current_step,
                    observations_json, errors_json, completed_steps_json,
                    is_simple, summary, created_at, updated_at
             FROM agent_tasks WHERE id = ?1"
        )?;

        let mut rows = stmt.query(params![id])?;

        if let Some(row) = rows.next()? {
            Ok(Some(self.row_to_task(row)?))
        } else {
            Ok(None)
        }
    }

    /// 获取会话的当前活跃任务
    pub fn get_active_by_session(&self, session_id: &str) -> AppResult<Option<Task>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, session_id, goal, status, plan_json, current_step,
                    observations_json, errors_json, completed_steps_json,
                    is_simple, summary, created_at, updated_at
             FROM agent_tasks
             WHERE session_id = ?1 AND status NOT IN ('COMPLETED', 'FAILED', 'CANCELLED')
             ORDER BY created_at DESC LIMIT 1"
        )?;

        let mut rows = stmt.query(params![session_id])?;

        if let Some(row) = rows.next()? {
            Ok(Some(self.row_to_task(row)?))
        } else {
            Ok(None)
        }
    }

    /// 获取会话的所有任务
    pub fn list_by_session(&self, session_id: &str, limit: i64) -> AppResult<Vec<Task>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, session_id, goal, status, plan_json, current_step,
                    observations_json, errors_json, completed_steps_json,
                    is_simple, summary, created_at, updated_at
             FROM agent_tasks
             WHERE session_id = ?1
             ORDER BY created_at DESC LIMIT ?2"
        )?;

        let tasks = stmt.query_map(params![session_id, limit], |row| {
            self.row_to_task(row).map_err(|e| rusqlite::Error::FromSqlConversionFailure(
                0,
                rusqlite::types::Type::Text,
                Box::new(e),
            ))
        })?;

        Ok(tasks.collect::<Result<Vec<_>, _>>()?)
    }

    /// 标记会话的所有活跃任务为已取消
    pub fn cancel_active_by_session(&self, session_id: &str) -> AppResult<()> {
        let now = chrono::Local::now().to_rfc3339();
        self.conn.execute(
            "UPDATE agent_tasks
             SET status = 'CANCELLED', updated_at = ?2
             WHERE session_id = ?1 AND status NOT IN ('COMPLETED', 'FAILED', 'CANCELLED')",
            params![session_id, &now],
        )?;
        Ok(())
    }

    /// 应用启动时清理所有非终态任务（自愈：崩溃后重启不会卡在 executing）
    ///
    /// 将所有 PENDING/PLANNING/RUNNING/EXECUTING/VERIFYING/RECOVERING 状态的任务
    /// 标记为 FAILED，原因是"应用重启，任务已中断"。
    pub fn cleanup_stale_tasks(&self) -> AppResult<usize> {
        let now = chrono::Local::now().to_rfc3339();
        let affected = self.conn.execute(
            "UPDATE agent_tasks
             SET status = 'FAILED',
                 summary = '应用重启，任务已中断',
                 updated_at = ?1
             WHERE status NOT IN ('COMPLETED', 'FAILED', 'CANCELLED')",
            params![&now],
        )?;
        Ok(affected)
    }

    /// 将行数据转换为 Task
    fn row_to_task(&self, row: &rusqlite::Row) -> AppResult<Task> {
        let status_str: String = row.get(3)?;
        let status = status_str.parse().unwrap_or(TaskStatus::Pending);

        let plan_json: String = row.get(4)?;
        let plan: Vec<TaskStep> = serde_json::from_str(&plan_json).unwrap_or_default();

        let observations_json: String = row.get(6)?;
        let observations: Vec<Observation> = serde_json::from_str(&observations_json).unwrap_or_default();

        let errors_json: String = row.get(7)?;
        let errors: Vec<TaskError> = serde_json::from_str(&errors_json).unwrap_or_default();

        let completed_json: String = row.get(8)?;
        let completed_steps: Vec<usize> = serde_json::from_str(&completed_json).unwrap_or_default();

        Ok(Task {
            id: row.get(0)?,
            session_id: row.get(1)?,
            goal: row.get(2)?,
            status,
            plan,
            current_step: row.get::<_, i64>(5)? as usize,
            observations,
            errors,
            completed_steps,
            is_simple: row.get::<_, i64>(9)? != 0,
            summary: row.get(10)?,
            created_at: row.get(11)?,
            updated_at: row.get(12)?,
        })
    }
}

// ============================================================
// 任务更新事件（推送给前端）
// ============================================================

/// 任务状态更新事件 payload
#[derive(Debug, Clone, Serialize)]
pub struct TaskUpdateEvent {
    pub task_id: String,
    pub session_id: String,
    pub goal: String,
    pub status: String,
    pub current_step: usize,
    pub total_steps: usize,
    pub progress_percent: u8,
    pub current_action: Option<String>,
    pub is_simple: bool,
}

impl TaskUpdateEvent {
    pub fn from_task(task: &Task) -> Self {
        let current_action = task.current_step_ref().map(|s| s.action.clone());
        Self {
            task_id: task.id.clone(),
            session_id: task.session_id.clone(),
            goal: task.goal.clone(),
            status: task.status.as_str().to_string(),
            current_step: task.current_step,
            total_steps: task.total_steps(),
            progress_percent: task.progress_percent(),
            current_action,
            is_simple: task.is_simple,
        }
    }
}

/// Action 状态事件（Agent Loop 每步执行时推送）
#[derive(Debug, Clone, Serialize)]
pub struct ActionStatusEvent {
    pub task_id: String,
    pub step: usize,
    pub action: String,
    pub status: String,
    pub message: String,
}
