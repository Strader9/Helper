//! 恢复引擎（V14 增强版）
//!
//! 当工具执行失败或验证失败时，Recovery 分析错误并决定恢复策略。
//!
//! 恢复策略：
//! - Retry: 临时错误（超时、网络）自动重试，最多 2 次，带指数退避
//! - AdjustParams: 参数错误时分析并调整参数后重试
//! - SwitchTool: 工具不可用时换用替代工具
//! - Replan: 当前方法不可行时重新生成剩余计划
//! - Escalate: 权限/系统限制时终止并告知用户
//! - Fail: 无法恢复，标记任务失败

use crate::tools::ToolResult;
use std::time::Duration;

/// 错误类型分类
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ErrorCategory {
    /// 临时错误（超时、网络抖动、资源忙）— 可重试
    Transient,
    /// 参数错误（缺参数、格式错）— 可调整参数
    InvalidParams,
    /// 工具不可用（程序未安装、路径不存在）— 可换工具
    ToolUnavailable,
    /// 权限不足（拒绝访问、需要管理员）— 升级
    PermissionDenied,
    /// 系统限制（磁盘满、内存不足）— 升级
    SystemLimit,
    /// 不可恢复错误 — 直接失败
    Fatal,
}

impl ErrorCategory {
    /// 从错误码和错误消息分类
    pub fn classify(error_code: &str, error_msg: &str) -> Self {
        let code = error_code.to_uppercase();
        let msg = error_msg.to_lowercase();

        // 权限类
        if code.contains("PERMISSION") || code.contains("ACCESS_DENIED") || code.contains("UNAUTHORIZED")
            || msg.contains("permission denied") || msg.contains("access denied")
            || msg.contains("拒绝访问") || msg.contains("权限不足")
        {
            return ErrorCategory::PermissionDenied;
        }

        // 系统限制类
        if code.contains("DISK_FULL") || code.contains("OUT_OF_MEMORY") || code.contains("RESOURCE_LIMIT")
            || msg.contains("no space left") || msg.contains("disk full")
            || msg.contains("out of memory") || msg.contains("磁盘满")
        {
            return ErrorCategory::SystemLimit;
        }

        // 临时错误类
        if code.contains("TIMEOUT") || code.contains("NETWORK") || code.contains("CONNECTION")
            || code.contains("BUSY") || code.contains("RETRY")
            || msg.contains("timeout") || msg.contains("timed out")
            || msg.contains("connection refused") || msg.contains("network error")
            || msg.contains("超时") || msg.contains("网络")
        {
            return ErrorCategory::Transient;
        }

        // 参数错误类
        if code.contains("INVALID_PARAM") || code.contains("BAD_REQUEST") || code.contains("MISSING_PARAM")
            || code.contains("VALIDATION")
            || msg.contains("invalid argument") || msg.contains("missing required")
            || msg.contains("参数错误") || msg.contains("缺少参数")
        {
            return ErrorCategory::InvalidParams;
        }

        // 工具不可用类
        if code.contains("NOT_FOUND") || code.contains("PROGRAM_NOT_FOUND") || code.contains("TOOL_NOT_FOUND")
            || code.contains("FILE_NOT_FOUND")
            || msg.contains("not found") || msg.contains("does not exist")
            || msg.contains("找不到") || msg.contains("不存在")
        {
            return ErrorCategory::ToolUnavailable;
        }

        // 默认：视为不可恢复
        ErrorCategory::Fatal
    }
}

/// 恢复策略
#[derive(Debug, Clone)]
pub enum RecoveryStrategy {
    /// 重试当前工具（相同参数），带退避延迟
    Retry { backoff_ms: u64 },
    /// 调整参数后重试
    AdjustParams { adjusted_arguments: serde_json::Value },
    /// 换用替代工具
    SwitchTool {
        alternative_tool: String,
        adjusted_arguments: serde_json::Value,
    },
    /// 重新规划剩余步骤（由 Agent Loop 处理）
    Replan { reason: String },
    /// 升级到用户（权限/系统限制，终止并告知）
    Escalate { reason: String },
    /// 无法恢复
    Fail { reason: String },
}

/// 恢复结果
#[derive(Debug, Clone)]
pub enum RecoveryResult {
    /// 已恢复，应用策略后重试
    Recovered {
        strategy: RecoveryStrategy,
        message: String,
    },
    /// 恢复失败
    Failed { reason: String },
}

/// 恢复事件（推送给前端）
#[derive(Debug, Clone, serde::Serialize)]
pub struct RecoveryEvent {
    pub task_id: String,
    pub step_id: usize,
    pub tool_name: String,
    pub attempt: usize,
    pub strategy: String,
    pub message: String,
    pub timestamp: i64,
}

