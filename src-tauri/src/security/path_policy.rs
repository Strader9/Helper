//! 路径安全策略
//!
//! 定义文件系统访问的安全等级和校验规则，
//! 防止越权访问和路径遍历攻击。

use std::path::{Path, PathBuf};

use crate::error::AppResult;

/// 路径访问安全等级
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PathSecurityLevel {
    /// 禁止访问（系统关键目录、注册表等）
    Protected,
    /// 仅只读（低风险系统目录）
    Restricted,
    /// 用户目录（可读写，但需确认）
    User,
    /// 可信目录（完全开放）
    Trusted,
}

/// 访问权限类型
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccessType {
    Read,
    Write,
    Delete,
}

/// 路径策略判定结果
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PathPolicyResult {
    /// 允许访问
    Allow,
    /// 需要确认（写操作在 USER 目录）
    ConfirmationRequired,
    /// 拒绝访问
    Deny { reason: String, code: String },
}

/// 路径安全策略
///
/// 定义不同目录的安全等级和访问规则。
/// 所有文件系统操作必须通过此策略校验。
pub struct PathPolicy {
    /// 可信目录列表（用户自定义 + 默认）
    trusted_dirs: Vec<PathBuf>,
    /// 用户目录列表（默认）
    user_dirs: Vec<PathBuf>,
    /// 系统黑名单目录（硬编码）
    protected_dirs: Vec<PathBuf>,
    /// 仅读目录（硬编码）
    restricted_dirs: Vec<PathBuf>,
}

/// 路径拒绝原因码
pub const PATH_DENY_PROTECTED: &str = "PATH_PROTECTED";
pub const PATH_DENY_TRAVERSAL: &str = "PATH_TRAVERSAL";
pub const PATH_DENY_SYMLINK_LOOP: &str = "PATH_SYMLINK_LOOP";
pub const PATH_DENY_UNC: &str = "PATH_UNC";
pub const PATH_DENY_INVALID: &str = "PATH_INVALID";
pub const PATH_DENY_OUTSIDE_SCOPE: &str = "PATH_OUTSIDE_SCOPE";

impl PathPolicy {
    /// 创建路径策略（使用系统默认值）
    pub fn new() -> Self {
        let mut policy = Self {
            trusted_dirs: Vec::new(),
            user_dirs: Vec::new(),
            protected_dirs: Vec::new(),
            restricted_dirs: Vec::new(),
        };
        policy.init_default_dirs();
        policy
    }

    /// 从可信目录列表创建策略
    pub fn with_trusted_dirs(dirs: Vec<PathBuf>) -> Self {
        let mut policy = Self::new();
        policy.trusted_dirs.extend(dirs);
        policy
    }

    /// 初始化默认目录
    fn init_default_dirs(&mut self) {
        #[cfg(target_os = "windows")]
        {
            // 系统关键目录（PROTECTED）
            self.protected_dirs = vec![
                PathBuf::from("C:\\Windows"),
                PathBuf::from("C:\\Program Files"),
                PathBuf::from("C:\\Program Files (x86)"),
                PathBuf::from("C:\\ProgramData"),
                PathBuf::from("C:\\System Volume Information"),
                PathBuf::from("C:\\$Recycle.Bin"),
                PathBuf::from("C:\\Boot"),
                PathBuf::from("C:\\Config.Msi"),
                PathBuf::from("C:\\Recovery"),
                PathBuf::from("C:\\EFI"),
                PathBuf::from("C:\\inetpub"),
            ];

            // 仅读目录（RESTRICTED）
            self.restricted_dirs = vec![];

            // 用户目录（USER）- 使用环境变量解析
            if let Ok(user_profile) = std::env::var("USERPROFILE") {
                let user = PathBuf::from(&user_profile);
                self.user_dirs = vec![
                    user.join("Desktop"),
                    user.join("Documents"),
                    user.join("Downloads"),
                    user.join("Pictures"),
                    user.join("Music"),
                    user.join("Videos"),
                ];
                // AppData 视为 RESTRICTED（只读）
                self.restricted_dirs.push(user.join("AppData"));
            }

            // 系统根目录（除了用户目录外）
            if let Ok(system_root) = std::env::var("SystemRoot") {
                self.protected_dirs.push(PathBuf::from(system_root));
            }
        }

        #[cfg(not(target_os = "windows"))]
        {
            // Linux/macOS 默认目录
            self.protected_dirs = vec![
                PathBuf::from("/bin"),
                PathBuf::from("/sbin"),
                PathBuf::from("/usr/bin"),
                PathBuf::from("/usr/sbin"),
                PathBuf::from("/etc"),
                PathBuf::from("/sys"),
                PathBuf::from("/proc"),
                PathBuf::from("/dev"),
                PathBuf::from("/root"),
                PathBuf::from("/var/lib"),
                PathBuf::from("/usr/lib"),
                PathBuf::from("/usr/share"),
            ];

            self.restricted_dirs = vec![
                PathBuf::from("/tmp"),
                PathBuf::from("/var/tmp"),
            ];

            if let Ok(home) = std::env::var("HOME") {
                let home = PathBuf::from(&home);
                self.user_dirs = vec![
                    home.join("Desktop"),
                    home.join("Documents"),
                    home.join("Downloads"),
                    home.join("Pictures"),
                    home.join("Music"),
                    home.join("Videos"),
                ];
            }
        }
    }

