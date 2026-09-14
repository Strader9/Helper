//! GitHub 工具
//!
//! 通过 GitHub Search API 获取趋势 / 搜索仓库，并将趋势快照持久化到
//! `github_snapshots` 表（数据库只存元数据 + JSON 文件路径，真实数据落盘）。
//! 无需鉴权（未登录限速 60 次/小时，足够本地使用）。

use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::Mutex;

use async_trait::async_trait;
use serde_json::{json, Value};

use crate::db::Database;
use crate::error::{AppError, AppResult};
use crate::tools::{AgentTool, RiskLevel, ToolResult};

const GITHUB_API_BASE: &str = "https://api.github.com";

/// 根据 range 计算“趋势”时间窗口的起始日期
fn range_start_date(range: &str) -> String {
    let days = match range.to_lowercase().as_str() {
        "daily" | "day" => 1,
        "weekly" | "week" => 7,
        "monthly" | "month" => 30,
        _ => 1,
    };
    let start = chrono::Utc::now() - chrono::Duration::days(days);
    start.format("%Y-%m-%d").to_string()
}

/// 调用 GitHub Search API 拉取仓库列表
///
/// - `query` 为空时，使用 `stars:>N pushed:>DATE` 近似“近期活跃热门”趋势
/// - `language` 可选，进一步过滤
pub async fn fetch_github_repos(
    range: &str,
    language: Option<&str>,
    query: Option<&str>,
    limit: usize,
) -> AppResult<Vec<Value>> {
    let client = reqwest::Client::builder()
        .user_agent("pc-guardian/0.1")
        .build()?;

    let mut q = match query {
        Some(q) if !q.trim().is_empty() => q.trim().to_string(),
        _ => {
            let date = range_start_date(range);
            format!("stars:>200 pushed:>{}", date)
        }
    };
    if let Some(lang) = language {
        if !lang.trim().is_empty() {
            q.push_str(&format!(" language:{}", lang.trim()));
        }
    }

    let per_page = limit.clamp(1, 100);
    let url = format!(
        "{}/search/repositories?q={}&sort=stars&order=desc&per_page={}",
        GITHUB_API_BASE,
        urlencoding::encode(&q),
        per_page
    );

    let resp = client
        .get(&url)
        .header("Accept", "application/vnd.github+json")
        .send()
        .await?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(AppError::Internal(format!(
            "GitHub API 返回 {}: {}",
            status, body
        )));
    }

    let json: Value = resp.json().await?;
    let items = json
        .get("items")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();

    let repos: Vec<Value> = items
        .iter()
        .map(|item| {
            json!({
                "name": item.get("name").cloned().unwrap_or(Value::Null),
                "full_name": item.get("full_name").cloned().unwrap_or(Value::Null),
                "description": item.get("description").cloned().unwrap_or(Value::Null),
                "html_url": item.get("html_url").cloned().unwrap_or(Value::Null),
                "stars": item.get("stargazers_count").cloned().unwrap_or(Value::Null),
                "forks": item.get("forks_count").cloned().unwrap_or(Value::Null),
                "language": item.get("language").cloned().unwrap_or(Value::Null),
                "owner": item.get("owner").and_then(|o| o.get("login")).cloned().unwrap_or(Value::Null),
                "updated_at": item.get("pushed_at").cloned().unwrap_or(Value::Null),
            })
        })
        .collect();

    Ok(repos)
}

/// 保存趋势快照：将仓库列表写入 JSON 文件，并在 `github_snapshots` 表记录元数据
pub fn save_github_snapshot(
    db: &Database,
    app_dir: &std::path::Path,
    range: &str,
    language: Option<&str>,
    repos: &[Value],
) -> AppResult<String> {
    let snap_dir = app_dir.join("github").join("snapshots");
    std::fs::create_dir_all(&snap_dir)?;

    let lang_tag = language
        .filter(|l| !l.trim().is_empty())
        .map(|l| l.trim().to_string())
        .unwrap_or_else(|| "all".to_string());
    let ts = chrono::Local::now().format("%Y%m%d_%H%M%S").to_string();
    let file_name = format!("{}_{}_{}.json", range, lang_tag, ts);
    let file_path = snap_dir.join(&file_name);

    let json_text = serde_json::to_string_pretty(repos)?;
    std::fs::write(&file_path, json_text)?;

    let file_path_str = file_path.to_string_lossy().to_string();
    let snapshot_date = chrono::Local::now().format("%Y-%m-%d").to_string();
    db.insert_github_snapshot(
        &snapshot_date,
        range,
        language,
        &file_path_str,
        repos.len() as i64,
    )?;

    Ok(file_path_str)
}

