//! 文件管理扩容工具
//!
//! 与 `file_ops.rs` 的分工：那边是"读、写、列、开"，这边负责搬迁、检索、
//! 属性与建目录。
//!
//! 两条贯穿全文件的规则：
//! 1. **同名不覆盖**。搬迁类工具遇到已存在的落点一律拒绝，除非调用方显式
//!    授权覆盖——覆盖是不可撤销的，而"复制/移动"的语义里没人授权覆盖。
//! 2. **删除先落回收站**。默认走系统回收站，只有显式 `permanent=true`
//!    才真删。工具不该比用户更果断。

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::SystemTime;

use async_trait::async_trait;
use sha2::{Digest, Sha256};

#[cfg(windows)]
use std::os::windows::process::CommandExt;

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x08000000;

use crate::error::{AppError, AppResult};
use crate::tools::{AgentTool, RiskLevel, ToolResult};

/// 遍历检索时跳过的目录名。
///
/// 这几个目录的共同点是"体量大且不承载用户内容"，扫进去只会拖慢
/// 检索，还会把依赖包里的字符串当成用户文件命中。
const SKIP_DIRS: &[&str] = &[
    "node_modules",
    ".git",
    "target",
    "dist",
    "build",
    ".next",
    "__pycache__",
    ".venv",
    "venv",
    "AppData",
    "$RECYCLE.BIN",
    "System Volume Information",
];

/// 文本检索默认认的扩展名。
const DEFAULT_TEXT_EXT: &[&str] = &[
    "txt", "md", "markdown", "json", "jsonl", "yaml", "yml", "toml", "ini", "csv", "tsv", "log",
    "rs", "ts", "tsx", "js", "jsx", "py", "go", "java", "c", "h", "cpp", "hpp", "cs", "sh", "ps1",
    "html", "htm", "css", "scss", "xml", "sql",
];

fn require_str<'a>(args: &'a serde_json::Value, key: &str) -> AppResult<&'a str> {
    args[key]
        .as_str()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| AppError::InvalidArgument(format!("{} is required", key)))
}

fn format_time(time: SystemTime) -> String {
    let dt: chrono::DateTime<chrono::Local> = time.into();
    dt.format("%Y-%m-%d %H:%M:%S").to_string()
}

/// 目录递归复制。`fs::copy` 只管单文件，目录必须自己走一遍。
fn copy_tree(from: &Path, to: &Path) -> std::io::Result<u64> {
    let mut bytes = 0u64;
    if from.is_dir() {
        fs::create_dir_all(to)?;
        for entry in fs::read_dir(from)? {
            let entry = entry?;
            let child_to = to.join(entry.file_name());
            bytes += copy_tree(&entry.path(), &child_to)?;
        }
    } else {
        if let Some(parent) = to.parent() {
            fs::create_dir_all(parent)?;
        }
        bytes += fs::copy(from, to)?;
    }
    Ok(bytes)
}

/// 递归统计目录体积。复制后核对字节数要用它。
fn tree_bytes(path: &Path) -> u64 {
    if path.is_file() {
        return fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    }
    let mut total = 0u64;
    if let Ok(entries) = fs::read_dir(path) {
        for entry in entries.flatten() {
            total += tree_bytes(&entry.path());
        }
    }
    total
}

fn sha256_of(path: &Path, limit: u64) -> Option<String> {
    let meta = fs::metadata(path).ok()?;
    if meta.len() > limit {
        return None;
    }
    let data = fs::read(path).ok()?;
    let mut hasher = Sha256::new();
    hasher.update(&data);
    Some(format!("{:x}", hasher.finalize()))
}

/// 通配符匹配（`*` 任意长度、`?` 单字符）。
///
/// 不引 glob 依赖：这里只需要匹配文件名，几十行足够，
/// 而多一个 crate 就多一份审计面。
fn glob_match(pattern: &str, name: &str) -> bool {
    let p: Vec<char> = pattern.to_lowercase().chars().collect();
    let n: Vec<char> = name.to_lowercase().chars().collect();
    glob_rec(&p, &n)
}

