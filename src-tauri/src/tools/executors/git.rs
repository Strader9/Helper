//! Git 仓库操作
//!
//! 设计取舍：只暴露**固定动作**，不提供 `git(args...)` 这种通用入口。
//! 通用入口等于把任意命令执行权交给模型——`git config alias.x '!rm -rf ~'`
//! 之后一条 `git x` 就绕过了全部闸门。这里的每个工具都只拼一组
//! 写死的参数顺序，用户提供的只有仓库路径、文件路径、提交信息这类
//! **数据**，进不了参数位。
//!
//! 另一条纪律：不引 git2/libgit2。绑定库会增加 3MB 体积和一堆
//! OpenSSL/SSH 编译选项，而用户机器上本来就装着 git（命令行为准，
//! 配置、hook、credential helper 全都按用户自己的来）。

use std::path::{Path, PathBuf};
use std::process::Command;

use async_trait::async_trait;
use serde_json::{json, Value};

use crate::error::{AppError, AppResult};
use crate::tools::registry::{AgentTool, RiskLevel, ToolResult};

#[cfg(windows)]
use std::os::windows::process::CommandExt;
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// 单次输出上限。diff 在大型仓库里能到几十 MB，全塞进上下文只会把
/// 后面的对话挤掉，还不如给个截断标记让模型自己决定要不要分段看。
const MAX_OUTPUT_CHARS: usize = 60_000;

fn require_str<'a>(args: &'a Value, key: &str) -> AppResult<&'a str> {
    args[key]
        .as_str()
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| AppError::InvalidArgument(format!("{} is required", key)))
}

/// 解析仓库路径并确认它真的是一个 Git 仓库。
///
/// 提前判定而不是让 git 自己报错：`git -C /some/dir status` 在非仓库目录下
/// 会一路往上找到父目录的仓库，于是"在 A 目录查状态"可能悄悄返回 B 仓库的
/// 结果——用户看到的东西和以为的地方不是同一个。
fn resolve_repo(args: &Value) -> AppResult<PathBuf> {
    let raw = args["repo"].as_str().filter(|s| !s.trim().is_empty());
    let path = match raw {
        Some(p) => PathBuf::from(p),
        None => std::env::current_dir()
            .map_err(|e| AppError::InvalidArgument(format!("无法确定当前目录: {}", e)))?,
    };
    if !path.exists() {
        return Err(AppError::InvalidArgument(format!(
            "仓库路径不存在: {}",
            path.display()
        )));
    }
    if !path.is_dir() {
        return Err(AppError::InvalidArgument(format!(
            "仓库路径必须是目录: {}",
            path.display()
        )));
    }
    // 只认本层的 .git，不往上找：避免误伤父仓库。
    if !path.join(".git").exists() {
        return Err(AppError::InvalidArgument(format!(
            "这不是 Git 仓库根目录（找不到 .git）: {}",
            path.display()
        )));
    }
    Ok(path)
}

/// 执行 git，返回 (成功, stdout, stderr)。
///
/// 统一带上 `-c core.quotepath=false`：默认情况下中文路径会被转义成
/// `"\344\270\255"` 这种八进制串，模型看不懂，用户更看不懂。
/// `GIT_TERMINAL_PROMPT=0` 防止需要凭据时进程挂在后台等输入。
fn run_git(repo: &Path, args: &[&str]) -> AppResult<(bool, String, String)> {
    let mut cmd = Command::new("git");
    cmd.arg("-C")
        .arg(repo)
        .arg("-c")
        .arg("core.quotepath=false")
        .arg("-c")
        .arg("color.ui=false")
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_PAGER", "cat");
    #[cfg(windows)]
    cmd.creation_flags(CREATE_NO_WINDOW);

    let output = cmd.output().map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            AppError::ToolExecution("未找到 git 命令，请先安装 Git for Windows".to_string())
        } else {
            AppError::ToolExecution(format!("无法执行 git: {}", e))
        }
    })?;
    Ok((
        output.status.success(),
        String::from_utf8_lossy(&output.stdout).to_string(),
        String::from_utf8_lossy(&output.stderr).to_string(),
    ))
}

fn truncate(text: &str, max: usize) -> (String, bool) {
    if text.chars().count() <= max {
        return (text.to_string(), false);
    }
    let cut: String = text.chars().take(max).collect();
    (cut, true)
}

fn git_missing() -> ToolResult {
    ToolResult::err("GIT_NOT_FOUND", "未找到 git 命令，请先安装 Git for Windows")
}

// ============================================================
// git_status
// ============================================================

