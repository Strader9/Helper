//! 开机自启
//!
//! 通过 Windows 注册表 `HKCU\Software\Microsoft\Windows\CurrentVersion\Run`
//! 写入当前可执行文件路径实现。
//!
//! 为什么不用 `tauri-plugin-autostart`：该插件会额外引入一个依赖树，
//! 而这里只需要"写一个字符串、删一个字符串"两项操作。用 `reg.exe`
//! 直接操作，零新依赖，且行为对用户透明（可以自己在注册表编辑器里看到、
//! 也可以手动删掉）。
//!
//! 选择 HKCU 而非 HKLM：不需要管理员权限，且只对当前用户生效——
//! 这正是"我的电脑开机自动启动"应该有的语义。

use crate::error::{AppError, AppResult};
use std::process::Command;
use std::sync::Mutex;
use std::sync::OnceLock;

/// 注册表中的应用标识（Value 名）
const APP_VALUE_NAME: &str = "EAssisant";

/// 注册表路径
const RUN_KEY: &str = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run";

#[cfg(target_os = "windows")]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// 缓存当前可执行文件路径
///
/// `current_exe()` 每次调用都要访问文件系统，而这个路径在进程生命周期内
/// 不会变。缓存下来避免重复 IO，同时保证注册表里写的路径始终一致。
fn exe_path() -> AppResult<&'static str> {
    static CACHE: OnceLock<Option<String>> = OnceLock::new();
    let cached = CACHE.get_or_init(|| {
        std::env::current_exe()
            .ok()
            .map(|p| p.to_string_lossy().to_string())
    });
    cached
        .as_deref()
        .ok_or_else(|| AppError::Internal("无法获取当前程序路径，开机自启设置失败".to_string()))
}

/// 应用开机自启设置
///
/// `enabled = true` 写入注册表，`false` 删除注册项。
/// 重复调用是幂等的：写两次同一个值不会出错，删一个不存在的值也不算出错。
pub fn apply(enabled: bool) -> AppResult<()> {
    if enabled {
        enable()
    } else {
        disable()
    }
}

/// 查询自启状态，并把"查不到"与"确认未启用"区分开。
///
/// 返回 `None` 表示 `reg.exe` 不可用或执行失败——此时**不能**当作"未启用"
/// 上报给用户，否则界面会在环境受限时谎称自启已关闭。
///
/// 只暴露这一个查询入口（不额外提供返回 bool 的版本）：调用方必须显式处理
/// "未知"这一情况，避免有人图省事 `unwrap_or(false)` 把受限环境说成"已关闭"。
pub fn query_or_none() -> Option<bool> {
    #[cfg(target_os = "windows")]
    {
        let output = Command::new("reg")
            .args(["query", RUN_KEY, "/v", APP_VALUE_NAME])
            .creation_flags(CREATE_NO_WINDOW)
            .output()
            .ok()?; // reg 起不来 → None

        Some(output.status.success())
    }
    #[cfg(not(target_os = "windows"))]
    {
        None
    }
}

#[cfg(target_os = "windows")]
fn enable() -> AppResult<()> {
    let path = exe_path()?;
    // 路径用双引号包住：安装目录含空格时（如 Program Files）不加引号
    // 会被 Windows 拆成多个参数，导致启动失败。
    let quoted = format!("\"{}\"", path);

    let output = Command::new("reg")
        .args([
            "add",
            RUN_KEY,
            "/v",
            APP_VALUE_NAME,
            "/t",
            "REG_SZ",
            "/d",
            &quoted,
            "/f",
        ])
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .map_err(|e| AppError::Internal(format!("调用 reg add 失败: {}", e)))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(AppError::Internal(format!(
            "写入开机自启注册表失败: {}",
            stderr.trim()
        )));
    }
    Ok(())
}

#[cfg(target_os = "windows")]
fn disable() -> AppResult<()> {
    // 删除一个不存在的项会返回非零退出码。这不算错误——
    // 目标状态（不再自启）已经达成，所以这里刻意不看退出状态。
    let _ = Command::new("reg")
        .args(["delete", RUN_KEY, "/v", APP_VALUE_NAME, "/f"])
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .map_err(|e| AppError::Internal(format!("调用 reg delete 失败: {}", e)))?;
    Ok(())
}

#[cfg(not(target_os = "windows"))]
fn enable() -> AppResult<()> {
    Err(AppError::Internal("开机自启目前只支持 Windows".to_string()))
}

#[cfg(not(target_os = "windows"))]
fn disable() -> AppResult<()> {
    Ok(())
}

#[cfg(target_os = "windows")]
use std::os::windows::process::CommandExt;

/// 全局互斥：避免用户快速连点开关时两个 reg 进程交错执行
static APPLY_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

/// 带互斥的 apply（供命令层调用）
pub fn apply_serialized(enabled: bool) -> AppResult<()> {
    let lock = APPLY_LOCK.get_or_init(|| Mutex::new(()));
    let _guard = lock
        .lock()
        .map_err(|_| AppError::Internal("开机自启设置锁被污染".to_string()))?;
    apply(enabled)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exe_path_is_available_in_tests() {
        // 测试进程本身就是一个可执行文件，路径必然拿得到。
        // 这个断言保证 exe_path 不会因为 OnceLock 初始化失败而永远返回 Err。
        assert!(exe_path().is_ok());
    }

    #[test]
    fn exe_path_is_stable_across_calls() {
        // 缓存必须生效：两次调用返回同一个字符串切片地址。
        // 如果缓存失效，注册表里的路径可能在不同时刻写成不同的值。
        let a = exe_path().expect("首次取值应成功");
        let b = exe_path().expect("二次取值应成功");
        assert_eq!(a, b);
        assert_eq!(a.as_ptr(), b.as_ptr(), "两次调用应命中同一份缓存");
    }

    #[test]
    fn run_key_points_at_the_current_user_hive() {
        // HKCU 而非 HKLM：不需要管理员权限，且只影响当前用户。
        // 这是"我的电脑开机启动"应有的语义，写进断言防止被误改。
        assert!(RUN_KEY.starts_with("HKCU"));
        assert!(RUN_KEY.contains("CurrentVersion\\Run"));
    }

    // 注意：这里**故意不测试** is_enabled / enable / disable 的真实行为。
    // 它们会启动 `reg.exe` 去读写注册表——测试进程触碰注册表既有副作用
    // （在开发者机器上真的加了一条自启项），也可能被安全策略拦截。
    // 这类"必须真的动系统"的逻辑只在应用运行时执行，测试只覆盖纯计算部分。
}