fn glob_rec(p: &[char], n: &[char]) -> bool {
    if p.is_empty() {
        return n.is_empty();
    }
    match p[0] {
        '*' => {
            let mut rest = &p[1..];
            while !rest.is_empty() && rest[0] == '*' {
                rest = &rest[1..];
            }
            if rest.is_empty() {
                return true;
            }
            (0..=n.len()).any(|i| glob_rec(rest, &n[i..]))
        }
        '?' => !n.is_empty() && glob_rec(&p[1..], &n[1..]),
        c => !n.is_empty() && n[0] == c && glob_rec(&p[1..], &n[1..]),
    }
}

/// Windows 回收站删除。走 shell 的 VisualBasic 文件 API，
/// 与资源管理器右键删除是同一套语义。
fn recycle(path: &Path) -> AppResult<()> {
    #[cfg(windows)]
    {
        let escaped = path.to_string_lossy().replace('\'', "''");
        let method = if path.is_dir() {
            "DeleteDirectory"
        } else {
            "DeleteFile"
        };
        // 目录要先确认存在，否则 VisualBasic 抛的是不可读的 COM 错误
        let script = format!(
            "Add-Type -AssemblyName Microsoft.VisualBasic\n\
             [Microsoft.VisualBasic.FileIO.FileSystem]::{}( '{}', 'OnlyErrorDialogs', 'SendToRecycleBin' )",
            method, escaped
        );
        let output = Command::new("powershell")
            .args([
                "-NoProfile",
                "-NonInteractive",
                "-WindowStyle",
                "Hidden",
                "-ExecutionPolicy",
                "Bypass",
                "-Command",
                &script,
            ])
            .creation_flags(CREATE_NO_WINDOW)
            .output()
            .map_err(|e| AppError::ToolExecution(format!("无法调用回收站: {}", e)))?;
        if output.status.success() {
            Ok(())
        } else {
            Err(AppError::ToolExecution(format!(
                "移入回收站失败: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            )))
        }
    }

    #[cfg(not(windows))]
    {
        let _ = path;
        Err(AppError::ToolExecution(
            "当前平台没有回收站实现".to_string(),
        ))
    }
}

// ============================================================
// 复制
// ============================================================

/// 复制文件或整个目录
pub struct CopyPathTool;

#[async_trait]
impl AgentTool for CopyPathTool {
    fn name(&self) -> &'static str {
        "copy_path"
    }

    fn description(&self) -> &'static str {
        "复制文件或整个目录到目标位置。目标已存在时默认拒绝，需要覆盖必须显式传 overwrite=true。返回复制的字节数。"
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "source": { "type": "string", "description": "源文件或目录路径" },
                "destination": { "type": "string", "description": "目标路径（含文件名）" },
                "overwrite": { "type": "boolean", "description": "目标已存在时是否覆盖，默认 false" }
            },
            "required": ["source", "destination"]
        })
    }

    fn risk_level(&self) -> RiskLevel {
        RiskLevel::Medium
    }

    async fn execute(&self, args: serde_json::Value) -> AppResult<ToolResult> {
        let source = require_str(&args, "source")?;
        let destination = require_str(&args, "destination")?;
        let overwrite = args["overwrite"].as_bool().unwrap_or(false);

        let src = Path::new(source);
        let dst = Path::new(destination);
        if !src.exists() {
            return Ok(ToolResult::err(
                "SOURCE_NOT_FOUND",
                &format!("源路径不存在: {}", source),
            ));
        }
        if dst.exists() && !overwrite {
            return Ok(ToolResult::err(
                "DESTINATION_EXISTS",
                &format!("目标已存在，未覆盖: {}", destination),
            ));
        }
        // 目录复制到自身内部会导致无限递归
        if src.is_dir() {
            if let (Ok(s), Ok(d)) = (src.canonicalize(), dst.canonicalize()) {
                if d.starts_with(&s) {
                    return Ok(ToolResult::err(
                        "INVALID_DESTINATION",
                        "不能把目录复制到它自己内部",
                    ));
                }
            } else if dst.starts_with(src) {
                return Ok(ToolResult::err(
                    "INVALID_DESTINATION",
                    "不能把目录复制到它自己内部",
                ));
            }
        }

        let kind = if src.is_dir() { "dir" } else { "file" };
        match copy_tree(src, dst) {
            Ok(bytes) => Ok(ToolResult::ok(serde_json::json!({
                "source": source,
                "destination": destination,
                "kind": kind,
                "bytes": bytes,
                "verified_bytes": tree_bytes(dst),
            }))),
            Err(e) => Ok(ToolResult::err("COPY_FAILED", &format!("复制失败: {}", e))),
        }
    }
}

