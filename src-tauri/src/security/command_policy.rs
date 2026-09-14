//! 命令安全策略
//!
//! 对 execute_command 工具的命令进行安全校验，阻止危险操作。
//! 采用黑名单 + 危险模式检测，后续可升级为白名单模式。


/// 命令策略判定结果
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommandPolicyResult {
    /// 允许执行
    Allow,
    /// 拒绝执行
    Deny { reason: String, code: String },
}

/// 危险命令拒绝码
pub const DENY_CMD_FORMAT: &str = "CMD_FORMAT";
pub const DENY_CMD_DISKPART: &str = "CMD_DISKPART";
pub const DENY_CMD_REGISTRY: &str = "CMD_REGISTRY";
pub const DENY_CMD_BOOT: &str = "CMD_BOOT";
pub const DENY_CMD_SHUTDOWN: &str = "CMD_SHUTDOWN";
pub const DENY_CMD_DELETE_BULK: &str = "CMD_DELETE_BULK";
pub const DENY_CMD_PERMISSION: &str = "CMD_PERMISSION";
pub const DENY_CMD_USER_MGMT: &str = "CMD_USER_MGMT";
pub const DENY_CMD_SERVICE: &str = "CMD_SERVICE";
pub const DENY_CMD_ENCODED: &str = "CMD_ENCODED";
pub const DENY_CMD_NESTED: &str = "CMD_NESTED_SHELL";
pub const DENY_CMD_WIPE: &str = "CMD_WIPE";

/// 命令安全策略器
pub struct CommandPolicy;

impl CommandPolicy {
    /// 校验命令是否允许执行
    ///
    /// # Arguments
    /// * `command` - 要执行的命令字符串
    /// * `args` - 额外参数列表
    ///
    /// # Returns
    /// CommandPolicyResult
    pub fn check(command: &str, args: &[String]) -> CommandPolicyResult {
        let full_cmd = if args.is_empty() {
            command.to_string()
        } else {
            format!("{} {}", command, args.join(" "))
        };
        let cmd_lower = full_cmd.to_lowercase();

        // 1. 格式化磁盘
        if cmd_lower.contains("format ") {
            return deny(DENY_CMD_FORMAT, "禁止执行格式化命令");
        }

        // 2. 磁盘分区工具
        if cmd_lower.contains("diskpart") {
            return deny(DENY_CMD_DISKPART, "禁止执行 diskpart 磁盘分区命令");
        }

        // 3. 注册表修改
        if cmd_lower.contains("reg delete") || cmd_lower.contains("reg add") {
            return deny(DENY_CMD_REGISTRY, "禁止修改注册表");
        }

        // 4. 启动配置
        if cmd_lower.contains("bcdedit") {
            return deny(DENY_CMD_BOOT, "禁止修改启动配置（bcdedit）");
        }

        // 5. 关机/重启
        if cmd_lower.contains("shutdown") || cmd_lower.contains("restart-computer") {
            return deny(DENY_CMD_SHUTDOWN, "禁止执行关机/重启命令");
        }

        // 6. 批量删除
        if (cmd_lower.contains("del ") && cmd_lower.contains(" /s"))
            || (cmd_lower.contains("rmdir ") && cmd_lower.contains(" /s"))
            || (cmd_lower.contains("rd ") && cmd_lower.contains(" /s"))
            || cmd_lower.contains("remove-item -recurse")
        {
            return deny(DENY_CMD_DELETE_BULK, "禁止执行递归批量删除命令");
        }

        // 7. 权限篡改
        if cmd_lower.contains("takeown") || cmd_lower.contains("icacls ") && cmd_lower.contains("/grant") {
            return deny(DENY_CMD_PERMISSION, "禁止篡改文件权限");
        }

        // 8. 用户管理
        if cmd_lower.contains("net user") || cmd_lower.contains("net localgroup") {
            return deny(DENY_CMD_USER_MGMT, "禁止操作用户账户");
        }

        // 9. 服务管理（删除服务）
        if cmd_lower.contains("sc delete") {
            return deny(DENY_CMD_SERVICE, "禁止删除系统服务");
        }

        // 10. 数据擦除
        if cmd_lower.contains("cipher /w") {
            return deny(DENY_CMD_WIPE, "禁止执行数据擦除命令（cipher /w）");
        }

        // 11. PowerShell 编码执行（Base64 混淆）
        if cmd_lower.contains("-encodedcommand") || cmd_lower.contains("-enc ") {
            return deny(DENY_CMD_ENCODED, "禁止执行编码混淆的 PowerShell 命令");
        }

        // 12. 多级 shell 嵌套
        let shell_count = count_shell_invocations(&cmd_lower);
        if shell_count >= 2 {
            return deny(DENY_CMD_NESTED, "禁止多级 shell 嵌套执行");
        }

        // 13. 禁止直接调用 powershell 执行任意脚本（需要显式参数）
        // 注意：powershell 本身不禁止，但 -Command 后接危险内容由上面规则覆盖

        CommandPolicyResult::Allow
    }
}