impl RecoveryEvent {
    pub fn new(
        task_id: &str,
        step_id: usize,
        tool_name: &str,
        attempt: usize,
        strategy: &str,
        message: &str,
    ) -> Self {
        Self {
            task_id: task_id.to_string(),
            step_id,
            tool_name: tool_name.to_string(),
            attempt,
            strategy: strategy.to_string(),
            message: message.to_string(),
            timestamp: chrono::Utc::now().timestamp_millis(),
        }
    }
}

/// 恢复引擎
pub struct RecoveryEngine {
    /// 最大恢复次数
    max_retries: usize,
    /// 基础退避时间（毫秒）
    base_backoff_ms: u64,
}

impl RecoveryEngine {
    pub fn new(max_retries: usize) -> Self {
        Self {
            max_retries,
            base_backoff_ms: 500,
        }
    }

    /// 计算指数退避时间
    pub fn backoff_duration(&self, attempt: usize) -> Duration {
        let ms = self.base_backoff_ms * (2u64.pow(attempt.min(5) as u32));
        Duration::from_millis(ms.min(5000))
    }

    /// 分析错误并决定恢复策略
    ///
    /// # Arguments
    /// * `tool_name` - 失败的工具名
    /// * `arguments` - 原始参数
    /// * `result` - 工具执行结果（失败）
    /// * `retry_count` - 已重试次数
    ///
    /// # Returns
    /// RecoveryResult
    pub fn analyze_and_recover(
        &self,
        tool_name: &str,
        arguments: &serde_json::Value,
        result: &ToolResult,
        retry_count: usize,
    ) -> RecoveryResult {
        // 超过最大重试次数，直接失败
        if retry_count >= self.max_retries {
            return RecoveryResult::Failed {
                reason: format!("已达到最大恢复次数 ({})", self.max_retries),
            };
        }

        let error_msg = result.error.as_deref().unwrap_or("未知错误");
        let error_code = result.error_code.as_deref().unwrap_or("UNKNOWN");

        // V14: 先做错误分类，再决定通用策略
        let category = ErrorCategory::classify(error_code, error_msg);

        match category {
            ErrorCategory::Transient => {
                // 临时错误：带退避重试
                let backoff = self.backoff_duration(retry_count);
                RecoveryResult::Recovered {
                    strategy: RecoveryStrategy::Retry {
                        backoff_ms: backoff.as_millis() as u64,
                    },
                    message: format!(
                        "临时错误（{}），{}ms 后重试（第 {} 次）",
                        error_code,
                        backoff.as_millis(),
                        retry_count + 1
                    ),
                }
            }
            ErrorCategory::PermissionDenied => {
                // 权限不足：升级到用户
                RecoveryResult::Recovered {
                    strategy: RecoveryStrategy::Escalate {
                        reason: format!("权限不足: {}", error_msg),
                    },
                    message: format!("需要用户授权: {}", error_msg),
                }
            }
            ErrorCategory::SystemLimit => {
                // 系统限制：升级到用户
                RecoveryResult::Recovered {
                    strategy: RecoveryStrategy::Escalate {
                        reason: format!("系统资源限制: {}", error_msg),
                    },
                    message: format!("系统资源不足: {}", error_msg),
                }
            }
            ErrorCategory::InvalidParams => {
                // 参数错误：尝试调整参数（通用策略：不修改，让 LLM 在下一轮调整）
                // 这里返回 Replan，让 Agent Loop 重新规划
                RecoveryResult::Recovered {
                    strategy: RecoveryStrategy::Replan {
                        reason: format!("参数错误，需要重新规划: {}", error_msg),
                    },
                    message: format!("参数错误（{}），将重新规划步骤", error_code),
                }
            }
            ErrorCategory::ToolUnavailable => {
                // 工具不可用：走工具特定的恢复逻辑
                self.recover_tool_specific(tool_name, arguments, error_code, error_msg, retry_count)
            }
            ErrorCategory::Fatal => {
                // 不可恢复错误：走工具特定逻辑，默认失败
                self.recover_tool_specific(tool_name, arguments, error_code, error_msg, retry_count)
            }
        }
    }