    /// 添加可信目录
    pub fn add_trusted_dir(&mut self, path: PathBuf) {
        self.trusted_dirs.push(path);
    }

    /// 设置可信目录列表
    pub fn set_trusted_dirs(&mut self, dirs: Vec<PathBuf>) {
        self.trusted_dirs = dirs;
    }

    /// 校验路径访问权限
    ///
    /// 主要校验逻辑：
    /// 1. 规范化路径（canonicalize）
    /// 2. 检查路径遍历（..）
    /// 3. 检查符号链接（最多 3 层）
    /// 4. 检查 UNC 路径
    /// 5. 检查环境变量注入
    /// 6. 匹配安全等级
    /// 7. 根据访问类型决定允许/确认/拒绝
    pub fn validate_path(
        &self,
        path: &str,
        access_type: AccessType,
    ) -> PathPolicyResult {
        // 1. 基础检查：空路径、环境变量注入
        if path.trim().is_empty() {
            return PathPolicyResult::Deny {
                reason: "路径为空".to_string(),
                code: PATH_DENY_INVALID.to_string(),
            };
        }

        // 检查环境变量注入（如 %SystemRoot%\..\Windows）
        if path.contains('%') {
            return PathPolicyResult::Deny {
                reason: "路径包含环境变量，请使用完整路径".to_string(),
                code: PATH_DENY_INVALID.to_string(),
            };
        }

        // 2. 检查 UNC 路径
        if is_unc_path(path) {
            return PathPolicyResult::Deny {
                reason: "UNC 网络路径被禁止".to_string(),
                code: PATH_DENY_UNC.to_string(),
            };
        }

        // 3. 规范化路径
        let normalized = match normalize_path(path) {
            Ok(p) => p,
            Err(e) => {
                return PathPolicyResult::Deny {
                    reason: format!("路径无效: {}", e),
                    code: PATH_DENY_INVALID.to_string(),
                };
            }
        };

        // 4. 检查路径遍历：规范化后如果在不同根目录下，说明有问题
        if let Some(traversal_result) = check_path_traversal(path, &normalized) {
            return traversal_result;
        }

        // 5. 检查符号链接（最多 3 层）
        match self.resolve_symlinks(&normalized) {
            Ok(resolved) => {
                // 解析后重新检查路径遍历
                if let Some(traversal_result) = check_path_traversal(path, &resolved) {
                    return traversal_result;
                }
            }
            Err(e) => {
                return PathPolicyResult::Deny {
                    reason: e,
                    code: PATH_DENY_SYMLINK_LOOP.to_string(),
                };
            }
        }

        // 6. 匹配安全等级
        let level = self.classify_path(&normalized);

        // 7. 根据安全等级和访问类型决定结果
        match level {
            PathSecurityLevel::Protected => PathPolicyResult::Deny {
                reason: format!(
                    "路径 '{}' 属于系统保护目录，禁止访问",
                    normalized.display()
                ),
                code: PATH_DENY_PROTECTED.to_string(),
            },
            PathSecurityLevel::Restricted => match access_type {
                AccessType::Read => PathPolicyResult::Allow,
                AccessType::Write | AccessType::Delete => PathPolicyResult::Deny {
                    reason: format!(
                        "路径 '{}' 属于受限目录，禁止写入/删除",
                        normalized.display()
                    ),
                    code: PATH_DENY_PROTECTED.to_string(),
                },
            },
            PathSecurityLevel::User => match access_type {
                AccessType::Read => PathPolicyResult::Allow,
                AccessType::Write | AccessType::Delete => PathPolicyResult::ConfirmationRequired,
            },
            PathSecurityLevel::Trusted => PathPolicyResult::Allow,
        }
    }

