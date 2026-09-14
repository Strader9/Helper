//! 工具执行器
//!
//! 三层闸门第三层：ToolExecutor —— 实际执行工具的沙箱层。
//! 负责：权限校验、超时控制、审计日志、错误封装。
//!
//! Phase 4 新增：权限确认弹窗支持，通过 Tauri Event 与前端交互。

use std::sync::Arc;
use std::collections::HashMap;
use tokio::sync::Mutex;
use tokio::time::{timeout, Duration};
use tauri::Emitter;

use crate::db::Database;
use crate::error::{AppError, AppResult};
use crate::security::{PermissionManager, PermissionResult, DENY_USER_DENIED};
use crate::security::{PermissionDecision, PermissionRequestPayload};
use crate::tools::{ToolRegistry, ToolResult, RiskLevel};

/// 工具执行请求
#[derive(Debug, Clone)]
pub struct ToolCallRequest {
    pub tool_name: String,
    pub arguments: serde_json::Value,
    /// 会话 ID（用于审计日志）
    pub session_id: Option<String>,
}

/// 工具执行器
///
/// 三层闸门最后一层：
/// 1. 从 ToolRegistry 查找工具
/// 2. PermissionManager 判定权限（含路径安全校验）
/// 3. CRITICAL → 拒绝
/// 4. ConfirmationRequired → 通过 Tauri Event 发送前端确认弹窗
/// 5. 执行工具（带超时）
/// 6. 记录审计日志
/// 7. 返回结果
pub struct ToolExecutor {
    permission_manager: Arc<Mutex<PermissionManager>>,
    tool_registry: Arc<ToolRegistry>,
    app_handle: std::sync::Mutex<Option<tauri::AppHandle>>,
    pending_permissions: std::sync::Mutex<Option<Arc<Mutex<HashMap<String, PendingPermission>>>>>,
    /// 数据库连接（用于审计日志）
    db: Arc<Mutex<Database>>,
}

/// 待处理的权限确认请求
#[derive(Debug)]
pub struct PendingPermission {
    pub sender: tokio::sync::oneshot::Sender<PermissionDecision>,
    pub tool_name: String,
    pub path: Option<String>,
    pub risk_level: String,
}

impl ToolExecutor {
    /// 创建工具执行器
    pub fn new(
        permission_manager: Arc<Mutex<PermissionManager>>,
        tool_registry: Arc<ToolRegistry>,
        db: Arc<Mutex<Database>>,
    ) -> Self {
        Self {
            permission_manager,
            tool_registry,
            app_handle: std::sync::Mutex::new(None),
            pending_permissions: std::sync::Mutex::new(None),
            db,
        }
    }

    /// 设置 AppHandle（用于向前端发送事件）
    pub fn set_app_handle(&self, handle: tauri::AppHandle) {
        let mut guard = self.app_handle.lock().unwrap();
        *guard = Some(handle);
    }

    /// 设置待处理权限请求映射
    pub fn set_pending_permissions(&self, pending: Arc<Mutex<HashMap<String, PendingPermission>>>) {
        let mut guard = self.pending_permissions.lock().unwrap();
        *guard = Some(pending);
    }

    /// 获取权限管理器引用
    pub fn permission_manager(&self) -> &Arc<Mutex<PermissionManager>> {
        &self.permission_manager
    }

    /// 获取工具注册表引用
    pub fn tool_registry(&self) -> &Arc<ToolRegistry> {
        &self.tool_registry
    }

