//! 观察模块
//!
//! Observation 是 Agent 对工具执行结果和系统状态的结构化记录。
//! 每个工具执行后都会产生一个 Observation，用于后续的 Verification 和 Recovery。
//!
//! Observation 来源：
//! - ToolResult：工具执行返回的结果
//! - ComputerState：系统状态（CPU、内存、磁盘等）
//! - FileSystem：文件系统状态
//! - Process：进程状态
//! - Window：窗口状态

use serde::{Deserialize, Serialize};

/// 观察来源
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum ObservationSource {
    /// 工具执行结果
    ToolResult,
    /// 系统监控状态
    SystemMonitor,
    /// 进程状态
    Process,
    /// 窗口状态
    Window,
    /// 文件系统状态
    FileSystem,
    /// 网络状态
    Network,
    /// 应用状态
    Application,
}

/// 单个观察记录
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Observation {
    /// 唯一 ID
    pub id: String,
    /// 关联的任务 ID
    pub task_id: String,
    /// 关联的步骤 ID（可选）
    pub step_id: Option<String>,
    /// 观察来源
    pub source: ObservationSource,
    /// 来源工具名（如果是 ToolResult）
    pub tool_name: Option<String>,
    /// 是否成功
    pub success: bool,
    /// 观察数据（JSON 格式）
    pub data: serde_json::Value,
    /// 简短摘要
    pub summary: String,
    /// 时间戳（Unix 毫秒）
    pub timestamp: i64,
}

impl Observation {
    /// 从工具结果创建观察记录
    pub fn from_tool_result(
        task_id: &str,
        step_id: Option<&str>,
        tool_name: &str,
        success: bool,
        data: serde_json::Value,
        summary: String,
    ) -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            task_id: task_id.to_string(),
            step_id: step_id.map(|s| s.to_string()),
            source: ObservationSource::ToolResult,
            tool_name: Some(tool_name.to_string()),
            success,
            data,
            summary,
            timestamp: chrono::Utc::now().timestamp_millis(),
        }
    }

    /// 从系统状态创建观察记录
    pub fn from_system_state(task_id: &str, data: serde_json::Value, summary: String) -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            task_id: task_id.to_string(),
            step_id: None,
            source: ObservationSource::SystemMonitor,
            tool_name: None,
            success: true,
            data,
            summary,
            timestamp: chrono::Utc::now().timestamp_millis(),
        }
    }

    /// 从进程状态创建观察记录
    pub fn from_process_state(
        task_id: &str,
        data: serde_json::Value,
        summary: String,
    ) -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            task_id: task_id.to_string(),
            step_id: None,
            source: ObservationSource::Process,
            tool_name: None,
            success: true,
            data,
            summary,
            timestamp: chrono::Utc::now().timestamp_millis(),
        }
    }

    /// 从窗口状态创建观察记录
    pub fn from_window_state(
        task_id: &str,
        data: serde_json::Value,
        summary: String,
    ) -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            task_id: task_id.to_string(),
            step_id: None,
            source: ObservationSource::Window,
            tool_name: None,
            success: true,
            data,
            summary,
            timestamp: chrono::Utc::now().timestamp_millis(),
        }
    }
}

/// 观察集合，用于上下文构建
#[derive(Debug, Clone, Default)]
pub struct ObservationBuffer {
    observations: Vec<Observation>,
    /// 最大保留数量
    max_size: usize,
}

impl ObservationBuffer {
    pub fn new(max_size: usize) -> Self {
        Self {
            observations: Vec::new(),
            max_size,
        }
    }

    /// 添加观察记录
    pub fn push(&mut self, observation: Observation) {
        self.observations.push(observation);
        // 超过最大数量时移除最旧的
        if self.observations.len() > self.max_size {
            self.observations.remove(0);
        }
    }

    /// 获取所有观察记录
    pub fn get_all(&self) -> &[Observation] {
        &self.observations
    }

    /// 获取最近 N 条观察记录
    pub fn get_recent(&self, n: usize) -> &[Observation] {
        let start = self.observations.len().saturating_sub(n);
        &self.observations[start..]
    }

    /// 获取失败的观察记录
    pub fn get_failures(&self) -> Vec<&Observation> {
        self.observations.iter().filter(|o| !o.success).collect()
    }

    /// 清空
    pub fn clear(&mut self) {
        self.observations.clear();
    }

    /// 数量
    pub fn len(&self) -> usize {
        self.observations.len()
    }

    pub fn is_empty(&self) -> bool {
        self.observations.is_empty()
    }

    /// 构建上下文摘要（给 LLM 看）
    pub fn build_context_summary(&self) -> String {
        if self.observations.is_empty() {
            return "无历史观察".to_string();
        }
        let recent = self.get_recent(5);
        recent
            .iter()
            .map(|o| {
                let status = if o.success { "✓" } else { "✗" };
                let tool = o.tool_name.as_deref().unwrap_or("system");
                format!("{} [{}] {}", status, tool, o.summary)
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
}
