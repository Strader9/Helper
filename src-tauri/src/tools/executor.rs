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
use crate::tools::contract::{contract_for, CapabilityContract, VerificationStrategy};
use crate::tools::receipt::{ProbeVerdict, VerificationOutcome, VerificationStatus};
use crate::tools::{ToolRegistry, ToolResult, RiskLevel};

/// 工具执行请求
#[derive(Debug, Clone)]
pub struct ToolCallRequest {
    pub tool_name: String,
    pub arguments: serde_json::Value,
    /// 会话 ID（用于审计日志）
    pub session_id: Option<String>,
    /// 本次用户指令的运行 ID（用于取消与前端事件隔离）
    pub run_id: Option<String>,
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
    pub session_id: String,
    pub run_id: Option<String>,
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

    /// 取消某个会话仍在等待的权限请求。移除 sender 会立即唤醒对应执行流并安全拒绝。
    pub async fn cancel_pending_for_session(&self, session_id: &str) {
        let pending = self.pending_permissions.lock().unwrap().clone();
        if let Some(pending) = pending {
            let mut map = pending.lock().await;
            map.retain(|_, request| request.session_id != session_id);
        }
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
        let run_id = request.run_id.clone();

        // 工具只能拿到 args，拿不到会话。任务编排类工具必须知道当前指令
        // 属于哪个会话，否则不同对话的计划会挤进同一个列表。
        crate::tools::set_current_session(&session_id);

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

        // 1.5 取能力契约。
        // 没有契约的工具照样能跑（不因为记账缺失就罢工），但回执里必须
        // 如实标记"未声明契约"，否则事后无法区分"验证通过"和"没人说要验证"。
        let contract = contract_for(&request.tool_name);
        if contract.is_none() {
            eprintln!(
                "[Capability] 工具 '{}' 没有能力契约，其副作用与验证策略未知",
                request.tool_name
            );
        }

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
                let denied_result = ToolResult::err(&code, &reason);
                let permission_str = format!("DENIED: {}", code);
                // 记录审计日志（被权限系统拒绝）
                self.write_audit(
                    &session_id, &request.tool_name, &risk_str,
                    Some(&request.arguments.to_string()),
                    &permission_str, false,
                    Some(&code), Some(&reason), 0,
                    Some(&format!("权限拒绝: {}", code)),
                ).await;
                let (verification, receipt_id) = self
                    .persist_receipt(
                        contract, &request.tool_name, &session_id, run_id.as_deref(),
                        &request.arguments, &denied_result, &permission_str, 0, &risk_str,
                        None,
                    )
                    .await;
                return Ok(ToolExecutionResult {
                    tool_name: request.tool_name,
                    success: false,
                    result: denied_result,
                    permission_result: permission_str,
                    duration_ms: 0,
                    verification,
                    receipt_id,
                });
            }
            PermissionResult::ConfirmationRequired => {
                // 无人值守会话（定时任务）没人能点确认按钮。
                //
                // 不特判的话，这里会发一条没人看的确认事件、干等 60 秒再超时拒绝——
                // 一次作业可能撞上好几个需要确认的工具，几分钟就这么耗掉了，
                // 而模型还一直以为自己只是在等一个会来的答复。直接拒绝更快，
                // 也把"为什么不行"讲清楚，模型能立刻换路。
                if is_unattended_session(&session_id) {
                    let reason = format!(
                        "无人值守模式下「{}」需要用户确认，已跳过。请改用不需要确认的方式完成任务。",
                        request.tool_name
                    );
                    let denied_result = ToolResult::err("UNATTENDED_REQUIRES_CONFIRMATION", &reason);
                    let permission_str = "DENIED: UNATTENDED".to_string();
                    self.write_audit(
                        &session_id, &request.tool_name, &risk_str,
                        Some(&request.arguments.to_string()),
                        &permission_str, false,
                        Some("UNATTENDED_REQUIRES_CONFIRMATION"), Some(&reason), 0,
                        Some(&format!("无人值守跳过: {}", reason)),
                    ).await;
                    let (verification, receipt_id) = self
                        .persist_receipt(
                            contract, &request.tool_name, &session_id, run_id.as_deref(),
                            &request.arguments, &denied_result, &permission_str, 0, &risk_str,
                            None,
                        )
                        .await;
                    return Ok(ToolExecutionResult {
                        tool_name: request.tool_name,
                        success: false,
                        result: denied_result,
                        permission_result: permission_str,
                        duration_ms: 0,
                        verification,
                        receipt_id,
                    });
                }

                let decision = match self.request_permission_confirmation(
                    &request.tool_name,
                    &request.arguments,
                    risk_level,
                    path_arg.as_deref(),
                    &session_id,
                    run_id.as_deref(),
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
                        let denied_result = ToolResult::err(DENY_USER_DENIED, "用户拒绝了此操作");
                        let permission_str = "DENIED: USER_DENIED".to_string();
                        self.write_audit(
                            &session_id, &request.tool_name, &risk_str,
                            Some(&request.arguments.to_string()),
                            &permission_str, false,
                            Some(DENY_USER_DENIED), Some("用户拒绝了此操作"), 0,
                            Some("用户拒绝操作"),
                        ).await;
                        let (verification, receipt_id) = self
                            .persist_receipt(
                                contract, &request.tool_name, &session_id, run_id.as_deref(),
                                &request.arguments, &denied_result, &permission_str, 0, &risk_str,
                                None,
                            )
                            .await;
                        return Ok(ToolExecutionResult {
                            tool_name: request.tool_name,
                            success: false,
                            result: denied_result,
                            permission_result: permission_str,
                            duration_ms: 0,
                            verification,
                            receipt_id,
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
        // 回执要留一份结构化参数算摘要与幂等键，同样在 move 之前取
        let args_for_receipt = request.arguments.clone();

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
                    if s.chars().count() > 200 {
                        format!("{}...", s.chars().take(200).collect::<String>())
                    } else {
                        s
                    }
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

        // 6. 独立探查：工具说成功了，去现实世界里看一眼是不是真成功。
        //    只对声明了机器可判据、且确实执行成功的能力做——探查要起子进程
        //    （tasklist 等），对失败调用和"没有判据"的能力没有意义。
        let probe = self
            .probe(&request.tool_name, contract, &args_for_receipt, &result)
            .await;

        // 7. 落一条事实回执：这次调用到底改变了什么、拿什么证据算完成
        let (verification, receipt_id) = self
            .persist_receipt(
                contract,
                &request.tool_name,
                &session_id,
                run_id.as_deref(),
                &args_for_receipt,
                &result,
                &permission_result_str,
                duration_ms,
                &risk_str,
                probe.as_ref(),
            )
            .await;

        if verification.status == VerificationStatus::Unverified {
            eprintln!(
                "[Receipt] '{}' 自报成功但回执缺少字段: {}",
                request.tool_name,
                verification.missing_fields.join(", ")
            );
        }
        if verification.status == VerificationStatus::Contradicted {
            eprintln!(
                "[Receipt] '{}' 自报成功但独立探查否证: {}",
                request.tool_name,
                verification.evidence.as_deref().unwrap_or("无判据")
            );
        }

        Ok(ToolExecutionResult {
            tool_name: request.tool_name,
            success,
            result,
            permission_result: permission_result_str,
            duration_ms,
            verification,
            receipt_id,
        })
    }

    /// 计算验证结论并落库（实现见 `receipt::record`）。
    ///
    /// 执行器只负责把上下文递过去，判定与记账的规则集中在回执模块，
    /// 避免"执行器一套规则、测试另一套规则"。
    #[allow(clippy::too_many_arguments)]
    async fn persist_receipt(
        &self,
        contract: Option<&'static CapabilityContract>,
        tool_name: &str,
        session_id: &str,
        run_id: Option<&str>,
        arguments: &serde_json::Value,
        result: &ToolResult,
        permission_result: &str,
        duration_ms: i64,
        risk_level: &str,
        probe: Option<&ProbeVerdict>,
    ) -> (VerificationOutcome, String) {
        let db = self.db.lock().await;
        crate::tools::receipt::record(
            &db,
            contract,
            tool_name,
            session_id,
            run_id,
            arguments,
            result,
            permission_result,
            duration_ms,
            risk_level,
            probe,
        )
    }

    /// 做一次独立探查，把"工具自述"与"现实"对齐。
    ///
    /// 探查会起子进程（`tasklist` 之类），所以：
    /// - 只在工具确实执行成功、且契约声明了机器可判据时才做；
    /// - 用 `spawn_blocking` 挪出异步线程池，避免阻塞运行时。
    async fn probe(
        &self,
        tool_name: &str,
        contract: Option<&'static CapabilityContract>,
        arguments: &serde_json::Value,
        result: &ToolResult,
    ) -> Option<ProbeVerdict> {
        let contract = contract?;
        if contract.verification == VerificationStrategy::None || !result.success {
            return Some(ProbeVerdict::Unavailable);
        }

        let tool = tool_name.to_string();
        let args = arguments.clone();
        let result = result.clone();

        let joined = tokio::task::spawn_blocking(move || {
            crate::agent::verification::VerificationEngine::new().verify(&tool, &args, &result)
        })
        .await;

        use crate::agent::verification::VerificationResult;
        match joined {
            Ok(VerificationResult::Pass { summary }) => Some(ProbeVerdict::Pass(summary)),
            Ok(VerificationResult::Fail { reason }) => Some(ProbeVerdict::Fail(reason)),
            Ok(VerificationResult::Unavailable { .. }) => Some(ProbeVerdict::Unavailable),
            // 探查任务本身崩了：如实记"没查"，不要吞掉也不要冒充通过。
            Err(e) => {
                eprintln!("[Probe] '{}' 探查任务失败: {}", tool_name, e);
                Some(ProbeVerdict::Unavailable)
            }
        }
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
        session_id: &str,
        run_id: Option<&str>,
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
                    session_id: session_id.to_string(),
                    run_id: run_id.map(str::to_owned),
                };

                // 注册 pending permission
                {
                    let mut map = pending.lock().await;
                    map.insert(request_id.clone(), pending_perm);
                }

                let payload = PermissionRequestPayload {
                    request_id: request_id.clone(),
                    session_id: session_id.to_string(),
                    run_id: run_id.map(str::to_owned),
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

/// 这个会话是不是无人值守（定时任务）跑起来的？
///
/// 判定只看前缀，是刻意的：调度器给作业会话起名 `autonomous:<job_id>`，
/// 而普通对话的 session id 是 uuid。前缀是这条边界唯一的凭据，
/// 所以单独抽出来测——一旦有人把前缀改掉，权限语义就跟着变了。
fn is_unattended_session(session_id: &str) -> bool {
    session_id.starts_with(crate::scheduler::AUTONOMOUS_SESSION_PREFIX)
}

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
        // 普通应用名（如 `qq`、`notepad`）不是文件路径，应先交给程序解析器。
        // 只有用户明确提供路径时，才进入路径策略并使用 Execute 权限校验。
        "launch_program" => args
            .get("name_or_path")
            .and_then(|v| v.as_str())
            .filter(|value| is_explicit_program_path(value))
            .map(|s| s.to_string()),
        "execute_command" => {
            args.get("cwd").and_then(|v| v.as_str()).map(|s| s.to_string())
        }
        _ => None,
    };
    raw.map(|p| expand_windows_env_vars(&p))
}

fn is_explicit_program_path(value: &str) -> bool {
    let path = std::path::Path::new(value);
    path.is_absolute()
        || value.contains('\\')
        || value.contains('/')
        || value.starts_with('.')
        || value.contains('%')
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
    /// 这次调用是否达成契约要求的验证结论
    pub verification: VerificationOutcome,
    /// 回执 ID（写库失败时为空串）
    pub receipt_id: String,
}

impl ToolExecutionResult {
    /// 转换为给 LLM 的 tool 消息内容
    pub fn to_tool_message_content(&self) -> String {
        let mut payload = if self.result.success {
            self.result
                .data
                .clone()
                .unwrap_or_else(|| serde_json::json!({}))
        } else {
            serde_json::json!({
                "error": self.result.error.as_deref().unwrap_or("unknown error"),
                "code": self.result.error_code.as_deref().unwrap_or("UNKNOWN"),
            })
        };

        // 把验证结论一并交给模型。
        //
        // 只记账不告知等于没记：模型看不到结论，就会照着"工具返回成功"
        // 继续往下编。让它在下一轮就看见 `CONTRADICTED` 与判据，
        // 才有机会纠正而不是继续声称完成。
        if self.verification.status != VerificationStatus::NotRequired {
            let mut verdict = serde_json::json!({
                "status": self.verification.status.as_str()
            });
            if let Some(evidence) = &self.verification.evidence {
                verdict["evidence"] = serde_json::Value::String(evidence.clone());
            }
            if !self.verification.missing_fields.is_empty() {
                verdict["missing_fields"] = serde_json::json!(self.verification.missing_fields);
            }

            match &mut payload {
                serde_json::Value::Object(map) => {
                    map.insert("verification".to_string(), verdict);
                }
                _ => {
                    payload = serde_json::json!({
                        "result": payload,
                        "verification": verdict,
                    });
                }
            }
        }

        payload.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::{extract_path_from_args, is_explicit_program_path, is_unattended_session};

    #[test]
    fn unattended_session_is_recognised_by_its_prefix() {
        // 调度器的作业会话以 autonomous: 开头；只有这种会话跳过确认。
        assert!(is_unattended_session("autonomous:job-abc"));
        assert!(is_unattended_session("autonomous:"));
        // 普通会话（含名字里恰好带 autonomous 的）都走正常确认流程
        assert!(!is_unattended_session("sess-autonomous"));
        assert!(!is_unattended_session("autonomousjob"));
        assert!(!is_unattended_session(""));
    }

    #[test]
    fn application_name_is_not_misclassified_as_a_file_path() {
        let args = serde_json::json!({"name_or_path": "qq"});
        assert_eq!(extract_path_from_args(&args, "launch_program"), None);
        assert!(!is_explicit_program_path("notepad.exe"));
    }

    #[test]
    fn explicit_program_path_is_validated() {
        let args = serde_json::json!({
            "name_or_path": r"C:\Program Files\Tencent\QQNT\QQ.exe"
        });
        assert_eq!(
            extract_path_from_args(&args, "launch_program"),
            Some(r"C:\Program Files\Tencent\QQNT\QQ.exe".to_string())
        );
    }

}
