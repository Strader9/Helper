//! 路径安全策略
//!
//! PathPolicy —— 文件系统操作前的路径安全检查。
//! 负责：路径遍历防护、系统目录保护、白名单校验。

use std::path::{Path, PathBuf};

/// 路径验证结果
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PathValidationResult {
    /// 路径在白名单内，允许操作
    Allowed,
    /// 路径在系统目录黑名单内，拒绝操作
    Denied { reason: String },
    /// 路径在白名单外但非系统目录，需要用户确认
    NeedsConfirmation,
}

/// 路径安全策略
///
/// 维护白名单目录和系统目录黑名单，提供路径校验功能。
pub struct PathPolicy;

impl PathPolicy {
    /// 校验路径安全性
    ///
    /// 检查流程：
    /// 1. 检测路径遍历（包含 `..`）
    /// 2. 检测符号链接 / junction 指向系统目录
    /// 3. 检测 UNC path
    /// 4. 检查系统目录黑名单
    /// 5. 检查白名单
    pub fn validate_path(path: &Path) -> PathValidationResult {
        // 1. 检测路径遍历
        if Self::contains_traversal(path) {
            return PathValidationResult::Denied {
                reason: "路径包含遍历序列（..），可能存在安全风险".to_string(),
            };
        }

        // 2. 检测 UNC path
        if Self::is_unc_path(path) {
            return PathValidationResult::Denied {
                reason: "UNC 路径不被允许".to_string(),
            };
        }

        // 3. 规范化路径
        let canonical = match Self::canonicalize_safe(path) {
            Some(p) => p,
            None => {
                // 文件不存在时无法 canonicalize，使用规范化后的路径继续检查
                path.to_path_buf()
            }
        };

        // 4. 检查符号链接 / junction 指向
        if let Ok(metadata) = std::fs::symlink_metadata(&canonical) {
            if metadata.file_type().is_symlink() {
                // 是符号链接，检查其目标
                if let Ok(target) = std::fs::read_link(&canonical) {
                    if Self::is_system_path(&target) {
                        return PathValidationResult::Denied {
                            reason: "符号链接指向系统目录".to_string(),
                        };
                    }
                }
            }
        }

        // 5. 检查系统目录黑名单
        if Self::is_system_path(&canonical) {
            return PathValidationResult::Denied {
                reason: "路径位于系统保护目录内".to_string(),
            };
        }

        // 6. 检查白名单
        if Self::is_in_whitelist(&canonical) {
            return PathValidationResult::Allowed;
        }

        // 白名单外但非系统目录 → 需要确认
        PathValidationResult::NeedsConfirmation
    }

    /// 检测路径是否包含遍历序列
    fn contains_traversal(path: &Path) -> bool {
        path.components().any(|c| {
            matches!(c, std::path::Component::ParentDir)
        })
    }

    /// 检测是否为 UNC 路径
    fn is_unc_path(path: &Path) -> bool {
        path.to_string_lossy().starts_with("\\\\")
    }

    /// 安全规范化路径（不 panic）
    fn canonicalize_safe(path: &Path) -> Option<PathBuf> {
        // 如果路径不存在，尝试规范化父目录
        if path.exists() {
            dunce::canonicalize(path).ok()
        } else {
            None
        }
    }

    /// 检查路径是否在系统目录黑名单内
    fn is_system_path(path: &Path) -> bool {
        let path_str = path.to_string_lossy().to_lowercase();
        let system_dirs = Self::system_blacklist();

        system_dirs.iter().any(|&dir| {
            let dir_lower = dir.to_lowercase();
            path_str.starts_with(&dir_lower)
        })
    }

    /// 检查路径是否在白名单内
    fn is_in_whitelist(path: &Path) -> bool {
        let whitelist = Self::whitelist_dirs();
        let path_str = path.to_string_lossy().to_lowercase();

        whitelist.iter().any(|&dir| {
            let dir_lower = dir.to_lowercase();
            path_str.starts_with(&dir_lower)
        })
    }

    /// 系统目录黑名单
    fn system_blacklist() -> Vec<&'static str> {
        vec![
            // Windows 系统目录
            r"C:\Windows",
            r"C:\Program Files",
            r"C:\Program Files (x86)",
            r"C:\ProgramData",
            r"C:\System Volume Information",
            r"C:\$Recycle.Bin",
            r"C:\Users\All Users",
            r"C:\Users\Default",
            r"C:\Users\Public",
            // Windows 系统文件
            r"C:\bootmgr",
            r"C:\BOOTNXT",
            r"C:\pagefile.sys",
            r"C:\hiberfil.sys",
            r"C:\swapfile.sys",
            // 注册表 / 系统配置（通过路径防护）
            r"C:\Windows\System32",
            r"C:\Windows\SysWOW64",
            r"C:\Windows\WinSxS",
            r"C:\Windows\Installer",
        ]
    }

    /// 白名单目录（用户主目录下的常用目录）
    fn whitelist_dirs() -> Vec<String> {
        let mut dirs = Vec::new();

        // 获取用户主目录
        if let Some(home) = dirs::home_dir() {
            let home_str = home.to_string_lossy().to_string();

            // 可读写目录
            dirs.push(format!(r"{}\Documents", home_str));
            dirs.push(format!(r"{}\Downloads", home_str));
            dirs.push(format!(r"{}\Desktop", home_str));

            // 只读目录（实际权限由调用方控制，这里只标记为白名单内）
            dirs.push(format!(r"{}\Pictures", home_str));
            dirs.push(format!(r"{}\Videos", home_str));
            dirs.push(format!(r"{}\Music", home_str));

            // 其他常用目录
            dirs.push(format!(r"{}\AppData\Local", home_str));
            dirs.push(format!(r"{}\AppData\Roaming", home_str));
        }

        dirs
    }
}

// ============================================================
// 工具辅助函数
// ============================================================

/// 从工具参数中提取路径
///
/// 支持常见参数名：path、file_path、directory、dir、target_path
pub fn extract_path_from_args(args: &serde_json::Value) -> Option<String> {
    let keys = ["path", "file_path", "directory", "dir", "target_path", "file"];

    for key in &keys {
        if let Some(path) = args.get(key).and_then(|v| v.as_str()) {
            return Some(path.to_string());
        }
    }

    None
}

/// 检查工具是否为文件系统相关工具
pub fn is_filesystem_tool(tool_name: &str) -> bool {
    matches!(
        tool_name,
        "read_file" | "write_file" | "open_file" | "list_directory" | "open_directory"
    )
}