/// 查看工作区状态
pub struct GitStatusTool;

#[async_trait]
impl AgentTool for GitStatusTool {
    fn name(&self) -> &'static str {
        "git_status"
    }

    fn description(&self) -> &'static str {
        "查看 Git 仓库当前状态：当前分支、领先/落后远端多少提交、已暂存/未暂存/未跟踪的文件清单。只读，不修改任何东西。"
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "repo": { "type": "string", "description": "仓库根目录（含 .git 的目录）。省略时用进程当前目录" },
                "porcelain": { "type": "boolean", "description": "用机器可读的简短格式（XY 状态码 + 路径），默认 false" }
            }
        })
    }

    fn risk_level(&self) -> RiskLevel {
        RiskLevel::Safe
    }

    async fn execute(&self, args: Value) -> AppResult<ToolResult> {
        let repo = resolve_repo(&args)?;
        let porcelain = args["porcelain"].as_bool().unwrap_or(false);

        let mut argv: Vec<&str> = vec!["status"];
        if porcelain {
            argv.extend(["--porcelain=v1", "-z"]);
        } else {
            argv.extend(["--branch", "--short", "--untracked-files=normal"]);
        }
        let (ok, stdout, stderr) = run_git(&repo, &argv)?;
        if !ok {
            if stderr.contains("not a git repository") {
                return Ok(ToolResult::err("NOT_A_GIT_REPO", stderr.trim()));
            }
            if stderr.contains("git: command not found") || stderr.is_empty() {
                // stderr 空且失败：多半是 git 本身拉不起来
                return Ok(git_missing());
            }
            return Ok(ToolResult::err("GIT_STATUS_FAILED", stderr.trim()));
        }

        // -z 用 NUL 分隔，转成换行便于阅读
        let body = if porcelain {
            stdout.replace('\0', "\n")
        } else {
            stdout.clone()
        };
        let (text, truncated) = truncate(body.trim_end(), MAX_OUTPUT_CHARS);

        // 从 --branch 输出里抠分支名；porcelain 模式下第一行是 ## branch...
        let mut branch = String::new();
        let mut ahead = 0u32;
        let mut behind = 0u32;
        for line in text.lines().take(1) {
            let trimmed = line.trim_start_matches('#').trim();
            if let Some(rest) = trimmed.strip_prefix("## ") {
                let name = rest.split("...").next().unwrap_or(rest).trim();
                branch = name.split_whitespace().next().unwrap_or("").to_string();
                for token in rest.split_whitespace() {
                    if let Some(n) = token.strip_prefix("ahead ") {
                        ahead = n.trim_end_matches(']').parse().unwrap_or(0);
                    }
                    if let Some(n) = token.strip_prefix("behind ") {
                        behind = n.trim_end_matches(']').parse().unwrap_or(0);
                    }
                }
            }
        }

        let changed = text
            .lines()
            .filter(|l| !l.trim().is_empty() && !l.trim_start().starts_with("##"))
            .count();

        Ok(ToolResult::ok(json!({
            "repo": repo.display().to_string(),
            "branch": branch,
            "ahead": ahead,
            "behind": behind,
            "changed_entries": changed,
            "clean": changed == 0,
            "truncated": truncated,
            "status": text,
        })))
    }
}

// ============================================================
// git_diff
// ============================================================

/// 查看改动内容
pub struct GitDiffTool;