    /// 执行单个工具调用
    ///
    /// # 流程
    /// 1. 查找工具
    /// 2. 权限校验（含路径安全）
    /// 3. 确认需要时发送弹窗事件
    /// 4. 等待用户确认或超时
    /// 5. 执行工具（带超时）
    /// 6. 记录审计日志
    /// 7. 返回结果
    pub async fn execute(
        &self,
        request: ToolCallRequest,
    ) -> AppResult<ToolExecutionResult> {
        let session_id = request.session_id.clone().unwrap_or_else(|| "unknown".to_string());

        // 1. 查找工具
        let tool = self
            .tool_registry
            .get(&request.tool_name)
            .ok_or_else(|| {
                AppError::ToolNotFound(format!(
                    "Tool '{}' is not registered",
                    request.tool_name
                ))
            })?;

        let risk_level = tool.risk_level();
        let risk_str = risk_level.as_str().to_string();

        // 2. 提取路径参数（用于路径安全校验）
        let path_arg = extract_path_from_args(&request.arguments, &request.tool_name);

        // 3. 权限校验
        let permission = {
            let pm = self.permission_manager.lock().await;
            pm.check_permission(risk_level, &request.tool_name, path_arg.as_deref())
        };

        let permission_result_str;

        match permission {
            PermissionResult::Allowed => {
                permission_result_str = "ALLOWED".to_string();
            }
            PermissionResult::Denied { reason, code } => {
                // 记录审计日志（被权限系统拒绝）
                self.write_audit(
                    &session_id, &request.tool_name, &risk_str,
                    Some(&request.arguments.to_string()),
                    &format!("DENIED: {}", code), false,
                    Some(&code), Some(&reason), 0,
                    Some(&format!("权限拒绝: {}", code)),
                ).await;
                return Ok(ToolExecutionResult {
                    tool_name: request.tool_name,
                    success: false,
                    result: ToolResult::err(&code, &reason),
                    permission_result: format!("DENIED: {}", code),
                    duration_ms: 0,
                });
            }
            PermissionResult::ConfirmationRequired => {
                let decision = match self.request_permission_confirmation(
                    &request.tool_name,
                    &request.arguments,
                    risk_level,
                    path_arg.as_deref(),
                ).await {
                    Ok(d) => d,
                    Err(e) => {
                        eprintln!("权限确认失败: {}, 默认拒绝", e);
                        PermissionDecision::Deny
                    }
                };

                match decision {
                    PermissionDecision::AllowOnce => {
                        permission_result_str = "ALLOWED_ONCE".to_string();
                    }
                    PermissionDecision::AlwaysAllow => {
                        permission_result_str = "ALLOWED_ALWAYS".to_string();
                    }
                    PermissionDecision::Deny => {
                        self.write_audit(
                            &session_id, &request.tool_name, &risk_str,
                            Some(&request.arguments.to_string()),
                            "DENIED: USER_DENIED", false,
                            Some(DENY_USER_DENIED), Some("用户拒绝了此操作"), 0,
                            Some("用户拒绝操作"),
                        ).await;
                        return Ok(ToolExecutionResult {
                            tool_name: request.tool_name,
                            success: false,
                            result: ToolResult::err(DENY_USER_DENIED, "用户拒绝了此操作"),
                            permission_result: "DENIED: USER_DENIED".to_string(),
                            duration_ms: 0,
                        });
                    }
                }
            }
        }

        // 4. 执行工具（带超时）
        let tool_timeout = Duration::from_secs(if risk_level == RiskLevel::Safe {
            2
        } else {
            10
        });

        let start = std::time::Instant::now();

        // 先克隆 arguments 用于审计（tool.execute 会 move 掉 request.arguments）
        let args_json = request.arguments.to_string();

        let result = match timeout(tool_timeout, tool.execute(request.arguments)).await {
            Ok(Ok(result)) => result,
            Ok(Err(e)) => {
                ToolResult::err("EXECUTION_ERROR", &e.to_string())
            }
            Err(_) => {
                ToolResult::err("TIMEOUT", "Tool execution timed out")
            }
        };

        let duration_ms = start.elapsed().as_millis() as i64;
        let success = result.success;

        // 生成结果摘要
        let result_summary = if success {
            result.data.as_ref()
                .map(|d| {
                    // 截取前 200 字符作为摘要
                    let s = d.to_string();
                    if s.len() > 200 { format!("{}...", &s[..200]) } else { s }
                })
                .or(Some("执行成功".to_string()))
        } else {
            result.error.clone()
        };

        // 5. 记录审计日志
        self.write_audit(
            &session_id, &request.tool_name, &risk_str,
            Some(&args_json),
            &permission_result_str, success,
            result.error_code.as_deref(), result.error.as_deref(),
            duration_ms,
            result_summary.as_deref(),
        ).await;

        Ok(ToolExecutionResult {
            tool_name: request.tool_name,
            success,
            result,
            permission_result: permission_result_str,
            duration_ms,
        })
    }

