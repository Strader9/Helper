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

/// 凭据类配置键：这些键的值不允许离开后端进程。
///
/// 判断依据是"值本身就是凭据"——即拿到这个字符串就等于拿到了访问权。
/// 像 `publish.github_repo` 这种只是名字，不属于此列；`ollama.base_url`
/// 是内网地址，也不算凭据。
pub fn is_secret_setting(key: &str) -> bool {
    matches!(
        key,
        "publish.github_token" | "publish.netlify_token"
    ) || ["token", "secret", "password", "api_key"]
        .iter()
        .any(|suffix| key.ends_with(suffix))
}

/// 把凭据值替换成掩码，只保留头尾极少量字符用于人工辨认。
///
/// 设计取舍：完全不显示的话，用户无法确认"我填的到底是哪个 token"，
/// 于是会反复重填。保留末 4 位是最小可辨认信息——足以区分多个令牌，
/// 又不足以还原凭据本身（GitHub PAT 是 40 位随机串，末 4 位没有利用价值）。
///
/// 空值保持空值：前端据此区分"未配置"与"已配置"。
pub fn mask_secret(value: &str) -> String {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    let chars: Vec<char> = trimmed.chars().collect();
    if chars.len() <= 8 {
        // 短值连末 4 位都不该透出，整体打码
        return "••••••••".to_string();
    }
    let tail: String = chars[chars.len() - 4..].iter().collect();
    format!("••••••••{tail}")
}

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
        self.ensure_settings_metadata_columns()?;
        self.seed_default_settings()?;
        self.purge_phantom_settings()?;
        self.refresh_setting_descriptions()?;
        self.seed_default_directories()?;
        self.run_phase4_migration()?;
        self.ensure_session_summary_columns()?;
        self.ensure_receipt_evidence_column()?;
        self.run_knowledge_migration()?;
        self.run_scheduler_migration()?;
        self.run_devices_migration()?;
        self.run_workflow_migration()?;
        self.run_work_takeover_migration()?;
        self.run_extension_migration()?;
        self.ensure_memory_tier_columns()?;
        Ok(())
    }

    /// 给已存在的 tool_receipts 表补上 verification_evidence 列。
    ///
    /// 老库里的回执没有独立探查证据，补列后历史行该字段为 NULL——
    /// 这是正确语义：当时确实没做过探查，不能事后追认。
    fn ensure_receipt_evidence_column(&self) -> AppResult<()> {
        let existing: Vec<String> = {
            let mut stmt = self.conn.prepare("PRAGMA table_info(tool_receipts)")?;
            let names: Vec<String> = stmt
                .query_map([], |row| row.get::<_, String>(1))?
                .filter_map(|r| r.ok())
                .collect();
            names
        };

        // 表可能还没建（首次启动会由 SCHEMA_SQL 建好），此时无需迁移。
        if existing.is_empty() {
            return Ok(());
        }

        if !existing.iter().any(|c| c == "verification_evidence") {
            self.conn
                .execute_batch("ALTER TABLE tool_receipts ADD COLUMN verification_evidence TEXT")?;
        }
        Ok(())
    }

    /// 给已存在的 settings 表补上 min_value / max_value / unit 三列。
    ///
    /// 与 `ensure_session_summary_columns` 同样的理由：SQLite 没有
    /// `ADD COLUMN IF NOT EXISTS`，重复 ALTER 会报 duplicate column name，
    /// 所以先查 `PRAGMA table_info` 再决定是否执行。
    fn ensure_settings_metadata_columns(&self) -> AppResult<()> {
        let existing: Vec<String> = {
            let mut stmt = self.conn.prepare("PRAGMA table_info(settings)")?;
            let names: Vec<String> = stmt
                .query_map([], |row| row.get::<_, String>(1))?
                .filter_map(|r| r.ok())
                .collect();
            names
        };

        for (column, decl) in [
            ("min_value", "REAL"),
            ("max_value", "REAL"),
            ("unit", "TEXT"),
        ] {
            if !existing.iter().any(|c| c == column) {
                self.conn
                    .execute_batch(&format!("ALTER TABLE settings ADD COLUMN {} {}", column, decl))?;
            }
        }
        Ok(())
    }

    /// 用最新的默认说明覆盖老库里的说明文本与取值范围。
    ///
    /// 老库里存的是写入当时的说明，随着实现演进可能已经不准确
    /// （典型例子：超时默认值从 30000 改成 60000、快捷键从 Alt+Space
    /// 改成 Ctrl+Shift+Space）。**只更新说明与元数据，不动用户填的值。**
    fn refresh_setting_descriptions(&self) -> AppResult<()> {
        let updates: [(&str, &str, Option<f64>, Option<f64>, Option<&str>); 20] = [
            ("ollama.base_url", "Ollama 服务地址", None, None, None),
            ("ollama.model", "默认模型名称", None, None, None),
            ("ollama.timeout_ms", "请求超时（毫秒）", Some(5000.0), Some(300000.0), Some("ms")),
            ("agent.max_iterations", "Agent Loop 最大轮次", Some(1.0), Some(50.0), Some("轮")),
            ("agent.temperature", "模型温度（越高越发散）", Some(0.0), Some(2.0), None),
            ("agent.num_ctx", "上下文窗口（token）", Some(2048.0), Some(32768.0), Some("token")),
            ("agent.num_predict", "单次回复上限（token）", Some(128.0), Some(8192.0), Some("token")),
            ("agent.proactive_interval_sec", "主动引擎检查间隔", Some(5.0), Some(300.0), Some("s")),
            ("agent.proactive_cooldown_sec", "同一规则触发冷却期", Some(30.0), Some(3600.0), Some("s")),
            ("ui.theme", "主题（dark/light）", None, None, None),
            ("security.confirm_medium_risk", "MEDIUM 风险是否弹窗确认", None, None, None),
            ("tray.minimize_on_close", "关闭窗口时最小化到托盘", None, None, None),
            ("startup.auto_launch", "开机自启", None, None, None),
            ("global_shortcut", "全局快捷键（如 Ctrl+Shift+Space）", None, None, None),
            ("browser.default", "优先使用的浏览器（auto/chrome/edge/...）", None, None, None),
            ("browser.cdp_port", "浏览器远程调试端口", Some(1024.0), Some(65535.0), None),
            ("publish.github_token", "GitHub Personal Access Token", None, None, None),
            ("publish.github_repo", "GitHub Pages 仓库名", None, None, None),
            ("publish.netlify_token", "Netlify 备用发布通道令牌", None, None, None),
            ("publish.proxy", "发布用代理地址（留空自动）", None, None, None),
        ];

        let mut stmt = self.conn.prepare(
            "UPDATE settings SET description = ?2, min_value = ?3, max_value = ?4, unit = ?5 WHERE key = ?1"
        )?;
        for (key, description, min_value, max_value, unit) in updates {
            stmt.execute(params![key, description, min_value, max_value, unit])?;
        }
        Ok(())
    }

    /// 给已存在的 chat_sessions 补上摘要相关字段。
    ///
    /// SQLite 没有 `ADD COLUMN IF NOT EXISTS`，而对旧库重复执行 `ALTER TABLE`
    /// 会报 "duplicate column name"。所以先查 `PRAGMA table_info` 再决定，
    /// 保证本方法可以反复调用。
    fn ensure_session_summary_columns(&self) -> AppResult<()> {
        let existing: Vec<String> = {
            let mut stmt = self
                .conn
                .prepare("PRAGMA table_info(chat_sessions)")?;
            let names: Vec<String> = stmt
                .query_map([], |row| row.get::<_, String>(1))?
                .filter_map(|r| r.ok())
                .collect();
            names
        };

        if !existing.iter().any(|c| c == "summary") {
            self.conn
                .execute_batch("ALTER TABLE chat_sessions ADD COLUMN summary TEXT")?;
        }
        if !existing.iter().any(|c| c == "summary_upto_id") {
            self.conn
                .execute_batch("ALTER TABLE chat_sessions ADD COLUMN summary_upto_id INTEGER")?;
        }
        Ok(())
    }

    /// 执行 Phase 4 迁移
    fn run_phase4_migration(&self) -> AppResult<()> {
        self.conn.execute_batch(schema::MIGRATION_PHASE4_SQL)?;
        Ok(())
    }

    /// 建本地知识库的两张表（已有库靠 IF NOT EXISTS 平滑升级）。
    fn run_knowledge_migration(&self) -> AppResult<()> {
        self.conn.execute_batch(schema::MIGRATION_KNOWLEDGE_SQL)?;
        Ok(())
    }

    /// 建常驻调度的两张表（已有库靠 IF NOT EXISTS 平滑升级）。
    fn run_scheduler_migration(&self) -> AppResult<()> {
        self.conn.execute_batch(schema::MIGRATION_SCHEDULER_SQL)?;
        Ok(())
    }

    /// 建多端（设备）两张表。
    fn run_devices_migration(&self) -> AppResult<()> {
        self.conn.execute_batch(schema::MIGRATION_DEVICES_SQL)?;
        Ok(())
    }

    /// 建工作流引擎的三张表（定义 / 运行 / 步骤）。
    fn run_workflow_migration(&self) -> AppResult<()> {
        self.conn.execute_batch(schema::MIGRATION_WORKFLOW_SQL)?;
        Ok(())
    }

    /// 建工作接管的两张表（任务台账 / 证据工作流）。
    fn run_work_takeover_migration(&self) -> AppResult<()> {
        self.conn.execute_batch(schema::MIGRATION_WORK_TAKEOVER_SQL)?;
        Ok(())
    }

    /// 建扩展注册表两张表（扩展清单 / 生命周期回执）。
    fn run_extension_migration(&self) -> AppResult<()> {
        self.conn.execute_batch(schema::MIGRATION_EXTENSION_SQL)?;
        Ok(())
    }

    /// 给已存在的 memory_items 表补上记忆分层两列（tier / consolidated_into）。
    ///
    /// 老库没有这两列时补上；`tier` 给默认 'episodic'，`consolidated_into` 为 NULL——
    /// 历史记忆尚未固化，语义正确（不能事后追认已经固化）。
    fn ensure_memory_tier_columns(&self) -> AppResult<()> {
        let existing: Vec<String> = {
            let mut stmt = self.conn.prepare("PRAGMA table_info(memory_items)")?;
            let names: Vec<String> = stmt
                .query_map([], |row| row.get::<_, String>(1))?
                .filter_map(|r| r.ok())
                .collect();
            names
        };

        // 表可能还没建（首次启动由 SCHEMA_SQL 建好），此时无需迁移。
        if existing.is_empty() {
            return Ok(());
        }

        if !existing.iter().any(|c| c == "tier") {
            self.conn
                .execute_batch("ALTER TABLE memory_items ADD COLUMN tier TEXT NOT NULL DEFAULT 'episodic'")?;
        }
        if !existing.iter().any(|c| c == "consolidated_into") {
            self.conn
                .execute_batch("ALTER TABLE memory_items ADD COLUMN consolidated_into TEXT")?;
        }
        Ok(())
    }

    /// 写入默认配置项
    ///
    /// 这里只收录**真正有消费点**的配置：每一项都能在源码里找到读取它的地方，
    /// 改了它行为就会变。曾有一批"看着像功能、实则无人读取"的假配置
    /// （ui.language、web_search.provider、github.token_set 等），
    /// 已由 `purge_phantom_settings` 从老库中清理。
    ///
    /// 元组含义：(key, 默认值, 类型, 说明, 最小值, 最大值, 单位)
    fn seed_default_settings(&self) -> AppResult<()> {
        let defaults: [(&str, &str, &str, &str, Option<f64>, Option<f64>, Option<&str>); 28] = [
            // ---- 模型连接 ----
            ("ollama.base_url", "http://localhost:11434", "string", "Ollama 服务地址", None, None, None),
            ("ollama.model", "qwen3:8b", "string", "默认模型名称", None, None, None),
            ("ollama.timeout_ms", "60000", "number", "请求超时（毫秒）", Some(5000.0), Some(300000.0), Some("ms")),
            // sidecar 多 provider 网关（模型层扩容）的地址；MCP 转发也走这里
            ("sidecar.url", "http://127.0.0.1:4770", "string", "模型网关（sidecar）地址", None, None, None),
            // ---- Agent 行为 ----
            ("agent.max_iterations", "10", "number", "Agent Loop 最大轮次", Some(1.0), Some(50.0), Some("轮")),
            ("agent.temperature", "0.7", "number", "模型温度（越高越发散）", Some(0.0), Some(2.0), None),
            // ---- 上下文窗口 ----
            // 8GB 显存下 8192 可全程跑 GPU，再大就会溢出到 CPU 让响应变慢。
            ("agent.num_ctx", "8192", "number", "上下文窗口（token）", Some(2048.0), Some(32768.0), Some("token")),
            ("agent.num_predict", "1024", "number", "单次回复上限（token）", Some(128.0), Some(8192.0), Some("token")),
            // ---- 主动助手 ----
            ("agent.proactive_interval_sec", "15", "number", "主动引擎检查间隔", Some(5.0), Some(300.0), Some("s")),
            ("agent.proactive_cooldown_sec", "300", "number", "同一规则触发冷却期", Some(30.0), Some(3600.0), Some("s")),
            // ---- 常驻调度 ----
            ("agent.scheduler_enabled", "true", "boolean", "启用常驻定时任务调度", None, None, None),
            ("agent.scheduler_tick_sec", "30", "number", "调度器检查间隔", Some(5.0), Some(300.0), Some("s")),
            ("agent.scheduler_job_timeout_sec", "600", "number", "单个定时任务执行超时", Some(60.0), Some(1800.0), Some("s")),
            // ---- 多端网关 ----
            // 默认只绑 127.0.0.1：手机要连必须由用户主动改成 0.0.0.0，
            // 默认就把助手暴露到局域网是危险设计。
            ("agent.gateway_enabled", "true", "boolean", "启用多端设备网关", None, None, None),
            ("agent.gateway_bind", "127.0.0.1", "string", "网关绑定地址（0.0.0.0 才允许局域网/手机访问）", None, None, None),
            ("agent.gateway_port", "4769", "number", "网关端口", Some(1024.0), Some(65535.0), None),
            ("agent.gateway_reply_timeout_sec", "180", "number", "远端消息等待回复超时", Some(30.0), Some(600.0), Some("s")),
            // ---- 界面 ----
            ("ui.theme", "dark", "string", "主题（dark/light）", None, None, None),
            // ---- 安全 ----
            ("security.confirm_medium_risk", "true", "boolean", "MEDIUM 风险是否弹窗确认", None, None, None),
            // ---- 托盘与启动 ----
            ("tray.minimize_on_close", "true", "boolean", "关闭窗口时最小化到托盘", None, None, None),
            ("startup.auto_launch", "false", "boolean", "开机自启", None, None, None),
            ("global_shortcut", "Ctrl+Shift+Space", "string", "全局快捷键（如 Ctrl+Shift+Space）", None, None, None),
            // ---- 浏览器自动化 ----
            ("browser.default", "auto", "string", "优先使用的浏览器（auto/chrome/edge/...）", None, None, None),
            ("browser.cdp_port", "9222", "number", "浏览器远程调试端口", Some(1024.0), Some(65535.0), None),
            // ---- 网页发布 ----
            ("publish.github_token", "", "string", "GitHub Personal Access Token", None, None, None),
            ("publish.github_repo", "", "string", "GitHub Pages 仓库名", None, None, None),
            ("publish.netlify_token", "", "string", "Netlify 备用发布通道令牌", None, None, None),
            ("publish.proxy", "", "string", "发布用代理地址（留空自动）", None, None, None),
        ];

        let mut stmt = self.conn.prepare(
            "INSERT OR IGNORE INTO settings (key, value, value_type, description, min_value, max_value, unit)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)"
        )?;

        for (key, value, value_type, description, min_value, max_value, unit) in defaults {
            stmt.execute(params![key, value, value_type, description, min_value, max_value, unit])?;
        }

        Ok(())
    }

    /// 清除历史遗留的"幽灵配置"：这些键曾经被写进 settings 表，
    /// 但源码里没有任何地方读取它们，改了对行为毫无影响。
    /// 把它们留在设置页只会误导用户以为自己掌握了一个开关。
    fn purge_phantom_settings(&self) -> AppResult<()> {
        // ui.language：应用只有中文界面，切语言无实现。
        // web_search.provider：模型侧无联网检索后端。
        // github.token_set：与 publish.github_token 语义重复，且是派生标志位，
        //                   不该作为独立配置项暴露给用户编辑。
        // monitor.*：告警逻辑并入主动助手规则系统后，这五项不再单独生效。
        //
        // 注意 publish.netlify_token **不在**此列——它是 Netlify 备用发布
        // 通道的凭据，lib.rs 的 publish_webpage 真的会读它。
        const PHANTOM_KEYS: [&str; 8] = [
            "ui.language",
            "web_search.provider",
            "github.token_set",
            "monitor.cpu_interval_ms",
            "monitor.process_interval_ms",
            "monitor.alert_cpu_threshold",
            "monitor.alert_memory_threshold",
            "monitor.alert_disk_threshold",
        ];

        let mut stmt = self.conn.prepare("DELETE FROM settings WHERE key = ?1")?;
        for key in PHANTOM_KEYS {
            stmt.execute(params![key])?;
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
    ///
    /// 对"元数据列尚不存在"的老库做了降级处理：如果 `min_value` / `max_value`
    /// / `unit` 还没被 `init_schema` 补上，就先用旧的三列查询、把三个元数据
    /// 字段留空，而不是直接抛 SQL 错误。
    ///
    /// 为什么需要这个兜底：产品路径里 `init_schema` 紧跟在建连之后，顺序上
    /// 没问题。但只要有一处代码在这之前读设置（诊断工具、临时的启动探针、
    /// 将来新增的某个初始化步骤），就会撞上一个"列不存在"的崩溃——
    /// 而崩溃信息 `no such column: min_value` 完全无法让人联想到"迁移没跑"。
    pub fn get_all_settings(&self) -> AppResult<Vec<SettingItem>> {
        let has_metadata = self.settings_has_metadata_columns();

        let sql = if has_metadata {
            "SELECT key, value, value_type, description, min_value, max_value, unit
             FROM settings ORDER BY key"
        } else {
            "SELECT key, value, value_type, description, NULL, NULL, NULL
             FROM settings ORDER BY key"
        };

        let mut stmt = self.conn.prepare(sql)?;
        let items = stmt.query_map([], |row| {
            Ok(SettingItem {
                key: row.get(0)?,
                value: row.get(1)?,
                value_type: row.get(2)?,
                description: row.get(3)?,
                min_value: row.get(4)?,
                max_value: row.get(5)?,
                unit: row.get(6)?,
            })
        })?;

        let mut collected = items.collect::<Result<Vec<_>, _>>()?;

        // 凭据类配置在离开数据库前就被替换成掩码，明文永不进入前端内存。
        // 前端拿到掩码后只用于判断"是否已配置"，写入仍走 update_setting。
        for item in collected.iter_mut() {
            if is_secret_setting(&item.key) {
                item.value = mask_secret(&item.value);
            }
        }

        Ok(collected)
    }

    /// settings 表是否已具备取值范围相关的三列
    fn settings_has_metadata_columns(&self) -> bool {
        let mut stmt = match self.conn.prepare("PRAGMA table_info(settings)") {
            Ok(s) => s,
            Err(_) => return false,
        };
        let names: Vec<String> = match stmt.query_map([], |row| row.get::<_, String>(1)) {
            Ok(rows) => rows.filter_map(|r| r.ok()).collect(),
            Err(_) => return false,
        };
        names.iter().any(|c| c == "min_value")
    }

    /// 读取单个配置项的取值范围约束
    ///
    /// 返回 (值类型, 下界, 上界, 单位)。键不存在时返回 None，由调用方
    /// 决定是放行还是拒绝——不在这里抛错，避免把"未知键"也做成致命错误。
    ///
    /// 老库缺元数据列时返回 None：那时也没有区间可查，与"键不存在"的处理
    /// 一致（放行），比让保存操作报 SQL 错误更合理。
    pub fn get_setting_bounds(
        &self,
        key: &str,
    ) -> AppResult<Option<(String, Option<f64>, Option<f64>, Option<String>)>> {
        if !self.settings_has_metadata_columns() {
            return Ok(None);
        }

        let mut stmt = self.conn.prepare(
            "SELECT value_type, min_value, max_value, unit FROM settings WHERE key = ?1"
        )?;
        let mut rows = stmt.query(params![key])?;
        if let Some(row) = rows.next()? {
            Ok(Some((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)))
        } else {
            Ok(None)
        }
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
    // 能力契约层：工具回执账本
    // ============================================================

    /// 写入一条工具回执。
    ///
    /// 失败不应影响工具本身的执行结果——回执是记账，不是业务。
    /// 但调用方必须把失败打到日志里，别让"记账失败"静默消失。
    pub fn insert_tool_receipt(&self, receipt: &crate::tools::receipt::ToolReceipt) -> AppResult<()> {
        self.conn.execute(
            "INSERT INTO tool_receipts (
                receipt_id, session_id, run_id, tool_name, capability_id,
                side_effect, scope, reversible, risk_level,
                verification_strategy, verification_status, missing_fields,
                idempotency_key, request_digest, result_digest,
                permission_result, success, error_code, duration_ms, verification_evidence
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20)",
            params![
                receipt.receipt_id,
                receipt.session_id,
                receipt.run_id,
                receipt.tool_name,
                receipt.capability_id,
                receipt.side_effect,
                receipt.scope,
                receipt.reversible as i64,
                receipt.risk_level,
                receipt.verification_strategy,
                receipt.verification_status,
                receipt.missing_fields,
                receipt.idempotency_key,
                receipt.request_digest,
                receipt.result_digest,
                receipt.permission_result,
                receipt.success as i64,
                receipt.error_code,
                receipt.duration_ms,
                receipt.verification_evidence,
            ],
        )?;
        Ok(())
    }

    /// 按时间倒序读取回执，可按工具名过滤。
    pub fn list_tool_receipts(
        &self,
        limit: i64,
        tool_name: Option<&str>,
    ) -> AppResult<Vec<crate::tools::receipt::ToolReceipt>> {
        let mut receipts = Vec::new();
        match tool_name {
            Some(name) => {
                let mut stmt = self.conn.prepare(
                    "SELECT receipt_id, session_id, run_id, tool_name, capability_id,
                            side_effect, scope, reversible, risk_level,
                            verification_strategy, verification_status, missing_fields,
                            idempotency_key, request_digest, result_digest,
                            permission_result, success, error_code, duration_ms, verification_evidence,
                            created_at
                     FROM tool_receipts WHERE tool_name = ?1
                     ORDER BY created_at DESC, rowid DESC LIMIT ?2",
                )?;
                let rows = stmt.query_map(params![name, limit], row_to_receipt)?;
                for row in rows {
                    receipts.push(row?);
                }
            }
            None => {
                let mut stmt = self.conn.prepare(
                    "SELECT receipt_id, session_id, run_id, tool_name, capability_id,
                            side_effect, scope, reversible, risk_level,
                            verification_strategy, verification_status, missing_fields,
                            idempotency_key, request_digest, result_digest,
                            permission_result, success, error_code, duration_ms, verification_evidence,
                            created_at
                     FROM tool_receipts
                     ORDER BY created_at DESC, rowid DESC LIMIT ?1",
                )?;
                let rows = stmt.query_map(params![limit], row_to_receipt)?;
                for row in rows {
                    receipts.push(row?);
                }
            }
        }
        Ok(receipts)
    }

    /// 按 `run_id` 读取同一次任务的全部回执，按发生顺序正序返回。
    ///
    /// 任务真值快照要靠这个函数把"一轮对话里的若干次工具调用"聚成一个整体。
    /// 这里用 `rowid` 排序而不是时间戳：同一毫秒内产生的两条回执，
    /// 时间戳无法定序，插入顺序才是它们真实的先后。
    pub fn list_tool_receipts_by_run(
        &self,
        run_id: &str,
        limit: i64,
    ) -> AppResult<Vec<crate::tools::receipt::ToolReceipt>> {
        let mut stmt = self.conn.prepare(
            "SELECT receipt_id, session_id, run_id, tool_name, capability_id,
                    side_effect, scope, reversible, risk_level,
                    verification_strategy, verification_status, missing_fields,
                    idempotency_key, request_digest, result_digest,
                    permission_result, success, error_code, duration_ms, verification_evidence,
                    created_at
             FROM tool_receipts WHERE run_id = ?1
             ORDER BY rowid ASC LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![run_id, limit], row_to_receipt)?;
        let mut receipts = Vec::new();
        for row in rows {
            receipts.push(row?);
        }
        Ok(receipts)
    }

    /// 查询同一条幂等键是否已有成功回执。
    ///
    /// 供后续"重放保护"使用：同一个会话重复提交同一份参数时，
    /// 可以先看这里，而不是再执行一次。
    pub fn find_successful_receipt_by_idempotency(
        &self,
        key: &str,
    ) -> AppResult<Option<crate::tools::receipt::ToolReceipt>> {
        let mut stmt = self.conn.prepare(
            "SELECT receipt_id, session_id, run_id, tool_name, capability_id,
                    side_effect, scope, reversible, risk_level,
                    verification_strategy, verification_status, missing_fields,
                    idempotency_key, request_digest, result_digest,
                    permission_result, success, error_code, duration_ms, verification_evidence,
                    created_at
             FROM tool_receipts
             WHERE idempotency_key = ?1 AND success = 1
             ORDER BY created_at DESC LIMIT 1",
        )?;
        let mut rows = stmt.query_map(params![key], row_to_receipt)?;
        match rows.next() {
            Some(row) => Ok(Some(row?)),
            None => Ok(None),
        }
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

/// 把一行 `tool_receipts` 映射成回执结构。
///
/// 列顺序必须与三条 SELECT 完全一致；新增列时三处一起改，
/// 否则会读错列而不是报错——这类错误只能靠这个函数集中兜住。
fn row_to_receipt(row: &rusqlite::Row<'_>) -> rusqlite::Result<crate::tools::receipt::ToolReceipt> {
    Ok(crate::tools::receipt::ToolReceipt {
        receipt_id: row.get(0)?,
        session_id: row.get(1)?,
        run_id: row.get(2)?,
        tool_name: row.get(3)?,
        capability_id: row.get(4)?,
        side_effect: row.get(5)?,
        scope: row.get(6)?,
        reversible: row.get::<_, i64>(7)? != 0,
        risk_level: row.get(8)?,
        verification_strategy: row.get(9)?,
        verification_status: row.get(10)?,
        missing_fields: row.get::<_, Option<String>>(11)?.unwrap_or_else(|| "[]".to_string()),
        idempotency_key: row.get(12)?,
        request_digest: row.get(13)?,
        result_digest: row.get(14)?,
        permission_result: row.get(15)?,
        success: row.get::<_, i64>(16)? != 0,
        error_code: row.get(17)?,
        duration_ms: row.get::<_, Option<i64>>(18)?.unwrap_or(0),
        verification_evidence: row.get(19)?,
        created_at: row.get::<_, Option<String>>(20)?.unwrap_or_default(),
    })
}


#[cfg(test)]
mod settings_tests {
    use super::*;
    use std::path::PathBuf;

    /// 建一个临时库并跑完整初始化，返回 (Database, 临时目录)
    fn fresh_db(tag: &str) -> (Database, PathBuf) {
        let dir = std::env::temp_dir().join(format!(
            "pcg-settings-test-{}-{}",
            tag,
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("创建临时目录");
        let db = Database::new(&dir.join("t.db")).expect("打开数据库");
        db.init_schema().expect("初始化 schema");
        (db, dir)
    }

    fn cleanup(dir: &PathBuf) {
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn seeds_only_settings_that_have_a_consumer() {
        let (db, dir) = fresh_db("seeds");
        let items = db.get_all_settings().expect("读取设置");

        // 已删除的"幽灵配置"不得再出现。它们曾经存在于库中，
        // 但没有任何代码读取，留在设置页只会误导用户。
        for phantom in [
            "ui.language",
            "web_search.provider",
            "github.token_set",
            "monitor.cpu_interval_ms",
            "monitor.process_interval_ms",
            "monitor.alert_cpu_threshold",
            "monitor.alert_memory_threshold",
            "monitor.alert_disk_threshold",
        ] {
            assert!(
                !items.iter().any(|s| s.key == phantom),
                "幽灵配置 {} 仍在设置表中",
                phantom
            );
        }

        // 新接入的配置项必须在。
        for expected in [
            "agent.num_ctx",
            "agent.num_predict",
            "agent.proactive_interval_sec",
            "agent.proactive_cooldown_sec",
            "browser.default",
            "browser.cdp_port",
            "startup.auto_launch",
            "ui.theme",
        ] {
            assert!(
                items.iter().any(|s| s.key == expected),
                "缺少配置项 {}",
                expected
            );
        }

        cleanup(&dir);
    }

    #[test]
    fn numeric_settings_carry_their_bounds() {
        let (db, dir) = fresh_db("bounds");

        // 有区间的项必须真的带上区间，否则前端无从做即时校验。
        let (vtype, lo, hi, _unit) = db
            .get_setting_bounds("agent.temperature")
            .expect("查询成功")
            .expect("temperature 存在");
        assert_eq!(vtype, "number");
        assert_eq!(lo, Some(0.0));
        assert_eq!(hi, Some(2.0));

        let (_, lo, hi, unit) = db
            .get_setting_bounds("ollama.timeout_ms")
            .expect("查询成功")
            .expect("timeout 存在");
        assert_eq!(lo, Some(5000.0));
        assert_eq!(hi, Some(300000.0));
        assert_eq!(unit.as_deref(), Some("ms"));

        // 字符串项没有区间，应当是 None 而不是 0——0 会被误当成合法下界。
        let (vtype, lo, hi, _) = db
            .get_setting_bounds("ollama.model")
            .expect("查询成功")
            .expect("model 存在");
        assert_eq!(vtype, "string");
        assert_eq!(lo, None);
        assert_eq!(hi, None);

        cleanup(&dir);
    }

    #[test]
    fn bounds_query_returns_none_for_unknown_key() {
        let (db, dir) = fresh_db("unknown");
        // 未知键不是错误，返回 None 让调用方自己决定放行还是拒绝。
        assert!(db
            .get_setting_bounds("no.such.key")
            .expect("查询不应报错")
            .is_none());
        cleanup(&dir);
    }

    #[test]
    fn initialization_is_idempotent() {
        let (db, dir) = fresh_db("idempotent");
        let before = db.get_all_settings().expect("首次读取").len();

        // 老库升级路径会反复调用这些方法，重复执行必须不报错、
        // 不重复插入、不覆盖用户已改的值。
        db.init_schema().expect("二次初始化不应失败");
        db.init_schema().expect("三次初始化不应失败");

        let after = db.get_all_settings().expect("再次读取").len();
        assert_eq!(before, after, "重复初始化不应改变配置项数量");

        cleanup(&dir);
    }

    #[test]
    fn refresh_keeps_the_users_value_but_updates_description() {
        let (db, dir) = fresh_db("refresh");

        // 模拟用户改过值，然后再次初始化（相当于升级后重启）。
        db.update_setting("ollama.timeout_ms", "120000")
            .expect("写入用户值");
        db.init_schema().expect("再次初始化");

        assert_eq!(
            db.get_setting("ollama.timeout_ms")
                .expect("读取")
                .as_deref(),
            Some("120000"),
            "用户的取值不应被默认值覆盖"
        );

        // 说明文本应来自最新定义（不是老库里的旧文案）。
        let desc = db
            .get_all_settings()
            .expect("读取")
            .into_iter()
            .find(|s| s.key == "ollama.timeout_ms")
            .and_then(|s| s.description)
            .unwrap_or_default();
        assert!(
            desc.contains("超时"),
            "说明应被刷新为最新文案，实际为: {}",
            desc
        );

        cleanup(&dir);
    }

    #[test]
    fn purge_removes_phantoms_from_an_old_database() {
        let (db, dir) = fresh_db("purge");

        // 模拟老库：手动塞入一个已废弃的配置项。
        db.connection()
            .execute(
                "INSERT OR REPLACE INTO settings (key, value, value_type, description)
                 VALUES ('ui.language', 'en-US', 'string', '旧的语言设置')",
                [],
            )
            .expect("塞入幽灵项");

        assert!(db
            .get_setting("ui.language")
            .expect("读取")
            .is_some());

        db.init_schema().expect("再次初始化应清理幽灵项");

        assert!(
            db.get_setting("ui.language").expect("读取").is_none(),
            "老库里的幽灵项应被清理"
        );

        cleanup(&dir);
    }

    #[test]
    fn get_all_settings_never_exposes_a_raw_token() {
        let (db, dir) = fresh_db("secret-mask");

        // 这是被修复的真实缺陷：凭据原本以明文随设置列表下发到前端，
        // 虽然输入框用 type=password 做了视觉掩码，但值就躺在 DOM 的
        // value 属性里，DevTools 一行 querySelector 即可读出。
        //
        // 这里必须是构造出来的假令牌：测试只需一个"长得像凭据"的字符串，
        // 绝不能用真实凭据——那等于把密钥写进仓库历史。
        let token = "ghp_FAKE_TEST_TOKEN_0123456789abcdefghij";
        db.update_setting("publish.github_token", token)
            .expect("写入令牌");

        let items = db.get_all_settings().expect("读取设置");
        let entry = items
            .iter()
            .find(|s| s.key == "publish.github_token")
            .expect("令牌项存在");

        assert!(
            !entry.value.contains("ghp_"),
            "令牌前缀不得出现在下发值中，实际为: {}",
            entry.value
        );
        assert!(
            !entry.value.contains("FAKE_TEST_TOKEN_0123456789abcdefghij"),
            "令牌主体不得出现在下发值中"
        );
        assert!(
            entry.value.contains("••••"),
            "应以掩码形式下发，实际为: {}",
            entry.value
        );

        // 关键：脱敏只作用于下发通道，库里必须仍存原文，
        // 否则发布会因为拿到掩码而静默失败。
        assert_eq!(
            db.get_setting("publish.github_token")
                .expect("读取")
                .as_deref(),
            Some(token),
            "数据库中的原文不能被掩码污染"
        );

        cleanup(&dir);
    }

    #[test]
    fn mask_keeps_just_enough_to_identify_the_credential() {
        // 长值：保留末 4 位，够用户辨认是哪一个 token。
        assert_eq!(mask_secret("ghp_abcdefghijklmnop1234"), "••••••••1234");
        // 空值：保持空，前端据此判断"未配置"。
        assert_eq!(mask_secret(""), "");
        assert_eq!(mask_secret("   "), "");
        // 短值：连末 4 位都不透出，整体打码。
        assert_eq!(mask_secret("abc123"), "••••••••");
    }

    #[test]
    fn secret_detection_covers_the_obvious_shapes() {
        assert!(is_secret_setting("publish.github_token"));
        assert!(is_secret_setting("publish.netlify_token"));
        assert!(is_secret_setting("foo.api_key"));
        assert!(is_secret_setting("bar.password"));
        assert!(is_secret_setting("baz_secret"));

        // 下面这些不是凭据：仓库名是公开信息，base_url 是内网地址，
        // 把它们的值也打码会让设置页变得没法用。
        assert!(!is_secret_setting("publish.github_repo"));
        assert!(!is_secret_setting("ollama.base_url"));
        assert!(!is_secret_setting("ollama.model"));
        assert!(!is_secret_setting("ui.theme"));
    }
}

#[cfg(test)]
mod receipt_tests {
    use super::*;
    use crate::tools::receipt::ToolReceipt;
    use std::path::PathBuf;

    fn fresh_db(tag: &str) -> (Database, PathBuf) {
        let dir = std::env::temp_dir().join(format!(
            "pcg-receipt-test-{}-{}",
            tag,
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("创建临时目录");
        let db = Database::new(&dir.join("t.db")).expect("打开数据库");
        db.init_schema().expect("初始化 schema");
        (db, dir)
    }

    fn cleanup(dir: &PathBuf) {
        let _ = std::fs::remove_dir_all(dir);
    }

    fn receipt(id: &str, run: Option<&str>, tool: &str, status: &str, evidence: Option<&str>) -> ToolReceipt {
        ToolReceipt {
            receipt_id: id.to_string(),
            session_id: "sess-1".to_string(),
            run_id: run.map(str::to_owned),
            tool_name: tool.to_string(),
            capability_id: tool.to_string(),
            side_effect: "local_write".to_string(),
            scope: "test".to_string(),
            reversible: true,
            risk_level: "LOW".to_string(),
            verification_strategy: "artifact".to_string(),
            verification_status: status.to_string(),
            verification_evidence: evidence.map(str::to_owned),
            missing_fields: "[]".to_string(),
            idempotency_key: None,
            request_digest: "req".to_string(),
            result_digest: Some("res".to_string()),
            permission_result: "ALLOWED".to_string(),
            success: true,
            error_code: None,
            duration_ms: 12,
            created_at: "2026-10-19 12:00:00".to_string(),
        }
    }

    /// 按 run_id 取回执时既不能串轮次，也不能把列读错位。
    ///
    /// 后者是这个测试真正的价值：`row_to_receipt` 依赖 SELECT 的列顺序，
    /// 少一列、多一列都不会报错，只会把 `verification_evidence` 读成
    /// 别的字段——那种错误只能靠断言具体字段抓出来。
    #[test]
    fn receipts_are_scoped_by_run_and_read_back_intact() {
        let (db, dir) = fresh_db("by-run");
        db.insert_tool_receipt(&receipt("r1", Some("run-a"), "write_file", "VERIFIED", Some("回读一致")))
            .expect("写入 r1");
        db.insert_tool_receipt(&receipt("r2", Some("run-b"), "write_file", "FAILED", None))
            .expect("写入 r2");
        db.insert_tool_receipt(&receipt("r3", Some("run-a"), "start_process", "CONTRADICTED", Some("进程未在运行")))
            .expect("写入 r3");

        let rows = db.list_tool_receipts_by_run("run-a", 100).expect("按 run 查询");
        assert_eq!(rows.len(), 2, "只应取回 run-a 的两条回执");
        assert_eq!(rows[0].receipt_id, "r1");
        assert_eq!(rows[1].receipt_id, "r3", "必须按插入顺序，而不是倒序");
        assert_eq!(rows[0].verification_evidence.as_deref(), Some("回读一致"));
        assert_eq!(rows[1].verification_status, "CONTRADICTED");
        assert_eq!(rows[1].duration_ms, 12);
        assert!(rows[0].success);

        cleanup(&dir);
    }

    #[test]
    fn unknown_run_returns_nothing() {
        let (db, dir) = fresh_db("missing-run");
        db.insert_tool_receipt(&receipt("r1", Some("run-a"), "write_file", "VERIFIED", None))
            .expect("写入 r1");
        assert!(db.list_tool_receipts_by_run("run-zzz", 100).expect("查询").is_empty());
        cleanup(&dir);
    }
}
