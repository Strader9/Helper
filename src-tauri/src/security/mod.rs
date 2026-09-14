//! 安全模块
//!
//! 三层闸门第二层：PermissionManager —— 权限判定中心。
//! 负责：风险等级校验、白名单检查、路径穿越防护、权限决策。

use serde::{Deserialize, Serialize};

use crate::tools::RiskLevel;

pub mod path_policy;
pub mod command_policy;

pub use path_policy::{
    PathPolicy, PathPolicyResult, PathSecurityLevel, AccessType,
    PATH_DENY_PROTECTED, PATH_DENY_TRAVERSAL, PATH_DENY_SYMLINK_LOOP,
    PATH_DENY_UNC, PATH_DENY_INVALID, PATH_DENY_OUTSIDE_SCOPE,
    load_trusted_dirs_from_db,
};
pub use command_policy::{CommandPolicy, CommandPolicyResult};

// ============================================================
// Phase 4: 权限确认弹窗类型
// ============================================================

/// 用户权限决策
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PermissionDecision {
    /// 允许一次
    AllowOnce,
    /// 始终允许
    AlwaysAllow,
    /// 拒绝
    Deny,
}

/// 权限请求事件负载（Rust → 前端）
#[derive(Debug, Clone, serde::Serialize)]
pub struct PermissionRequestPayload {
    pub request_id: String,
    pub tool_name: String,
    pub description: String,
    pub risk_level: String,
    pub path: Option<String>,
}

/// 权限响应（前端 → Rust）
#[derive(Debug, Clone, serde::Deserialize)]
pub struct PermissionResponse {
    pub request_id: String,
    pub decision: PermissionDecision,
}

/// 权限判定结果
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum PermissionResult {
    /// 允许执行
    Allowed,
    /// 需要用户确认
    ConfirmationRequired,
    /// 拒绝执行
    Denied {
        reason: String,
        code: String,
    },
}

/// 拒绝原因码
pub const DENY_TOOL_NOT_FOUND: &str = "TOOL_NOT_FOUND";
pub const DENY_BLACKLISTED: &str = "BLACKLISTED";
pub const DENY_CRITICAL_OPERATION: &str = "CRITICAL_OPERATION";
pub const DENY_USER_DENIED: &str = "USER_DENIED";
pub const DENY_PATH_TRAVERSAL: &str = "PATH_TRAVERSAL";

/// 权限管理器
///
/// 三层闸门第二层：唯一有权限放行/拒绝的模块。
///
/// 权限判定流程：
/// 1. 工具是否已注册 → 未注册则拒绝
/// 2. 获取工具 riskLevel
/// 3. 检查路径/协议/进程黑名单 → 命中则拒绝
/// 4. 检查 trusted_tools 白名单 → 命中则自动放行
/// 5. 根据 riskLevel 决定策略：
///    - SAFE/LOW → 直接放行
///    - MEDIUM/HIGH → 检查白名单，不在白名单则请求用户确认
///    - CRITICAL → 禁止执行
pub struct PermissionManager {
    // 配置项（从 settings 表加载）
    confirm_medium_risk: bool,
    // 路径安全策略
    path_policy: PathPolicy,
    // 已信任工具列表（从 trusted_tools 表加载）
    trusted_tools: Vec<TrustedToolEntry>,
}

/// 已信任工具条目
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrustedToolEntry {
    pub tool_name: String,
    pub path_pattern: Option<String>,
    pub risk_level: String,
}

impl PermissionManager {
    /// 创建权限管理器
    pub fn new() -> Self {
        Self {
            confirm_medium_risk: true,
            path_policy: PathPolicy::new(),
            trusted_tools: Vec::new(),
        }
    }

    /// 创建并加载数据库配置
    pub fn with_db(db_conn: &rusqlite::Connection) -> Self {
        let mut manager = Self::new();

        // 加载可信目录
        match load_trusted_dirs_from_db(db_conn) {
            Ok(dirs) => manager.path_policy.set_trusted_dirs(dirs),
            Err(e) => eprintln!("加载可信目录失败: {}", e),
        }

        // 加载 trusted_tools
        match load_trusted_tools_from_db(db_conn) {
            Ok(tools) => manager.trusted_tools = tools,
            Err(e) => eprintln!("加载信任工具失败: {}", e),
        }

        manager
    }

    /// 更新配置
    pub fn update_config(&mut self, confirm_medium_risk: bool) {
        self.confirm_medium_risk = confirm_medium_risk;
    }

    /// 获取路径策略引用
    pub fn path_policy(&self) -> &PathPolicy {
        &self.path_policy
    }

    /// 获取路径策略可变引用
    pub fn path_policy_mut(&mut self) -> &mut PathPolicy {
        &mut self.path_policy
    }

    /// 重新加载可信目录和信任工具
    pub fn reload_trusted_data(&mut self, db_conn: &rusqlite::Connection) {
        match load_trusted_dirs_from_db(db_conn) {
            Ok(dirs) => self.path_policy.set_trusted_dirs(dirs),
            Err(e) => eprintln!("重新加载可信目录失败: {}", e),
        }
        match load_trusted_tools_from_db(db_conn) {
            Ok(tools) => self.trusted_tools = tools,
            Err(e) => eprintln!("重新加载信任工具失败: {}", e),
        }
    }