    /// 写入审计日志（失败仅记录日志，不影响主流程）
    async fn write_audit(
        &self,
        session_id: &str,
        tool_name: &str,
        risk_level: &str,
        arguments: Option<&str>,
        permission_result: &str,
        success: bool,
        error_code: Option<&str>,
        error_message: Option<&str>,
        duration_ms: i64,
        result_summary: Option<&str>,
    ) {
        let db = self.db.lock().await;
        if let Err(e) = db.write_audit_log(
            session_id, tool_name, risk_level, arguments,
            permission_result, success, error_code, error_message,
            Some(duration_ms), result_summary,
        ) {
            eprintln!("[Audit] 写入审计日志失败: {}", e);
        }
    }

    /// 批量执行工具调用
    pub async fn execute_batch(
        &self,
        requests: Vec<ToolCallRequest>,
    ) -> Vec<AppResult<ToolExecutionResult>> {
        let mut results = Vec::with_capacity(requests.len());

        for req in requests {
            let result = self.execute(req).await;
            results.push(result);
        }

        results
    }

    // ============================================================
    // 权限确认交互
    // ============================================================

    /// 向前端发送权限确认请求，等待用户响应
    ///
    /// 使用 tokio::sync::oneshot 实现异步等待。
    /// 超时 30 秒默认拒绝。
    async fn request_permission_confirmation(
        &self,
        tool_name: &str,
        arguments: &serde_json::Value,
        risk_level: RiskLevel,
        path: Option<&str>,
    ) -> AppResult<PermissionDecision> {
        // 缩小锁作用域：clone 所需值后立即释放 MutexGuard，避免跨越 await
        let (app_handle_opt, pending_opt) = {
            let app = self.app_handle.lock().unwrap();
            let pending = self.pending_permissions.lock().unwrap();
            (app.clone(), pending.clone())
        };

        match (app_handle_opt.as_ref(), pending_opt.as_ref()) {
            (Some(app_handle), Some(pending)) => {
                let request_id = uuid::Uuid::new_v4().to_string();
                let (tx_perm, rx_perm) = tokio::sync::oneshot::channel();

                // 生成描述
                let description = format_tool_description(tool_name, arguments, path);

                let pending_perm = PendingPermission {
                    sender: tx_perm,
                    tool_name: tool_name.to_string(),
                    path: path.map(|s| s.to_string()),
                    risk_level: risk_level.as_str().to_string(),
                };

                // 注册 pending permission
                {
                    let mut map = pending.lock().await;
                    map.insert(request_id.clone(), pending_perm);
                }

                let payload = PermissionRequestPayload {
                    request_id: request_id.clone(),
                    tool_name: tool_name.to_string(),
                    description,
                    risk_level: risk_level.as_str().to_string(),
                    path: path.map(|s| s.to_string()),
                };

                // 发送事件到前端
                app_handle.emit("permission-request", &payload)
                    .map_err(|e| AppError::Internal(format!("发送权限请求事件失败: {}", e)))?;

                // 等待前端响应，60秒超时（给用户足够时间阅读和操作）
                let decision = match timeout(Duration::from_secs(60), rx_perm).await {
                    Ok(Ok(decision)) => decision,
                    Ok(Err(_)) => {
                        eprintln!("权限确认通道已关闭，默认拒绝");
                        // 清理 pending
                        let mut map = pending.lock().await;
                        map.remove(&request_id);
                        PermissionDecision::Deny
                    }
                    Err(_) => {
                        eprintln!("权限确认超时（60秒），默认拒绝");
                        // 清理 pending
                        let mut map = pending.lock().await;
                        map.remove(&request_id);
                        PermissionDecision::Deny
                    }
                };

                Ok(decision)
            }
            _ => {
                // 没有 AppHandle 或 pending map，权限确认不可用 → 默认拒绝（安全优先）
                eprintln!("Warning: AppHandle 或 pending_permissions 未设置，权限确认不可用。默认拒绝。");
                Ok(PermissionDecision::Deny)
            }
        }
    }
}

// ============================================================
// 辅助函数
// ============================================================