// ============================================================
// 移动 / 重命名
// ============================================================

/// 移动或重命名文件、目录
pub struct MovePathTool;

#[async_trait]
impl AgentTool for MovePathTool {
    fn name(&self) -> &'static str {
        "move_path"
    }

    fn description(&self) -> &'static str {
        "移动或重命名文件/目录。目标已存在时默认拒绝；跨磁盘移动会自动退化为复制后删除源。"
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "source": { "type": "string", "description": "源路径" },
                "destination": { "type": "string", "description": "目标路径" },
                "overwrite": { "type": "boolean", "description": "目标已存在时是否覆盖，默认 false" }
            },
            "required": ["source", "destination"]
        })
    }

    fn risk_level(&self) -> RiskLevel {
        RiskLevel::Medium
    }

    async fn execute(&self, args: serde_json::Value) -> AppResult<ToolResult> {
        let source = require_str(&args, "source")?;
        let destination = require_str(&args, "destination")?;
        let overwrite = args["overwrite"].as_bool().unwrap_or(false);

        let src = Path::new(source);
        let dst = Path::new(destination);
        if !src.exists() {
            return Ok(ToolResult::err(
                "SOURCE_NOT_FOUND",
                &format!("源路径不存在: {}", source),
            ));
        }
        if dst.exists() {
            if !overwrite {
                return Ok(ToolResult::err(
                    "DESTINATION_EXISTS",
                    &format!("目标已存在，未移动: {}", destination),
                ));
            }
            // 覆盖目录是数据丢失，不做
            if dst.is_dir() {
                return Ok(ToolResult::err(
                    "DESTINATION_EXISTS",
                    "目标是已存在的目录，拒绝覆盖",
                ));
            }
            let _ = fs::remove_file(dst);
        }

        // rename 在同一卷上是原子操作，跨卷才需要复制
        match fs::rename(src, dst) {
            Ok(()) => Ok(ToolResult::ok(serde_json::json!({
                "source": source,
                "destination": destination,
                "mode": "rename",
                "source_gone": !src.exists(),
            }))),
            Err(_) => {
                let bytes = copy_tree(src, dst)
                    .map_err(|e| AppError::ToolExecution(format!("跨卷移动失败: {}", e)))?;
                if src.is_dir() {
                    fs::remove_dir_all(src)?;
                } else {
                    fs::remove_file(src)?;
                }
                Ok(ToolResult::ok(serde_json::json!({
                    "source": source,
                    "destination": destination,
                    "mode": "copy_then_delete",
                    "bytes": bytes,
                    "source_gone": !src.exists(),
                })))
            }
        }
    }
}

// ============================================================
// 删除（默认进回收站）
// ============================================================

/// 删除文件或目录
pub struct DeletePathTool;

#[async_trait]
impl AgentTool for DeletePathTool {
    fn name(&self) -> &'static str {
        "delete_path"
    }

    fn description(&self) -> &'static str {
        "删除文件或目录。默认移入系统回收站，可恢复；只有显式传 permanent=true 才永久删除。"
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "要删除的路径" },
                "permanent": { "type": "boolean", "description": "true 表示永久删除，不走回收站，默认 false" }
            },
            "required": ["path"]
        })
    }

    fn risk_level(&self) -> RiskLevel {
        RiskLevel::High
    }

    async fn execute(&self, args: serde_json::Value) -> AppResult<ToolResult> {
        let path = require_str(&args, "path")?;
        let permanent = args["permanent"].as_bool().unwrap_or(false);

        let target = Path::new(path);
        if !target.exists() {
            return Ok(ToolResult::err(
                "PATH_NOT_FOUND",
                &format!("路径不存在: {}", path),
            ));
        }
        // 根目录永远不动：一次手滑就是整盘
        if target.parent().is_none() {
            return Ok(ToolResult::err("REFUSED_ROOT", "拒绝删除磁盘根目录"));
        }

        let kind = if target.is_dir() { "dir" } else { "file" };
        let existed = true;

        let outcome = if permanent {
            if target.is_dir() {
                fs::remove_dir_all(target)
            } else {
                fs::remove_file(target)
            }
            .map_err(|e| AppError::ToolExecution(format!("永久删除失败: {}", e)))
        } else {
            recycle(target)
        };

        match outcome {
            Ok(()) => Ok(ToolResult::ok(serde_json::json!({
                "path": path,
                "kind": kind,
                "permanent": permanent,
                "existed": existed,
                "exists_after": target.exists(),
                "recoverable": !permanent,
            }))),
            Err(e) => Ok(ToolResult::err("DELETE_FAILED", &e.to_string())),
        }
    }
}