    /// 判定工具调用权限
    ///
    /// 检查工具是否在 trusted_tools 白名单中，如果是则自动放行。
    /// 然后按照风险等级决定是否需要确认。
    ///
    /// # Arguments
    /// * `risk_level` - 工具风险等级
    /// * `tool_name` - 工具名称
    /// * `path` - 可选路径参数（用于路径安全校验）
    ///
    /// # Returns
    /// 权限判定结果
    pub fn check_permission(
        &self,
        risk_level: RiskLevel,
        tool_name: &str,
        path: Option<&str>,
    ) -> PermissionResult {
        // 1. 检查 trusted_tools 白名单
        if self.is_tool_trusted(tool_name, risk_level, path) {
            return PermissionResult::Allowed;
        }

        // 2. 路径安全校验（如果提供了路径）
        if let Some(path_str) = path {
            // 根据工具类型推断访问类型
            let access_type = infer_access_type(tool_name);
            match self.path_policy.validate_path(path_str, access_type) {
                PathPolicyResult::Allow => {}
                PathPolicyResult::ConfirmationRequired => {
                    // 路径安全策略要求确认，但还要结合风险等级
                    // 如果风险等级已经是 MEDIUM/HIGH，直接返回 ConfirmationRequired
                    // 如果是 LOW，路径策略要求确认，说明是写操作在 USER 目录
                    return PermissionResult::ConfirmationRequired;
                }
                PathPolicyResult::Deny { reason, code } => {
                    return PermissionResult::Denied { reason, code };
                }
            }
        }

        // 3. 按风险等级判定
        match risk_level {
            RiskLevel::Safe | RiskLevel::Low => PermissionResult::Allowed,
            RiskLevel::Medium => {
                if self.confirm_medium_risk {
                    PermissionResult::ConfirmationRequired
                } else {
                    PermissionResult::Allowed
                }
            }
            RiskLevel::High => PermissionResult::ConfirmationRequired,
            RiskLevel::Critical => PermissionResult::Denied {
                reason: "CRITICAL 级操作禁止执行".to_string(),
                code: DENY_CRITICAL_OPERATION.to_string(),
            },
        }
    }

    /// 检查工具是否已信任
    fn is_tool_trusted(&self, tool_name: &str, risk_level: RiskLevel, path: Option<&str>) -> bool {
        for entry in &self.trusted_tools {
            if entry.tool_name == tool_name {
                // 检查风险等级是否匹配
                if entry.risk_level == risk_level.as_str() || entry.risk_level == "ALL" {
                    // 检查路径模式（如果存在）
                    if let Some(pattern) = &entry.path_pattern {
                        if let Some(path_str) = path {
                            // 简单前缀匹配（大小写不敏感）
                            let path_lower = path_str.to_lowercase();
                            let pattern_lower = pattern.to_lowercase();
                            if path_lower.starts_with(&pattern_lower) {
                                return true;
                            }
                        } else {
                            // 路径模式存在但没有路径参数，不匹配
                            return false;
                        }
                    } else {
                        // 没有路径模式，全局信任
                        return true;
                    }
                }
            }
        }
        false
    }

    /// 校验文件路径是否在允许目录内（防路径穿越）
    ///
    /// 委托给 PathPolicy 实现完整路径安全校验。
    ///
    /// # Arguments
    /// * `path` - 待校验的路径
    /// * `access_type` - 访问类型（读/写/删除）
    ///
    /// # Returns
    /// PathPolicyResult
    pub fn validate_path(
        &self,
        path: &str,
        access_type: AccessType,
    ) -> PathPolicyResult {
        self.path_policy.validate_path(path, access_type)
    }

    /// 系统目录黑名单（硬编码）
    ///
    /// 这些目录下的可执行文件默认禁止操作。
    pub fn system_blacklist_dirs() -> Vec<&'static str> {
        vec![
            "C:\\Windows\\",
            "C:\\Program Files\\",
            "C:\\Program Files (x86)\\",
            "C:\\ProgramData\\",
            "C:\\System Volume Information\\",
        ]
    }
}

impl Default for PermissionManager {
    fn default() -> Self {
        Self::new()
    }
}

// ============================================================
// 工具访问类型推断
// ============================================================

fn infer_access_type(tool_name: &str) -> AccessType {
    match tool_name {
        "read_file" | "list_directory" | "open_file" | "find_program" | "take_screenshot" => AccessType::Read,
        "write_file" => AccessType::Write,
        "delete_file" | "move_file" => AccessType::Delete,
        "execute_command" => AccessType::Write, // 命令执行视为写操作
        "launch_program" => AccessType::Read,   // 启动程序视为读操作
        _ => AccessType::Read,
    }
}

// ============================================================
// 数据库加载
// ============================================================

fn load_trusted_tools_from_db(conn: &rusqlite::Connection) -> crate::error::AppResult<Vec<TrustedToolEntry>> {
    let mut stmt = conn.prepare(
        "SELECT tool_name, path_pattern, risk_level FROM trusted_tools"
    )?;

    let rows = stmt.query_map([], |row| {
        Ok(TrustedToolEntry {
            tool_name: row.get(0)?,
            path_pattern: row.get(1)?,
            risk_level: row.get(2)?,
        })
    })?;

    let mut tools = Vec::new();
    for row in rows {
        if let Ok(entry) = row {
            tools.push(entry);
        }
    }

    Ok(tools)
}
