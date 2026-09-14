//! 数据库模块
//!
//! SQLite 嵌入式数据库，单文件存储。
//! 负责：连接管理、迁移执行、基础 CRUD、对话管理。

pub mod conversation;
pub mod schema;

use rusqlite::{Connection, params};
use std::path::Path;

use crate::error::AppResult;
use crate::commands::SettingItem;
use crate::agent::observation::Observation as AgentObservation;

/// 数据库连接封装
pub struct Database {
    conn: Connection,
}

impl Database {
    /// 打开数据库连接并配置连接参数
    pub fn new(path: &Path) -> AppResult<Self> {
        let conn = Connection::open(path)?;

        // 连接级配置：性能与安全平衡
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        conn.pragma_update(None, "busy_timeout", "5000")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.pragma_update(None, "cache_size", "-20000")?; // ~20MB

        Ok(Self { conn })
    }

    /// 初始化数据库 schema
    ///
    /// 创建所有表和索引。Phase 1 一次性建完所有表，
    /// 避免后续多次迁移。
    pub fn init_schema(&self) -> AppResult<()> {
        self.conn.execute_batch(schema::SCHEMA_SQL)?;
        self.seed_default_settings()?;
        self.seed_default_directories()?;
        self.run_phase4_migration()?;
        Ok(())
    }

    /// 执行 Phase 4 迁移
    fn run_phase4_migration(&self) -> AppResult<()> {
        self.conn.execute_batch(schema::MIGRATION_PHASE4_SQL)?;
        Ok(())
    }

    /// 写入默认配置项
    fn seed_default_settings(&self) -> AppResult<()> {
        let defaults = [
            ("ollama.base_url", "http://localhost:11434", "string", "Ollama 服务地址"),
            ("ollama.model", "qwen3:8b", "string", "默认模型名称"),
            ("ollama.timeout_ms", "30000", "number", "请求超时（毫秒）"),
            ("agent.max_iterations", "10", "number", "Agent Loop 最大轮次"),
            ("agent.temperature", "0.7", "number", "模型温度"),
            ("monitor.cpu_interval_ms", "2000", "number", "CPU 采集间隔"),
            ("monitor.process_interval_ms", "5000", "number", "进程采集间隔"),
            ("monitor.alert_cpu_threshold", "90", "number", "CPU 告警阈值（%）"),
            ("monitor.alert_memory_threshold", "90", "number", "内存告警阈值（%）"),
            ("monitor.alert_disk_threshold", "10", "number", "磁盘剩余告警阈值（%）"),
            ("ui.theme", "dark", "string", "主题（dark/light）"),
            ("ui.language", "zh-CN", "string", "界面语言"),
            ("security.confirm_medium_risk", "true", "boolean", "MEDIUM 风险是否弹窗确认"),
            ("tray.minimize_on_close", "true", "boolean", "关闭窗口时最小化到托盘"),
            ("startup.auto_launch", "false", "boolean", "开机自启"),
            ("global_shortcut", "Alt+Space", "string", "全局快捷键（如 Alt+Space）"),
            ("web_search.provider", "", "string", "Web 搜索 Provider"),
            ("github.token_set", "false", "boolean", "GitHub Token 是否已设置"),
        ];

        let mut stmt = self.conn.prepare(
            "INSERT OR IGNORE INTO settings (key, value, value_type, description) VALUES (?1, ?2, ?3, ?4)"
        )?;

        for (key, value, value_type, description) in defaults {
            stmt.execute(params![key, value, value_type, description])?;
        }

        Ok(())
    }

    /// 写入默认允许目录
    fn seed_default_directories(&self) -> AppResult<()> {
        // 注意：环境变量在运行时展开，这里只存占位符模式
        let defaults = [
            ("%USERPROFILE%\\Desktop", "桌面", "read", 1),
            ("%USERPROFILE%\\Documents", "文档", "read", 1),
            ("%USERPROFILE%\\Downloads", "下载", "read", 1),
        ];

        let mut stmt = self.conn.prepare(
            "INSERT OR IGNORE INTO allowed_directories (path, label, access_level, is_default) VALUES (?1, ?2, ?3, ?4)"
        )?;

        for (path, label, access_level, is_default) in defaults {
            stmt.execute(params![path, label, access_level, is_default])?;
        }

        Ok(())
    }