    /// 工具特定的恢复逻辑
    fn recover_tool_specific(
        &self,
        tool_name: &str,
        arguments: &serde_json::Value,
        error_code: &str,
        error_msg: &str,
        retry_count: usize,
    ) -> RecoveryResult {
        match tool_name {
            "launch_program" => {
                self.recover_launch_program(arguments, error_code, error_msg, retry_count)
            }
            "close_program" => self.recover_close_program(error_code, error_msg),
            "read_file" | "write_file" | "list_directory" | "open_file" => {
                self.recover_file_ops(tool_name, arguments, error_code, error_msg, retry_count)
            }
            "execute_command" => {
                // 命令执行失败：如果是临时错误重试，否则重新规划
                if retry_count == 0 {
                    RecoveryResult::Recovered {
                        strategy: RecoveryStrategy::Retry { backoff_ms: 500 },
                        message: "命令执行失败，重试一次".to_string(),
                    }
                } else {
                    RecoveryResult::Recovered {
                        strategy: RecoveryStrategy::Replan {
                            reason: format!("命令执行失败: {}", error_msg),
                        },
                        message: "命令执行失败，将重新规划".to_string(),
                    }
                }
            }
            "kill_process" => {
                // 进程不存在视为已结束
                if error_code == "PROCESS_NOT_FOUND" {
                    RecoveryResult::Recovered {
                        strategy: RecoveryStrategy::Fail {
                            reason: "进程未在运行，无需结束".to_string(),
                        },
                        message: "进程未在运行".to_string(),
                    }
                } else if retry_count == 0 {
                    RecoveryResult::Recovered {
                        strategy: RecoveryStrategy::Retry { backoff_ms: 300 },
                        message: "结束进程失败，重试一次".to_string(),
                    }
                } else {
                    RecoveryResult::Failed {
                        reason: format!("结束进程失败: {}", error_msg),
                    }
                }
            }
            _ => {
                // 默认：重试一次，之后重新规划
                if retry_count == 0 {
                    RecoveryResult::Recovered {
                        strategy: RecoveryStrategy::Retry { backoff_ms: 300 },
                        message: "重试一次".to_string(),
                    }
                } else {
                    RecoveryResult::Recovered {
                        strategy: RecoveryStrategy::Replan {
                            reason: format!("工具执行失败: {}", error_msg),
                        },
                        message: "工具执行失败，将重新规划".to_string(),
                    }
                }
            }
        }
    }

    /// launch_program 恢复：程序找不到 → find_program → 用路径重试
    fn recover_launch_program(
        &self,
        arguments: &serde_json::Value,
        _error_code: &str,
        error_msg: &str,
        retry_count: usize,
    ) -> RecoveryResult {
        if retry_count >= self.max_retries {
            return RecoveryResult::Failed {
                reason: format!("启动程序失败: {}", error_msg),
            };
        }

        let name_or_path = arguments
            .get("name_or_path")
            .and_then(|v| v.as_str())
            .unwrap_or("");

        // 如果参数已经是完整路径且存在，直接重试一次
        if std::path::Path::new(name_or_path).exists() {
            if retry_count == 0 {
                return RecoveryResult::Recovered {
                    strategy: RecoveryStrategy::Retry { backoff_ms: 500 },
                    message: "路径已存在，重试启动".to_string(),
                };
            }
            return RecoveryResult::Failed {
                reason: format!("程序启动失败: {}", error_msg),
            };
        }

        // 参数是程序名，尝试搜索路径
        RecoveryResult::Recovered {
            strategy: RecoveryStrategy::SwitchTool {
                alternative_tool: "find_program".to_string(),
                adjusted_arguments: serde_json::json!({
                    "name": name_or_path
                }),
            },
            message: format!("程序 '{}' 启动失败，将先搜索安装路径", name_or_path),
        }
    }

    /// close_program 恢复：进程不存在 → 直接成功（程序本来就没运行）
    fn recover_close_program(&self, error_code: &str, error_msg: &str) -> RecoveryResult {
        if error_code == "PROCESS_NOT_FOUND" {
            RecoveryResult::Recovered {
                strategy: RecoveryStrategy::Fail {
                    reason: "程序未在运行，无需关闭".to_string(),
                },
                message: "程序未在运行".to_string(),
            }
        } else {
            RecoveryResult::Failed {
                reason: format!("关闭程序失败: {}", error_msg),
            }
        }
    }

    /// 文件操作恢复：重试一次，之后重新规划
    fn recover_file_ops(
        &self,
        tool_name: &str,
        arguments: &serde_json::Value,
        _error_code: &str,
        error_msg: &str,
        retry_count: usize,
    ) -> RecoveryResult {
        if retry_count == 0 {
            RecoveryResult::Recovered {
                strategy: RecoveryStrategy::Retry { backoff_ms: 300 },
                message: format!("{} 失败，重试一次", tool_name),
            }
        } else {
            let path = arguments
                .get("path")
                .and_then(|v| v.as_str())
                .unwrap_or("未知路径");
            // 文件不存在时重新规划（可能需要先创建目录或换路径）
            RecoveryResult::Recovered {
                strategy: RecoveryStrategy::Replan {
                    reason: format!("{} 失败 ({}): {}", tool_name, path, error_msg),
                },
                message: format!("{} 失败，将重新规划", tool_name),
            }
        }
    }
}

impl Default for RecoveryEngine {
    fn default() -> Self {
        Self::new(2)
    }
}
