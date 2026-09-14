//! 上下文引擎（V15 新增）
//!
//! ContextEngine 负责聚合和管理 Agent 的运行时上下文，包括：
//! - 对话历史摘要
//! - 系统状态（CPU/内存/磁盘/活动窗口/前台进程）
//! - 任务状态（当前 Task + 步骤进度 + 观察记录）
//! - 工具结果缓存（最近工具调用结果）
//! - 用户偏好（从 settings 表读取）
//!
//! 上下文窗口管理：自动截断/摘要超长历史，重要信息优先级保留，滑动窗口策略。
//! 上下文注入：构建 System Prompt 时自动注入相关上下文。

use serde::{Deserialize, Serialize};
use std::collections::VecDeque;

use crate::agent::observation::Observation;
use crate::agent::task::Task;
use crate::monitoring::SystemMetrics;

// ============================================================
// 上下文数据模型
// ============================================================

/// 系统状态快照
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SystemStateSnapshot {
    pub cpu_usage: f32,
    pub memory_usage_percent: f32,
    pub memory_used_gb: f32,
    pub memory_total_gb: f32,
    pub disks: Vec<DiskSnapshot>,
    pub active_window: Option<String>,
    pub foreground_process: Option<String>,
    pub process_count: usize,
    pub network_ok: bool,
    pub timestamp: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiskSnapshot {
    pub drive: String,
    pub usage_percent: f32,
    pub free_gb: f32,
}

impl SystemStateSnapshot {
    /// 从 SystemMetrics 构建（活动窗口和进程数需要额外获取）
    pub fn from_metrics(metrics: &SystemMetrics) -> Self {
        Self {
            cpu_usage: metrics.cpu_usage,
            memory_usage_percent: metrics.memory_usage_percent,
            memory_used_gb: metrics.memory_used_gb,
            memory_total_gb: metrics.memory_total_gb,
            disks: metrics
                .disks
                .iter()
                .map(|d| DiskSnapshot {
                    drive: d.drive.clone(),
                    usage_percent: d.usage_percent,
                    free_gb: (d.total_gb - d.used_gb).max(0.0),
                })
                .collect(),
            active_window: None,
            foreground_process: None,
            process_count: 0,
            network_ok: metrics.network_ok,
            timestamp: chrono::Utc::now().timestamp_millis(),
        }
    }

    /// 生成简短摘要文本
    pub fn summary(&self) -> String {
        let disk_str = if self.disks.is_empty() {
            "无磁盘信息".to_string()
        } else {
            self.disks
                .iter()
                .map(|d| format!("{}:{:.0}%", d.drive, d.usage_percent))
                .collect::<Vec<_>>()
                .join(" ")
        };

        let window_str = self
            .active_window
            .as_deref()
            .unwrap_or("未知");

        format!(
            "CPU:{:.0}% 内存:{:.0}%({:.1}/{:.1}GB) 磁盘:[{}] 窗口:{} 网络:{}",
            self.cpu_usage,
            self.memory_usage_percent,
            self.memory_used_gb,
            self.memory_total_gb,
            disk_str,
            window_str,
            if self.network_ok { "正常" } else { "异常" }
        )
    }
}

/// 工具结果缓存条目
#[derive(Debug, Clone)]
pub struct ToolResultCacheEntry {
    pub tool_name: String,
    pub success: bool,
    pub summary: String,
    pub timestamp: i64,
}

/// 用户偏好
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct UserPreferences {
    pub language: String,
    pub confirm_medium_risk: bool,
    pub max_iterations: usize,
    pub temperature: f32,
}

// ============================================================
// 上下文引擎
// ============================================================

/// 上下文引擎
///
/// 聚合所有上下文源，管理上下文窗口，提供注入方法。
pub struct ContextEngine {
    /// 系统状态快照（定期更新）
    system_state: Option<SystemStateSnapshot>,
    /// 工具结果缓存（滑动窗口）
    tool_result_cache: VecDeque<ToolResultCacheEntry>,
    /// 最大缓存条目数
    max_cache_size: usize,
    /// 对话历史最大保留条数
    max_history_messages: usize,
    /// 用户偏好
    preferences: UserPreferences,
}

impl ContextEngine {
    pub fn new() -> Self {
        Self {
            system_state: None,
            tool_result_cache: VecDeque::new(),
            max_cache_size: 10,
            max_history_messages: 20,
            preferences: UserPreferences::default(),
        }
    }

    /// 更新系统状态快照
    pub fn update_system_state(&mut self, state: SystemStateSnapshot) {
        self.system_state = Some(state);
    }

    /// 获取当前系统状态
    pub fn get_system_state(&self) -> Option<&SystemStateSnapshot> {
        self.system_state.as_ref()
    }

    /// 添加工具结果到缓存
    pub fn add_tool_result(&mut self, tool_name: &str, success: bool, summary: &str) {
        let entry = ToolResultCacheEntry {
            tool_name: tool_name.to_string(),
            success,
            summary: summary.to_string(),
            timestamp: chrono::Utc::now().timestamp_millis(),
        };
        self.tool_result_cache.push_back(entry);
        if self.tool_result_cache.len() > self.max_cache_size {
            self.tool_result_cache.pop_front();
        }
    }

    /// 设置用户偏好
    pub fn set_preferences(&mut self, prefs: UserPreferences) {
        self.preferences = prefs;
    }

    /// 获取用户偏好
    pub fn get_preferences(&self) -> &UserPreferences {
        &self.preferences
    }

    // ============================================================
    // 上下文窗口管理
    // ============================================================

    /// 截断对话历史到最大条数，保留系统消息和最近的消息
    pub fn truncate_history<T>(&self, messages: &[T]) -> Vec<T>
    where
        T: Clone,
    {
        if messages.len() <= self.max_history_messages {
            return messages.to_vec();
        }

        // 保留第一条（通常是 system prompt）和最近的 N-1 条
        let mut result = Vec::with_capacity(self.max_history_messages);
        result.push(messages[0].clone());
        let start = messages.len() - (self.max_history_messages - 1);
        result.extend_from_slice(&messages[start..]);
        result
    }

    /// 构建上下文摘要（给 LLM 看的精简版）
    pub fn build_context_summary(
        &self,
        task: &Task,
        observations: &[Observation],
    ) -> String {
        let mut parts = Vec::new();

        // 系统状态
        if let Some(ref state) = self.system_state {
            parts.push(format!("## 系统状态\n{}", state.summary()));
        }

        // 任务进度
        parts.push(format!(
            "## 任务进度\n目标: {}\n状态: {}\n进度: {}/{} 步",
            task.goal,
            task.status.as_str(),
            task.completed_count(),
            task.total_steps()
        ));

        // 最近观察（最多 5 条）
        if !observations.is_empty() {
            let recent = &observations[observations.len().saturating_sub(5)..];
            let obs_str = recent
                .iter()
                .map(|o| {
                    let status = if o.success { "✓" } else { "✗" };
                    let tool = o.tool_name.as_deref().unwrap_or("system");
                    format!("{} [{}] {}", status, tool, o.summary)
                })
                .collect::<Vec<_>>()
                .join("\n");
            parts.push(format!("## 近期观察\n{}", obs_str));
        }

        // 工具结果缓存摘要
        if !self.tool_result_cache.is_empty() {
            let cache_str = self
                .tool_result_cache
                .iter()
                .rev()
                .take(3)
                .map(|e| {
                    let status = if e.success { "✓" } else { "✗" };
                    format!("{} [{}] {}", status, e.tool_name, e.summary)
                })
                .collect::<Vec<_>>()
                .join("\n");
            parts.push(format!("## 最近工具结果\n{}", cache_str));
        }

        parts.join("\n\n")
    }

    /// 构建完整的 System Prompt 注入文本（复杂任务用）
    pub fn build_system_prompt_injection(
        &self,
        task: &Task,
        observations: &[Observation],
    ) -> String {
        let summary = self.build_context_summary(task, observations);
        format!(
            "## 运行时上下文（自动注入）\n\n{}\n\n请根据以上上下文决定下一步操作。",
            summary
        )
    }

    /// 构建简单任务的上下文注入（精简版）
    pub fn build_simple_context_injection(&self) -> String {
        if let Some(ref state) = self.system_state {
            format!(
                "## 系统状态\n{}\n",
                state.summary()
            )
        } else {
            String::new()
        }
    }

    /// 估算上下文 token 数（粗略估算：中文 1.5 char/token，英文 4 char/token）
    pub fn estimate_tokens(text: &str) -> usize {
        let chinese_chars = text.chars().filter(|c| c.is_ascii()).count();
        let other_chars = text.len() - chinese_chars;
        (chinese_chars / 4) + (other_chars / 2)
    }

    /// 智能截断：如果文本超过 token 限制，保留开头和结尾，中间用摘要替代
    pub fn smart_truncate(text: &str, max_tokens: usize) -> String {
        let estimated = Self::estimate_tokens(text);
        if estimated <= max_tokens {
            return text.to_string();
        }

        // 粗略按比例截断：保留前 40% 和后 40%，中间标注省略
        let chars: Vec<char> = text.chars().collect();
        let keep_count = (chars.len() as f32 * 0.8) as usize;
        let head_count = keep_count / 2;
        let tail_start = chars.len() - (keep_count - head_count);

        let head: String = chars[..head_count].iter().collect();
        let tail: String = chars[tail_start..].iter().collect();
        format!("{}...[内容已截断，省略约 {} 字符]...{}", head, chars.len() - keep_count, tail)
    }
}

impl Default for ContextEngine {
    fn default() -> Self {
        Self::new()
    }
}