    /// 获取所有配置项
    pub fn get_all_settings(&self) -> AppResult<Vec<SettingItem>> {
        let mut stmt = self.conn.prepare(
            "SELECT key, value, value_type, description FROM settings ORDER BY key"
        )?;

        let items = stmt.query_map([], |row| {
            Ok(SettingItem {
                key: row.get(0)?,
                value: row.get(1)?,
                value_type: row.get(2)?,
                description: row.get(3)?,
            })
        })?;

        Ok(items.collect::<Result<Vec<_>, _>>()?)
    }

    /// 获取单个配置项
    pub fn get_setting(&self, key: &str) -> AppResult<Option<String>> {
        let mut stmt = self.conn.prepare(
            "SELECT value FROM settings WHERE key = ?1"
        )?;
        let mut rows = stmt.query(params![key])?;
        if let Some(row) = rows.next()? {
            Ok(Some(row.get(0)?))
        } else {
            Ok(None)
        }
    }

    /// 更新配置项
    pub fn update_setting(&self, key: &str, value: &str) -> AppResult<()> {
        self.conn.execute(
            "UPDATE settings SET value = ?1, updated_at = datetime('now') WHERE key = ?2",
            params![value, key],
        )?;
        Ok(())
    }

    /// 获取数据库连接引用（供复杂查询使用）
    pub fn connection(&self) -> &Connection {
        &self.conn
    }

    // ============================================================
    // Phase 4: 安全相关数据库操作
    // ============================================================

    /// 添加可信目录
    pub fn add_allowed_directory(&self, path: &str, label: &str, access_level: &str) -> AppResult<i64> {
        self.conn.execute(
            "INSERT OR REPLACE INTO allowed_directories (path, label, access_level, is_default) VALUES (?1, ?2, ?3, 0)",
            params![path, label, access_level],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    /// 删除可信目录
    pub fn remove_allowed_directory(&self, path: &str) -> AppResult<()> {
        self.conn.execute(
            "DELETE FROM allowed_directories WHERE path = ?1 AND is_default = 0",
            params![path],
        )?;
        Ok(())
    }

    /// 获取所有可信目录
    pub fn get_allowed_directories(&self) -> AppResult<Vec<AllowedDirectory>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, path, label, access_level, is_default, added_at FROM allowed_directories ORDER BY is_default DESC, added_at DESC"
        )?;

        let dirs = stmt.query_map([], |row| {
            Ok(AllowedDirectory {
                id: row.get(0)?,
                path: row.get(1)?,
                label: row.get(2)?,
                access_level: row.get(3)?,
                is_default: row.get::<_, i64>(4)? != 0,
                added_at: row.get(5)?,
            })
        })?;

        Ok(dirs.collect::<Result<Vec<_>, _>>()?)
    }