#[async_trait]
impl AgentTool for GitDiffTool {
    fn name(&self) -> &'static str {
        "git_diff"
    }

    fn description(&self) -> &'static str {
        "查看 Git 改动差异（unified diff）。默认看工作区未暂存的改动；staged=true 看已暂存待提交的内容；commit 指定某个历史提交则看该提交本身。只读。"
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "repo": { "type": "string", "description": "仓库根目录，省略时用当前目录" },
                "path": { "type": "string", "description": "只查看某个文件或子目录的改动（相对仓库根）" },
                "staged": { "type": "boolean", "description": "查看已暂存的改动（相当于 git diff --staged），默认 false" },
                "commit": { "type": "string", "description": "查看某个提交引入的改动（如 HEAD、HEAD~1、提交哈希）" },
                "max_chars": { "type": "integer", "description": "最多返回多少字符，默认 20000，上限 60000" }
            }
        })
    }

    fn risk_level(&self) -> RiskLevel {
        RiskLevel::Safe
    }

    async fn execute(&self, args: Value) -> AppResult<ToolResult> {
        let repo = resolve_repo(&args)?;
        let max = args["max_chars"]
            .as_u64()
            .unwrap_or(20_000)
            .min(MAX_OUTPUT_CHARS as u64) as usize;

        let mut argv: Vec<String> = vec!["diff".into(), "--no-color".into()];
        if args["staged"].as_bool().unwrap_or(false) {
            argv.push("--staged".into());
        }
        if let Some(commit) = args["commit"].as_str().filter(|s| !s.is_empty()) {
            // 只允许引用与哈希：形如 HEAD~3 / abc1234 / main。
            // 带空格或前导 - 的字符串会被 git 当成选项解析（例如 --output=...），
            // 那就不再是"看差异"了。
            let safe = commit
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || "~^/._-".contains(c));
            if !safe || commit.starts_with('-') {
                return Err(AppError::InvalidArgument(format!(
                    "commit 参数不合法（只接受分支名/标签/哈希/HEAD~n）: {}",
                    commit
                )));
            }
            argv.push(format!("{}^!", commit));
        }
        if let Some(path) = args["path"].as_str().filter(|s| !s.is_empty()) {
            argv.push("--".into());
            argv.push(path.to_string());
        }

        let refs: Vec<&str> = argv.iter().map(|s| s.as_str()).collect();
        let (ok, stdout, stderr) = run_git(&repo, &refs)?;
        if !ok {
            return Ok(ToolResult::err("GIT_DIFF_FAILED", stderr.trim()));
        }

        let (text, truncated) = truncate(&stdout, max);
        let added = text
            .lines()
            .filter(|l| l.starts_with('+') && !l.starts_with("+++"))
            .count();
        let removed = text
            .lines()
            .filter(|l| l.starts_with('-') && !l.starts_with("---"))
            .count();
        let files: Vec<String> = text
            .lines()
            .filter_map(|l| l.strip_prefix("+++ b/").map(|s| s.to_string()))
            .collect();

        Ok(ToolResult::ok(json!({
            "repo": repo.display().to_string(),
            "empty": stdout.trim().is_empty(),
            "files_changed": files.len(),
            "files": files,
            "added_lines": added,
            "removed_lines": removed,
            "truncated": truncated,
            "diff": text.trim_end(),
        })))
    }
}

// ============================================================
// git_log
// ============================================================

/// 查看提交历史
pub struct GitLogTool;

#[async_trait]
impl AgentTool for GitLogTool {
    fn name(&self) -> &'static str {
        "git_log"
    }

    fn description(&self) -> &'static str {
        "查看 Git 提交历史（哈希、作者、时间、提交信息）。可按文件路径过滤。只读。"
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "repo": { "type": "string", "description": "仓库根目录，省略时用当前目录" },
                "limit": { "type": "integer", "description": "返回多少条，默认 20，上限 200" },
                "path": { "type": "string", "description": "只看影响该文件的提交" },
                "author": { "type": "string", "description": "只看某个作者的提交" }
            }
        })
    }

    fn risk_level(&self) -> RiskLevel {
        RiskLevel::Safe
    }

    async fn execute(&self, args: Value) -> AppResult<ToolResult> {
        let repo = resolve_repo(&args)?;
        let limit = args["limit"].as_u64().unwrap_or(20).min(200).to_string();

        // %x1f 是字段分隔符，%x1e 是记录分隔符：提交信息里可能含换行，
        // 用换行切分会把一条提交读成多条。
        let mut argv: Vec<String> = vec![
            "log".into(),
            format!("-{}", limit),
            "--no-color".into(),
            "--date=iso".into(),
            "--format=%H%x1f%an%x1f%ad%x1f%s%x1e".into(),
        ];
        if let Some(author) = args["author"].as_str().filter(|s| !s.is_empty()) {
            argv.push(format!("--author={}", author));
        }
        if let Some(path) = args["path"].as_str().filter(|s| !s.is_empty()) {
            argv.push("--".into());
            argv.push(path.to_string());
        }

        let refs: Vec<&str> = argv.iter().map(|s| s.as_str()).collect();
        let (ok, stdout, stderr) = run_git(&repo, &refs)?;
        if !ok {
            // 空仓库（还没有任何提交）时 git log 会失败，这不是错误。
            if stderr.contains("does not have any commits") || stderr.contains("unknown revision") {
                return Ok(ToolResult::ok(json!({
                    "repo": repo.display().to_string(),
                    "count": 0,
                    "commits": [],
                })));
            }
            return Ok(ToolResult::err("GIT_LOG_FAILED", stderr.trim()));
        }

        let commits: Vec<Value> = stdout
            .split('\u{1e}')
            .filter(|rec| !rec.trim().is_empty())
            .filter_map(|rec| {
                let mut parts = rec.trim_matches('\n').split('\u{1f}');
                let hash = parts.next()?.to_string();
                let author = parts.next().unwrap_or("").to_string();
                let date = parts.next().unwrap_or("").to_string();
                let subject = parts.next().unwrap_or("").to_string();
                Some(json!({
                    "hash": hash,
                    "short_hash": hash.chars().take(7).collect::<String>(),
                    "author": author,
                    "date": date,
                    "subject": subject,
                }))
            })
            .collect();

        Ok(ToolResult::ok(json!({
            "repo": repo.display().to_string(),
            "count": commits.len(),
            "commits": commits,
        })))
    }
}