// ============================================================
// 文件属性
// ============================================================

/// 读取文件/目录属性
pub struct GetFileInfoTool;

#[async_trait]
impl AgentTool for GetFileInfoTool {
    fn name(&self) -> &'static str {
        "get_file_info"
    }

    fn description(&self) -> &'static str {
        "读取文件或目录的属性：大小、时间、只读标记，文件还可带 SHA-256 摘要。用于确认文件是否真的被写入了新内容。"
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "文件或目录路径" },
                "hash": { "type": "boolean", "description": "是否计算 SHA-256，默认 true（仅文件且不超过 64MB）" }
            },
            "required": ["path"]
        })
    }

    fn risk_level(&self) -> RiskLevel {
        RiskLevel::Safe
    }

    async fn execute(&self, args: serde_json::Value) -> AppResult<ToolResult> {
        let path = require_str(&args, "path")?;
        let want_hash = args["hash"].as_bool().unwrap_or(true);

        let target = Path::new(path);
        let meta = match fs::metadata(target) {
            Ok(m) => m,
            Err(_) => {
                return Ok(ToolResult::err(
                    "PATH_NOT_FOUND",
                    &format!("路径不存在: {}", path),
                ))
            }
        };

        let is_dir = meta.is_dir();
        let bytes = if is_dir {
            tree_bytes(target)
        } else {
            meta.len()
        };
        let mut payload = serde_json::json!({
            "path": path,
            "kind": if is_dir { "dir" } else { "file" },
            "bytes": bytes,
            "readonly": meta.permissions().readonly(),
            "created": meta.created().ok().map(format_time),
            "modified": meta.modified().ok().map(format_time),
            "extension": target.extension().and_then(|e| e.to_str()),
        });

        if want_hash && !is_dir {
            if let Some(hash) = sha256_of(target, 64 * 1024 * 1024) {
                payload["sha256"] = serde_json::Value::String(hash);
            } else {
                payload["sha256"] = serde_json::Value::Null;
                payload["hash_skipped"] = serde_json::Value::String("文件超过 64MB".to_string());
            }
        }

        Ok(ToolResult::ok(payload))
    }
}

// ============================================================
// 按名检索
// ============================================================

/// 按文件名检索
pub struct SearchFilesTool;

#[async_trait]
impl AgentTool for SearchFilesTool {
    fn name(&self) -> &'static str {
        "search_files"
    }

    fn description(&self) -> &'static str {
        "在目录下按文件名通配符（* 与 ?）递归检索文件，例如「找一下所有 *.pdf」。只匹配名字，不读内容。"
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "root": { "type": "string", "description": "检索起点目录" },
                "pattern": { "type": "string", "description": "文件名通配符，如 *.pdf 或 report_?.docx" },
                "max_results": { "type": "integer", "description": "最多返回多少条，默认 200" },
                "max_depth": { "type": "integer", "description": "最大递归深度，默认 8" },
                "include_dirs": { "type": "boolean", "description": "是否也匹配目录名，默认 false" }
            },
            "required": ["root", "pattern"]
        })
    }

    fn risk_level(&self) -> RiskLevel {
        RiskLevel::Low
    }

    async fn execute(&self, args: serde_json::Value) -> AppResult<ToolResult> {
        let root = require_str(&args, "root")?;
        let pattern = require_str(&args, "pattern")?;
        let max_results = args["max_results"].as_u64().unwrap_or(200).min(2000) as usize;
        let max_depth = args["max_depth"].as_u64().unwrap_or(8).min(32) as usize;
        let include_dirs = args["include_dirs"].as_bool().unwrap_or(false);

        let base = PathBuf::from(root);
        if !base.is_dir() {
            return Ok(ToolResult::err(
                "ROOT_NOT_DIR",
                &format!("检索起点不是目录: {}", root),
            ));
        }

        let mut hits: Vec<serde_json::Value> = Vec::new();
        let mut stack: Vec<(PathBuf, usize)> = vec![(base.clone(), 0)];
        let mut scanned = 0usize;
        let mut truncated = false;

        while let Some((dir, depth)) = stack.pop() {
            if hits.len() >= max_results {
                truncated = true;
                break;
            }
            let entries = match fs::read_dir(&dir) {
                Ok(e) => e,
                Err(_) => continue,
            };
            for entry in entries.flatten() {
                let path = entry.path();
                let name = entry.file_name().to_string_lossy().to_string();
                let is_dir = path.is_dir();
                scanned += 1;

                if (!is_dir || include_dirs) && glob_match(pattern, &name) {
                    hits.push(serde_json::json!({
                        "path": path.to_string_lossy(),
                        "name": name,
                        "kind": if is_dir { "dir" } else { "file" },
                        "bytes": if is_dir { 0 } else { entry.metadata().map(|m| m.len()).unwrap_or(0) },
                        "modified": entry.metadata().ok().and_then(|m| m.modified().ok()).map(format_time),
                    }));
                    if hits.len() >= max_results {
                        truncated = true;
                        break;
                    }
                }

                if is_dir && depth < max_depth && !SKIP_DIRS.contains(&name.as_str()) {
                    stack.push((path, depth + 1));
                }
            }
        }

        Ok(ToolResult::ok(serde_json::json!({
            "root": root,
            "pattern": pattern,
            "matches": hits,
            "count": hits.len(),
            "scanned": scanned,
            "truncated": truncated,
        })))
    }
}