    /// 添加信任工具
    pub fn add_trusted_tool(&self, tool_name: &str, path_pattern: Option<&str>, risk_level: &str) -> AppResult<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO trusted_tools (tool_name, path_pattern, risk_level) VALUES (?1, ?2, ?3)",
            params![tool_name, path_pattern, risk_level],
        )?;
        Ok(())
    }

    /// 删除信任工具
    pub fn remove_trusted_tool(&self, tool_name: &str, path_pattern: Option<&str>) -> AppResult<()> {
        self.conn.execute(
            "DELETE FROM trusted_tools WHERE tool_name = ?1 AND path_pattern IS ?2",
            params![tool_name, path_pattern],
        )?;
        Ok(())
    }

    /// 获取所有信任工具
    pub fn get_trusted_tools(&self) -> AppResult<Vec<TrustedTool>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, tool_name, path_pattern, risk_level, added_at FROM trusted_tools ORDER BY added_at DESC"
        )?;

        let tools = stmt.query_map([], |row| {
            Ok(TrustedTool {
                id: row.get(0)?,
                tool_name: row.get(1)?,
                path_pattern: row.get(2)?,
                risk_level: row.get(3)?,
                added_at: row.get(4)?,
            })
        })?;

        Ok(tools.collect::<Result<Vec<_>, _>>()?)
    }

    /// 写入审计日志
    pub fn write_audit_log(
        &self,
        session_id: &str,
        tool_name: &str,
        tool_risk_level: &str,
        arguments: Option<&str>,
        permission_result: &str,
        success: bool,
        error_code: Option<&str>,
        error_message: Option<&str>,
        duration_ms: Option<i64>,
        result_summary: Option<&str>,
    ) -> AppResult<()> {
        self.conn.execute(
            "INSERT INTO audit_logs (session_id, tool_name, tool_risk_level, arguments, permission_result, success, error_code, error_message, duration_ms, result_summary)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                session_id,
                tool_name,
                tool_risk_level,
                arguments,
                permission_result,
                success as i64,
                error_code,
                error_message,
                duration_ms,
                result_summary,
            ],
        )?;
        Ok(())
    }

    // ============================================================
    // V10: GitHub 趋势快照持久化
    // ============================================================

    /// 插入一条 GitHub 趋势快照（同日同范围同语言已存在则替换）
    ///
    /// 真实的仓库数据落盘为 JSON 文件，数据库只保存元数据指针（file_path）。
    pub fn insert_github_snapshot(
        &self,
        snapshot_date: &str,
        range: &str,
        language: Option<&str>,
        file_path: &str,
        repo_count: i64,
    ) -> AppResult<i64> {
        self.conn.execute(
            "INSERT OR REPLACE INTO github_snapshots (snapshot_date, range, language, file_path, repo_count, generated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, datetime('now'))",
            params![snapshot_date, range, language, file_path, repo_count],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    // ============================================================
    // V13: Agent Observation 持久化
    // ============================================================

    /// 插入一条 Agent 观察记录
    pub fn insert_observation(&self, obs: &AgentObservation) -> AppResult<()> {
        let data_json = serde_json::to_string(&obs.data)?;
        let source_str = match obs.source {
            crate::agent::observation::ObservationSource::ToolResult => "tool_result",
            crate::agent::observation::ObservationSource::SystemMonitor => "system_monitor",
            crate::agent::observation::ObservationSource::Process => "process",
            crate::agent::observation::ObservationSource::Window => "window",
            crate::agent::observation::ObservationSource::FileSystem => "file_system",
            crate::agent::observation::ObservationSource::Network => "network",
            crate::agent::observation::ObservationSource::Application => "application",
        };

        self.conn.execute(
            "INSERT OR REPLACE INTO agent_observations (
                id, task_id, step_id, source, tool_name, success, data, summary, timestamp
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                &obs.id,
                &obs.task_id,
                &obs.step_id,
                source_str,
                &obs.tool_name,
                obs.success as i64,
                &data_json,
                &obs.summary,
                obs.timestamp,
            ],
        )?;
        Ok(())
    }

    /// 根据任务 ID 获取所有观察记录（按时间升序）
    pub fn get_observations_by_task(&self, task_id: &str) -> AppResult<Vec<AgentObservation>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, task_id, step_id, source, tool_name, success, data, summary, timestamp
             FROM agent_observations WHERE task_id = ?1 ORDER BY timestamp ASC"
        )?;

        let rows = stmt.query_map(params![task_id], |row| {
            let source_str: String = row.get(3)?;
            let source = match source_str.as_str() {
                "tool_result" => crate::agent::observation::ObservationSource::ToolResult,
                "system_monitor" => crate::agent::observation::ObservationSource::SystemMonitor,
                "process" => crate::agent::observation::ObservationSource::Process,
                "window" => crate::agent::observation::ObservationSource::Window,
                "file_system" => crate::agent::observation::ObservationSource::FileSystem,
                "network" => crate::agent::observation::ObservationSource::Network,
                _ => crate::agent::observation::ObservationSource::Application,
            };

            let data_str: String = row.get(6)?;
            let data: serde_json::Value = serde_json::from_str(&data_str).unwrap_or(serde_json::Value::Null);

            Ok(AgentObservation {
                id: row.get(0)?,
                task_id: row.get(1)?,
                step_id: row.get(2)?,
                source,
                tool_name: row.get(4)?,
                success: row.get::<_, i64>(5)? != 0,
                data,
                summary: row.get(7)?,
                timestamp: row.get(8)?,
            })
        })?;

        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }
}

/// 允许目录条目
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct AllowedDirectory {
    pub id: i64,
    pub path: String,
    pub label: Option<String>,
    pub access_level: String,
    pub is_default: bool,
    pub added_at: String,
}

/// 信任工具条目
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct TrustedTool {
    pub id: i64,
    pub tool_name: String,
    pub path_pattern: Option<String>,
    pub risk_level: String,
    pub added_at: String,
}