// ============================================================
// git_stage
// ============================================================

/// 暂存文件
pub struct GitStageTool;

#[async_trait]
impl AgentTool for GitStageTool {
    fn name(&self) -> &'static str {
        "git_stage"
    }

    fn description(&self) -> &'static str {
        "把改动加入暂存区（git add）。必须明确给出 paths 列表；只有在同时传 all=true 时才会暂存全部改动——不提供「不加参数就暂存一切」的默认行为。"
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "repo": { "type": "string", "description": "仓库根目录，省略时用当前目录" },
                "paths": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "要暂存的文件/目录（相对仓库根）。推荐逐个列出"
                },
                "all": { "type": "boolean", "description": "暂存所有改动（含删除）。默认 false，必须显式打开" }
            }
        })
    }

    fn risk_level(&self) -> RiskLevel {
        RiskLevel::Low
    }

    async fn execute(&self, args: Value) -> AppResult<ToolResult> {
        let repo = resolve_repo(&args)?;
        let all = args["all"].as_bool().unwrap_or(false);
        let paths: Vec<String> = args["paths"]
            .as_array()
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str())
                    .filter(|s| !s.trim().is_empty())
                    .map(|s| s.to_string())
                    .collect()
            })
            .unwrap_or_default();

        if !all && paths.is_empty() {
            return Err(AppError::InvalidArgument(
                "paths 为空：请明确列出要暂存的文件；确实要暂存全部改动时传 all=true".to_string(),
            ));
        }
        // 路径不得以 - 开头，否则会被当成选项（如 --force）
        if paths.iter().any(|p| p.starts_with('-')) {
            return Err(AppError::InvalidArgument("paths 不能以 - 开头".to_string()));
        }

        let argv: Vec<String> = if all {
            vec!["add".into(), "-A".into()]
        } else {
            let mut v = vec!["add".into(), "--".into()];
            v.extend(paths.iter().cloned());
            v
        };
        let refs: Vec<&str> = argv.iter().map(|s| s.as_str()).collect();
        let (ok, _stdout, stderr) = run_git(&repo, &refs)?;
        if !ok {
            return Ok(ToolResult::err("GIT_STAGE_FAILED", stderr.trim()));
        }

        // 暂存后再查一次：git add 成功不代表暂存区真有东西
        // （比如 pathspec 匹配不到任何文件时 git 会警告但退出码是 1）。
        let (ok2, staged, _) = run_git(&repo, &["diff", "--cached", "--name-only"])?;
        let staged_files: Vec<String> = if ok2 {
            staged
                .lines()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect()
        } else {
            Vec::new()
        };

        Ok(ToolResult::ok(json!({
            "repo": repo.display().to_string(),
            "mode": if all { "all" } else { "paths" },
            "requested_paths": paths,
            "staged_count": staged_files.len(),
            "staged_files": staged_files,
        })))
    }
}

// ============================================================
// git_commit
// ============================================================

/// 提交
pub struct GitCommitTool;