// ============================================================
// 内容检索
// ============================================================

/// 在目录下检索文本内容
pub struct SearchContentTool;

#[async_trait]
impl AgentTool for SearchContentTool {
    fn name(&self) -> &'static str {
        "search_content"
    }

    fn description(&self) -> &'static str {
        "在目录下的文本文件里检索关键字，返回命中的文件、行号与整行内容。用于「哪些文件提到了 xxx」。"
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "root": { "type": "string", "description": "检索起点目录" },
                "query": { "type": "string", "description": "要查找的关键字（子串匹配，不是正则）" },
                "file_pattern": { "type": "string", "description": "只查这些文件名，多个用逗号分隔，默认常见文本扩展名" },
                "max_results": { "type": "integer", "description": "最多返回多少处命中，默认 100" },
                "case_sensitive": { "type": "boolean", "description": "是否区分大小写，默认 false" }
            },
            "required": ["root", "query"]
        })
    }

    fn risk_level(&self) -> RiskLevel {
        RiskLevel::Low
    }

    async fn execute(&self, args: serde_json::Value) -> AppResult<ToolResult> {
        let root = require_str(&args, "root")?;
        let query = require_str(&args, "query")?;
        let max_results = args["max_results"].as_u64().unwrap_or(100).min(1000) as usize;
        let case_sensitive = args["case_sensitive"].as_bool().unwrap_or(false);

        let patterns: Vec<String> = args["file_pattern"]
            .as_str()
            .map(|s| {
                s.split(',')
                    .map(|p| p.trim().to_string())
                    .filter(|p| !p.is_empty())
                    .collect()
            })
            .unwrap_or_default();
        let explicit = !patterns.is_empty();

        let base = PathBuf::from(root);
        if !base.is_dir() {
            return Ok(ToolResult::err(
                "ROOT_NOT_DIR",
                &format!("检索起点不是目录: {}", root),
            ));
        }

        let needle = if case_sensitive {
            query.to_string()
        } else {
            query.to_lowercase()
        };

        let mut hits: Vec<serde_json::Value> = Vec::new();
        let mut stack: Vec<PathBuf> = vec![base];
        let mut files_read = 0usize;
        let mut truncated = false;

        'outer: while let Some(dir) = stack.pop() {
            let entries = match fs::read_dir(&dir) {
                Ok(e) => e,
                Err(_) => continue,
            };
            for entry in entries.flatten() {
                let path = entry.path();
                let name = entry.file_name().to_string_lossy().to_string();
                if path.is_dir() {
                    if !SKIP_DIRS.contains(&name.as_str()) {
                        stack.push(path);
                    }
                    continue;
                }

                let ext = path
                    .extension()
                    .and_then(|e| e.to_str())
                    .unwrap_or("")
                    .to_lowercase();
                let matched = if explicit {
                    patterns.iter().any(|p| glob_match(p, &name))
                } else {
                    DEFAULT_TEXT_EXT.contains(&ext.as_str())
                };
                if !matched {
                    continue;
                }

                // 大文件读进内存会把检索变成内存事故
                if entry.metadata().map(|m| m.len()).unwrap_or(0) > 4 * 1024 * 1024 {
                    continue;
                }
                let content = match fs::read_to_string(&path) {
                    Ok(c) => c,
                    Err(_) => continue,
                };
                files_read += 1;

                for (idx, line) in content.lines().enumerate() {
                    let hay = if case_sensitive {
                        line.to_string()
                    } else {
                        line.to_lowercase()
                    };
                    if hay.contains(&needle) {
                        let clipped: String = line.chars().take(300).collect();
                        hits.push(serde_json::json!({
                            "path": path.to_string_lossy(),
                            "line": idx + 1,
                            "text": clipped,
                        }));
                        if hits.len() >= max_results {
                            truncated = true;
                            break 'outer;
                        }
                    }
                }
            }
        }

        Ok(ToolResult::ok(serde_json::json!({
            "root": root,
            "query": query,
            "matches": hits,
            "count": hits.len(),
            "files_read": files_read,
            "truncated": truncated,
        })))
    }
}