// ============================================================
// 工具定义
// ============================================================

/// GitHub 趋势仓库工具
pub struct GithubTrendingTool {
    db: Arc<Mutex<Database>>,
    app_dir: PathBuf,
}

impl GithubTrendingTool {
    pub fn new(db: Arc<Mutex<Database>>, app_dir: PathBuf) -> Self {
        Self { db, app_dir }
    }
}

#[async_trait]
impl AgentTool for GithubTrendingTool {
    fn name(&self) -> &'static str {
        "github_trending"
    }

    fn description(&self) -> &'static str {
        "获取 GitHub 趋势仓库（按 star 排序的近期活跃热门仓库）。可指定时间范围(daily/weekly/monthly)与编程语言。结果会保存为趋势快照。风险等级 LOW。"
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "range": {
                    "type": "string",
                    "description": "时间范围：daily / weekly / monthly，默认 daily"
                },
                "language": {
                    "type": "string",
                    "description": "可选，按编程语言过滤，如 rust / python"
                },
                "limit": {
                    "type": "integer",
                    "description": "返回数量，默认 20，最大 100"
                }
            },
            "additionalProperties": false
        })
    }

    fn risk_level(&self) -> RiskLevel {
        RiskLevel::Low
    }

    async fn execute(&self, args: Value) -> AppResult<ToolResult> {
        let range = args
            .get("range")
            .and_then(|v| v.as_str())
            .unwrap_or("daily");
        let language = args.get("language").and_then(|v| v.as_str());
        let limit = args
            .get("limit")
            .and_then(|v| v.as_u64())
            .unwrap_or(20) as usize;

        let repos = fetch_github_repos(range, language, None, limit).await?;

        // 保存快照（失败不影响返回结果）
        {
            let db_guard = self.db.lock().await;
            if let Err(e) =
                save_github_snapshot(&db_guard, &self.app_dir, range, language, &repos)
            {
                eprintln!("[GitHub] 保存快照失败: {}", e);
            }
        }

        Ok(ToolResult::ok(json!({
            "repos": repos,
            "count": repos.len(),
            "range": range,
        })))
    }
}

/// GitHub 仓库搜索工具
pub struct GithubSearchTool {
    db: Arc<Mutex<Database>>,
    app_dir: PathBuf,
}

impl GithubSearchTool {
    pub fn new(db: Arc<Mutex<Database>>, app_dir: PathBuf) -> Self {
        Self { db, app_dir }
    }
}

#[async_trait]
impl AgentTool for GithubSearchTool {
    fn name(&self) -> &'static str {
        "github_search"
    }

    fn description(&self) -> &'static str {
        "在 GitHub 上搜索仓库。当用户说‘搜索 xxx 相关的项目’、‘找一下做 yyy 的开源库’时使用。返回匹配的仓库列表。风险等级 LOW。"
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "搜索关键词，支持 GitHub 搜索语法，如 'react state management'"
                },
                "language": {
                    "type": "string",
                    "description": "可选，按编程语言过滤"
                },
                "limit": {
                    "type": "integer",
                    "description": "返回数量，默认 20，最大 100"
                }
            },
            "required": ["query"],
            "additionalProperties": false
        })
    }

    fn risk_level(&self) -> RiskLevel {
        RiskLevel::Low
    }

    async fn execute(&self, args: Value) -> AppResult<ToolResult> {
        let query = args
            .get("query")
            .and_then(|v| v.as_str())
            .ok_or_else(|| AppError::InvalidArgument("query is required".to_string()))?;
        let language = args.get("language").and_then(|v| v.as_str());
        let limit = args
            .get("limit")
            .and_then(|v| v.as_u64())
            .unwrap_or(20) as usize;

        let repos = fetch_github_repos("daily", language, Some(query), limit).await?;

        {
            let db_guard = self.db.lock().await;
            if let Err(e) =
                save_github_snapshot(&db_guard, &self.app_dir, "search", language, &repos)
            {
                eprintln!("[GitHub] 保存快照失败: {}", e);
            }
        }

        Ok(ToolResult::ok(json!({
            "repos": repos,
            "count": repos.len(),
            "query": query,
        })))
    }
}