    /// 分类路径的安全等级
    fn classify_path(&self, path: &Path) -> PathSecurityLevel {
        let path_str = path.to_string_lossy().to_lowercase();

        // 检查是否在 PROTECTED 目录中
        for protected in &self.protected_dirs {
            let protected_lower = protected.to_string_lossy().to_lowercase();
            if path_str.starts_with(&protected_lower) {
                return PathSecurityLevel::Protected;
            }
        }

        // 检查是否在 RESTRICTED 目录中
        for restricted in &self.restricted_dirs {
            let restricted_lower = restricted.to_string_lossy().to_lowercase();
            if path_str.starts_with(&restricted_lower) {
                return PathSecurityLevel::Restricted;
            }
        }

        // 检查是否在 TRUSTED 目录中
        for trusted in &self.trusted_dirs {
            let trusted_lower = trusted.to_string_lossy().to_lowercase();
            if path_str.starts_with(&trusted_lower) {
                return PathSecurityLevel::Trusted;
            }
        }

        // 检查是否在 USER 目录中
        for user in &self.user_dirs {
            let user_lower = user.to_string_lossy().to_lowercase();
            if path_str.starts_with(&user_lower) {
                return PathSecurityLevel::User;
            }
        }

        // 默认：如果路径在用户目录下（如 USERPROFILE\xxx），也视为 USER
        #[cfg(target_os = "windows")]
        {
            if let Ok(user_profile) = std::env::var("USERPROFILE") {
                let profile_lower = user_profile.to_lowercase();
                if path_str.starts_with(&profile_lower) {
                    return PathSecurityLevel::User;
                }
            }
        }
        #[cfg(not(target_os = "windows"))]
        {
            if let Ok(home) = std::env::var("HOME") {
                let home_lower = home.to_lowercase();
                if path_str.starts_with(&home_lower) {
                    return PathSecurityLevel::User;
                }
            }
        }

        // 不在任何已知目录中：默认可用（USER 级别），允许读、写/删需确认
        PathSecurityLevel::User
    }

    /// 解析符号链接（最多 3 层）
    fn resolve_symlinks(&self, path: &Path) -> Result<PathBuf, String> {
        let mut current = path.to_path_buf();
        let mut depth = 0;
        const MAX_SYMLINK_DEPTH: usize = 3;

        while depth < MAX_SYMLINK_DEPTH {
            #[cfg(target_os = "windows")]
            {
                // Windows: 检查 junction / reparse point
                match is_reparse_point(&current) {
                    Ok(true) => {
                        match resolve_reparse_point(&current) {
                            Ok(target) => current = target,
                            Err(e) => return Err(format!("无法解析符号链接: {}", e)),
                        }
                    }
                    Ok(false) => break,
                    Err(e) => return Err(format!("检查符号链接失败: {}", e)),
                }
            }
            #[cfg(not(target_os = "windows"))]
            {
                match std::fs::read_link(&current) {
                    Ok(target) => {
                        current = if target.is_absolute() {
                            target
                        } else {
                            current.parent().unwrap_or(Path::new("/")).join(target)
                        };
                    }
                    Err(_) => break,
                }
            }
            depth += 1;
        }

        if depth >= MAX_SYMLINK_DEPTH {
            return Err("符号链接嵌套层数超过限制（最多 3 层）".to_string());
        }

        Ok(current)
    }

    /// 获取安全等级描述（用于前端展示）
    pub fn level_description(level: PathSecurityLevel) -> &'static str {
        match level {
            PathSecurityLevel::Protected => "系统保护目录",
            PathSecurityLevel::Restricted => "受限目录（只读）",
            PathSecurityLevel::User => "用户目录",
            PathSecurityLevel::Trusted => "可信目录",
        }
    }
}

impl Default for PathPolicy {
    fn default() -> Self {
        Self::new()
    }
}

// ============================================================
// 路径规范化
// ============================================================

/// 规范化路径（使用 dunce 处理 Windows UNC 前缀）
fn normalize_path(path: &str) -> AppResult<PathBuf> {
    let path = PathBuf::from(path);

    // 使用 dunce 规范化（处理 Windows 的 UNC 前缀和路径分隔符）
    let normalized = dunce::simplified(&path);

    // 转换为绝对路径（如果相对路径的话）
    let absolute = if normalized.is_absolute() {
        normalized.to_path_buf()
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(normalized)
    };

    // 清理路径中的 . 和 ..（不依赖文件系统存在性）
    let cleaned = clean_path(&absolute);

    Ok(cleaned)
}

/// 手动清理路径中的 . 和 ..（不依赖文件系统存在性）
fn clean_path(path: &Path) -> PathBuf {
    let mut result = PathBuf::new();

    for component in path.components() {
        match component {
            std::path::Component::Prefix(p) => {
                result.push(p.as_os_str());
            }
            std::path::Component::RootDir => {
                result.push(component.as_os_str());
            }
            std::path::Component::CurDir => {
                // 忽略 .
            }
            std::path::Component::ParentDir => {
                // 处理 ..：只有当路径有实际内容时才 pop
                if result.file_name().is_some() {
                    result.pop();
                }
            }
            std::path::Component::Normal(name) => {
                result.push(name);
            }
        }
    }

    result
}

// ============================================================
// 路径遍历检查
// ============================================================