// ============================================================
// 建目录
// ============================================================

/// 创建目录（含中间层级）
pub struct CreateDirectoryTool;

#[async_trait]
impl AgentTool for CreateDirectoryTool {
    fn name(&self) -> &'static str {
        "create_directory"
    }

    fn description(&self) -> &'static str {
        "创建目录，父目录不存在会自动一并创建。已存在时视为成功并标记 existed=true。"
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "要创建的目录路径" }
            },
            "required": ["path"]
        })
    }

    fn risk_level(&self) -> RiskLevel {
        RiskLevel::Low
    }

    async fn execute(&self, args: serde_json::Value) -> AppResult<ToolResult> {
        let path = require_str(&args, "path")?;
        let target = Path::new(path);
        let existed = target.is_dir();

        if target.exists() && !existed {
            return Ok(ToolResult::err(
                "PATH_IS_FILE",
                &format!("同名文件已存在: {}", path),
            ));
        }

        match fs::create_dir_all(target) {
            Ok(()) => Ok(ToolResult::ok(serde_json::json!({
                "path": path,
                "existed": existed,
                "is_dir": target.is_dir(),
            }))),
            Err(e) => Ok(ToolResult::err(
                "CREATE_FAILED",
                &format!("创建目录失败: {}", e),
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn glob_covers_star_and_question() {
        assert!(glob_match("*.pdf", "报告.pdf"));
        assert!(glob_match("*.pdf", "a.PDF"), "Windows 侧大小写不敏感");
        assert!(!glob_match("*.pdf", "报告.docx"));
        assert!(glob_match("report_?.docx", "report_1.docx"));
        assert!(!glob_match("report_?.docx", "report_12.docx"));
        assert!(glob_match("a*b*c", "axxbyyc"));
        assert!(glob_match("*", "任意名字"));
        assert!(!glob_match("*.txt", "txt"));
    }

    #[test]
    fn copy_tree_preserves_nested_files() {
        let base = std::env::temp_dir().join(format!("pcg-fsx-{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        let src = base.join("src");
        fs::create_dir_all(src.join("nested")).unwrap();
        fs::write(src.join("a.txt"), "hello").unwrap();
        fs::write(src.join("nested/b.txt"), "world!").unwrap();

        let dst = base.join("dst");
        let bytes = copy_tree(&src, &dst).expect("复制成功");
        assert_eq!(bytes, 11);
        assert_eq!(tree_bytes(&dst), 11);
        assert!(dst.join("nested/b.txt").exists());

        let _ = fs::remove_dir_all(&base);
    }

    /// 删除必须真的让路径消失，且 exists_after 要能反映现实。
    #[test]
    fn file_info_reports_hash_and_size() {
        let base = std::env::temp_dir().join(format!("pcg-fsx-hash-{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(&base).unwrap();
        let file = base.join("h.txt");
        fs::write(&file, "abc").unwrap();

        let hash = sha256_of(&file, 1024).expect("摘要");
        // "abc" 的 SHA-256 是固定值，用它证明哈希算的是内容而不是元数据
        assert_eq!(
            hash,
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert!(sha256_of(&file, 1).is_none(), "超过上限应放弃计算");

        let _ = fs::remove_dir_all(&base);
    }
}
