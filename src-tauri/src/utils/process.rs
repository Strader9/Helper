//! 进程执行工具模块
//!
//! 提供隐藏窗口的进程执行函数，避免 PowerShell/cmd 窗口闪烁。
//! Windows 平台使用 CREATE_NO_WINDOW 标志 + -WindowStyle Hidden 参数。

use std::process::{Command, Output};

/// Windows CREATE_NO_WINDOW 标志
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x08000000;

/// 执行 PowerShell 脚本（隐藏窗口）
///
/// 自动添加 -NoProfile -NonInteractive -WindowStyle Hidden 参数。
/// Windows 平台同时设置 CREATE_NO_WINDOW 标志。
pub fn run_powershell(script: &str) -> std::io::Result<Output> {
    let mut cmd = Command::new("powershell");
    cmd.args(&[
        "-NoProfile",
        "-NonInteractive",
        "-WindowStyle",
        "Hidden",
        "-Command",
        script,
    ]);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd.output()
}

/// 执行 PowerShell 脚本（带额外参数，隐藏窗口）
///
/// 用于需要传递额外参数的场景。extra_args 会插入到 -Command 之前。
pub fn run_powershell_with_args(script: &str, extra_args: &[&str]) -> std::io::Result<Output> {
    let mut cmd = Command::new("powershell");
    cmd.args(&["-NoProfile", "-NonInteractive", "-WindowStyle", "Hidden"]);
    cmd.args(extra_args);
    cmd.args(&["-Command", script]);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd.output()
}

/// 执行 cmd 命令（隐藏窗口）
///
/// 等价于 cmd /c command，但隐藏窗口。
pub fn run_cmd(command: &str) -> std::io::Result<Output> {
    let mut cmd = Command::new("cmd");
    cmd.args(&["/c", command]);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd.output()
}

/// 执行任意控制台程序（隐藏窗口）
///
/// 用于 taskkill、tasklist、nvidia-smi、where 等控制台工具。
/// 注意：不要用于启动 GUI 程序（如浏览器、记事本），这些程序需要显示窗口。
pub fn run_console(program: &str, args: &[&str]) -> std::io::Result<Output> {
    let mut cmd = Command::new(program);
    cmd.args(args);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd.output()
}

/// 为已有 Command 应用隐藏窗口设置（Windows）
///
/// 用于需要自定义 Command 参数的场景。
/// 用法：
/// ```ignore
/// let mut cmd = Command::new("powershell");
/// cmd.args(...);
/// hide_command_window(&mut cmd);
/// let output = cmd.output()?;
/// ```
pub fn hide_command_window(cmd: &mut Command) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
}