#[async_trait]
impl AgentTool for GitCommitTool {
    fn name(&self) -> &'static str {
        "git_commit"
    }

    fn description(&self) -> &'static str {
        "把已暂存的改动提交为一个新提交。若同时给了 paths，会先把这些路径加入暂存再提交。消息为空会被拒绝；不执行 push，也不修改任何远端。"
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "repo": { "type": "string", "description": "仓库根目录，省略时用当前目录" },
                "message": { "type": "string", "description": "提交信息（必填）" },
                "paths": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "可选：提交前先暂存这些路径"
                },
                "allow_empty": { "type": "boolean", "description": "暂存区为空时是否仍创建空提交，默认 false" }
            },
            "required": ["message"]
        })
    }

    fn risk_level(&self) -> RiskLevel {
        RiskLevel::Medium
    }

    async fn execute(&self, args: Value) -> AppResult<ToolResult> {
        let repo = resolve_repo(&args)?;
        let message = require_str(&args, "message")?.trim().to_string();
        if message.is_empty() {
            return Err(AppError::InvalidArgument("提交信息不能为空".to_string()));
        }
        if message.chars().count() > 5000 {
            return Err(AppError::InvalidArgument(
                "提交信息过长（上限 5000 字符）".to_string(),
            ));
        }

        if let Some(arr) = args["paths"].as_array() {
            let paths: Vec<String> = arr
                .iter()
                .filter_map(|v| v.as_str())
                .filter(|s| !s.trim().is_empty())
                .map(|s| s.to_string())
                .collect();
            if paths.iter().any(|p| p.starts_with('-')) {
                return Err(AppError::InvalidArgument("paths 不能以 - 开头".to_string()));
            }
            if !paths.is_empty() {
                let mut argv: Vec<String> = vec!["add".into(), "--".into()];
                argv.extend(paths);
                let refs: Vec<&str> = argv.iter().map(|s| s.as_str()).collect();
                let (ok, _out, err) = run_git(&repo, &refs)?;
                if !ok {
                    return Ok(ToolResult::err("GIT_STAGE_FAILED", err.trim()));
                }
            }
        }

        // 先看暂存区是否为空。直接 commit 也能报错，但错误信息是
        // "nothing to commit"，看不出到底有没有暂存到东西。
        let (ok, staged, _) = run_git(&repo, &["diff", "--cached", "--name-only"])?;
        let staged_count = if ok {
            staged.lines().filter(|l| !l.trim().is_empty()).count()
        } else {
            0
        };
        let allow_empty = args["allow_empty"].as_bool().unwrap_or(false);
        if staged_count == 0 && !allow_empty {
            return Ok(ToolResult::err(
                "NOTHING_STAGED",
                "暂存区为空，没有可提交的内容。请先用 git_stage 暂存，或传 allow_empty=true 创建空提交。",
            ));
        }

        // 提交信息走 -F - 从 stdin 传入：`-m` 遇到含换行/引号的消息时，
        // 经命令行传递在 Windows 上会被 cmd 的引号规则改写。
        let mut cmd = Command::new("git");
        cmd.arg("-C")
            .arg(&repo)
            .args(["-c", "core.quotepath=false"])
            .args(["commit", "-F", "-"])
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("GIT_PAGER", "cat")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        if allow_empty {
            cmd.arg("--allow-empty");
        }
        #[cfg(windows)]
        cmd.creation_flags(CREATE_NO_WINDOW);

        let mut child = cmd
            .spawn()
            .map_err(|e| AppError::ToolExecution(format!("无法执行 git commit: {}", e)))?;
        {
            use std::io::Write;
            if let Some(mut stdin) = child.stdin.take() {
                stdin
                    .write_all(message.as_bytes())
                    .map_err(|e| AppError::ToolExecution(format!("写入提交信息失败: {}", e)))?;
            }
        }
        let output = child
            .wait_with_output()
            .map_err(|e| AppError::ToolExecution(format!("git commit 执行失败: {}", e)))?;
        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        if !output.status.success() {
            return Ok(ToolResult::err("GIT_COMMIT_FAILED", stderr.trim()));
        }

        // 取新提交的哈希：这是"真的提交成功了"的唯一硬证据，
        // 光看 stdout 里那句 "[main abc1234] msg" 得去正则解析，不可靠。
        let (_, head, _) = run_git(&repo, &["rev-parse", "--short", "HEAD"])?;
        let hash = head.trim().to_string();

        Ok(ToolResult::ok(json!({
            "repo": repo.display().to_string(),
            "commit": hash,
            "message": message,
            "staged_files_before_commit": staged_count,
            "output": stdout.trim(),
        })))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_keeps_short_text_intact() {
        let (text, cut) = truncate("abc", 10);
        assert_eq!(text, "abc");
        assert!(!cut);
    }

    #[test]
    fn truncate_counts_chars_not_bytes() {
        // 中文一字 3 字节：按字节截会把字切坏
        let (text, cut) = truncate("中文中文", 2);
        assert_eq!(text, "中文");
        assert!(cut);
    }

    #[test]
    fn resolve_repo_rejects_missing_path() {
        let args = json!({ "repo": "Z:/definitely/not/here" });
        assert!(resolve_repo(&args).is_err());
    }

    #[test]
    fn run_git_reports_missing_repo_cleanly() {
        let args = json!({ "repo": "Z:/definitely/not/here" });
        let err = resolve_repo(&args).unwrap_err().to_string();
        assert!(err.contains("不存在"), "错误信息应说清路径不存在: {}", err);
    }
}