/// 从工具参数中提取路径参数
/// 从工具参数中提取路径参数，并解析环境变量（如 %USERPROFILE%）
fn extract_path_from_args(args: &serde_json::Value, tool_name: &str) -> Option<String> {
    let raw = match tool_name {
        "read_file" | "write_file" | "delete_file" | "open_file" | "list_directory" | "create_directory" => {
            args.get("path").and_then(|v| v.as_str()).map(|s| s.to_string())
        }
        "move_file" => {
            args.get("src").and_then(|v| v.as_str()).map(|s| s.to_string())
                .or_else(|| args.get("destination").and_then(|v| v.as_str()).map(|s| s.to_string()))
        }
        "launch_program" => {
            args.get("name_or_path").and_then(|v| v.as_str()).map(|s| s.to_string())
        }
        "execute_command" => {
            args.get("cwd").and_then(|v| v.as_str()).map(|s| s.to_string())
        }
        _ => None,
    };
    raw.map(|p| expand_windows_env_vars(&p))
}

/// 解析 Windows 环境变量（如 %USERPROFILE% → C:\Users\xxx）
fn expand_windows_env_vars(path: &str) -> String {
    let mut result = path.to_string();
    let mut start = 0;
    while let Some(pct1) = result[start..].find('%') {
        let abs_pct1 = start + pct1;
        if let Some(pct2) = result[abs_pct1 + 1..].find('%') {
            let abs_pct2 = abs_pct1 + 1 + pct2;
            let var_name = &result[abs_pct1 + 1..abs_pct2];
            if let Ok(val) = std::env::var(var_name) {
                result.replace_range(abs_pct1..=abs_pct2, &val);
                start = abs_pct1 + val.len();
            } else {
                start = abs_pct2 + 1;
            }
        } else {
            break;
        }
    }
    result
}

/// 生成工具描述（用于弹窗展示）
fn format_tool_description(tool_name: &str, args: &serde_json::Value, path: Option<&str>) -> String {
    match tool_name {
        "read_file" => {
            if let Some(p) = path {
                format!("读取文件: {}", p)
            } else {
                "读取文件".to_string()
            }
        }
        "write_file" => {
            if let Some(p) = path {
                format!("写入文件: {}", p)
            } else {
                "写入文件".to_string()
            }
        }
        "delete_file" => {
            if let Some(p) = path {
                format!("删除文件: {}", p)
            } else {
                "删除文件".to_string()
            }
        }
        "list_directory" => {
            if let Some(p) = path {
                format!("列出目录: {}", p)
            } else {
                "列出目录".to_string()
            }
        }
        "open_file" => {
            if let Some(p) = path {
                format!("打开文件: {}", p)
            } else {
                "打开文件".to_string()
            }
        }
        "launch_program" => {
            if let Some(p) = path {
                format!("启动程序: {}", p)
            } else {
                "启动程序".to_string()
            }
        }
        "execute_command" => {
            if let Some(cmd) = args.get("command").and_then(|v| v.as_str()) {
                format!("执行命令: {}", cmd)
            } else {
                "执行系统命令".to_string()
            }
        }
        "take_screenshot" => "截取屏幕".to_string(),
        "find_program" => {
            if let Some(name) = args.get("name").and_then(|v| v.as_str()) {
                format!("搜索程序: {}", name)
            } else {
                "搜索程序".to_string()
            }
        }
        _ => format!("执行工具: {}", tool_name),
    }
}

/// 工具执行结果
#[derive(Debug, Clone)]
pub struct ToolExecutionResult {
    pub tool_name: String,
    pub success: bool,
    pub result: ToolResult,
    pub permission_result: String,
    pub duration_ms: i64,
}

impl ToolExecutionResult {
    /// 转换为给 LLM 的 tool 消息内容
    pub fn to_tool_message_content(&self) -> String {
        if self.result.success {
            self.result
                .data
                .as_ref()
                .map(|d| d.to_string())
                .unwrap_or_else(|| "{}".to_string())
        } else {
            format!(
                r#"{{"error": "{}", "code": "{}"}}"#,
                self.result.error.as_deref().unwrap_or("unknown error"),
                self.result.error_code.as_deref().unwrap_or("UNKNOWN")
            )
        }
    }
}