fn deny(code: &str, reason: &str) -> CommandPolicyResult {
    CommandPolicyResult::Deny {
        reason: reason.to_string(),
        code: code.to_string(),
    }
}

/// 统计命令中 shell 调用的嵌套层数
fn count_shell_invocations(cmd_lower: &str) -> usize {
    let shells = ["cmd ", "cmd.exe", "powershell", "pwsh", "bash", "wsl "];
    let mut count = 0;
    for shell in &shells {
        count += cmd_lower.matches(shell).count();
    }
    count
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_safe_commands() {
        assert!(matches!(
            CommandPolicy::check("dir", &[]),
            CommandPolicyResult::Allow
        ));
        assert!(matches!(
            CommandPolicy::check("ipconfig", &[]),
            CommandPolicyResult::Allow
        ));
        assert!(matches!(
            CommandPolicy::check("echo hello", &[]),
            CommandPolicyResult::Allow
        ));
    }

    #[test]
    fn test_format_blocked() {
        let result = CommandPolicy::check("format C:", &[]);
        assert!(matches!(result, CommandPolicyResult::Deny { .. }));
    }

    #[test]
    fn test_registry_blocked() {
        assert!(matches!(
            CommandPolicy::check("reg delete HKLM\\Software", &[]),
            CommandPolicyResult::Deny { .. }
        ));
        assert!(matches!(
            CommandPolicy::check("reg add HKCU\\Test", &[]),
            CommandPolicyResult::Deny { .. }
        ));
    }

    #[test]
    fn test_shutdown_blocked() {
        assert!(matches!(
            CommandPolicy::check("shutdown /s /t 0", &[]),
            CommandPolicyResult::Deny { .. }
        ));
    }

    #[test]
    fn test_bulk_delete_blocked() {
        assert!(matches!(
            CommandPolicy::check("del /s C:\\temp\\*", &[]),
            CommandPolicyResult::Deny { .. }
        ));
        assert!(matches!(
            CommandPolicy::check("rmdir /s /q C:\\old", &[]),
            CommandPolicyResult::Deny { .. }
        ));
    }

    #[test]
    fn test_encoded_powershell_blocked() {
        assert!(matches!(
            CommandPolicy::check("powershell -EncodedCommand ABC123", &[]),
            CommandPolicyResult::Deny { .. }
        ));
    }

    #[test]
    fn test_nested_shell_blocked() {
        assert!(matches!(
            CommandPolicy::check("cmd /c powershell -Command echo", &[]),
            CommandPolicyResult::Deny { .. }
        ));
    }

    #[test]
    fn test_diskpart_blocked() {
        assert!(matches!(
            CommandPolicy::check("diskpart /s script.txt", &[]),
            CommandPolicyResult::Deny { .. }
        ));
    }
}