/// 检查路径是否包含遍历攻击特征
fn check_path_traversal(original: &str, normalized: &Path) -> Option<PathPolicyResult> {
    let _original_lower = original.to_lowercase();

    // 检查原始路径中的 .. 序列是否试图穿越到系统目录
    if original.contains("..") || original.contains("./") || original.contains(".\\") {
        // 规范化后如果在不同盘符或根目录，可能是遍历攻击
        let orig_path = PathBuf::from(original);
        if let (Some(_orig_parent), Some(_norm_parent)) = (
            orig_path.parent(),
            normalized.parent(),
        ) {
            // 如果原始路径在父目录，但规范化后跨到了不同的目录树
            if orig_path.components().count() < normalized.components().count() {
                // 路径变长了，说明可能在利用 .. 和符号链接穿越
                return Some(PathPolicyResult::Deny {
                    reason: "检测到路径遍历攻击".to_string(),
                    code: PATH_DENY_TRAVERSAL.to_string(),
                });
            }
        }
    }

    // 检查规范化后的路径是否包含 ..（说明路径不存在或无法解析）
    let normalized_str = normalized.to_string_lossy();
    if normalized_str.contains("..") {
        return Some(PathPolicyResult::Deny {
            reason: "路径包含非法的 .. 序列".to_string(),
            code: PATH_DENY_TRAVERSAL.to_string(),
        });
    }

    None
}

// ============================================================
// UNC 路径检查
// ============================================================

/// 检查是否为 UNC 路径
fn is_unc_path(path: &str) -> bool {
    path.starts_with("\\\\") || path.starts_with("//")
}

// ============================================================
// Windows 符号链接检查
// ============================================================

#[cfg(target_os = "windows")]
fn is_reparse_point(path: &Path) -> std::io::Result<bool> {
    use std::os::windows::fs::MetadataExt;
    match std::fs::symlink_metadata(path) {
        Ok(meta) => {
            let file_attr = meta.file_attributes();
            // FILE_ATTRIBUTE_REPARSE_POINT = 0x400
            Ok((file_attr & 0x400) != 0)
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e),
    }
}

#[cfg(target_os = "windows")]
fn resolve_reparse_point(path: &Path) -> std::io::Result<PathBuf> {
    // Windows 上使用 std::fs::canonicalize 来解析符号链接
    // 但 canonicalize 会添加 UNC 前缀，所以这里用 read_link
    std::fs::read_link(path)
}

#[cfg(not(target_os = "windows"))]
fn is_reparse_point(_path: &Path) -> std::io::Result<bool> {
    Ok(false)
}

#[cfg(not(target_os = "windows"))]
fn resolve_reparse_point(_path: &Path) -> std::io::Result<PathBuf> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "Reparse points only on Windows",
    ))
}

// ============================================================
// 从数据库加载可信目录
// ============================================================

use rusqlite::Connection;

/// 从数据库加载允许目录并转换为 PathBuf 列表
pub fn load_trusted_dirs_from_db(conn: &Connection) -> AppResult<Vec<PathBuf>> {
    let mut stmt = conn.prepare(
        "SELECT path FROM allowed_directories WHERE access_level IN ('TRUSTED', 'read', 'write')"
    )?;

    let rows = stmt.query_map([], |row| {
        let path_str: String = row.get(0)?;
        // 展开环境变量
        let expanded = expand_env_vars(&path_str);
        Ok(PathBuf::from(expanded))
    })?;

    let mut dirs = Vec::new();
    for row in rows {
        if let Ok(path) = row {
            dirs.push(path);
        }
    }

    Ok(dirs)
}

/// 展开 Windows 环境变量（如 %USERPROFILE%）
fn expand_env_vars(path: &str) -> String {
    #[cfg(target_os = "windows")]
    {
        let mut result = path.to_string();
        // 展开 %USERPROFILE%
        if let Ok(userprofile) = std::env::var("USERPROFILE") {
            result = result.replace("%USERPROFILE%", &userprofile);
        }
        // 展开其他常见变量
        if let Ok(systemroot) = std::env::var("SystemRoot") {
            result = result.replace("%SystemRoot%", &systemroot);
        }
        if let Ok(programfiles) = std::env::var("ProgramFiles") {
            result = result.replace("%ProgramFiles%", &programfiles);
        }
        if let Ok(localappdata) = std::env::var("LOCALAPPDATA") {
            result = result.replace("%LOCALAPPDATA%", &localappdata);
        }
        if let Ok(appdata) = std::env::var("APPDATA") {
            result = result.replace("%APPDATA%", &appdata);
        }
        result
    }
    #[cfg(not(target_os = "windows"))]
    {
        let mut result = path.to_string();
        if let Ok(home) = std::env::var("HOME") {
            result = result.replace("$HOME", &home);
            result = result.replace("~/", &format!("{}/", home));
        }
        result
    }
}
