//! 数据库 Schema 定义
//!
//! 所有表结构和索引定义。
//! 设计原则：写少读多、预建索引、数据最小化。
//!
//! 共 11 张表：
//! 1. migrations         - 迁移版本记录
//! 2. settings           - 配置项
//! 3. audit_logs         - 审计日志
//! 4. trusted_apps       - 可信应用白名单
//! 5. allowed_directories - 允许目录白名单
//! 6. system_events      - 系统事件
//! 7. chat_sessions      - 对话会话
//! 8. chat_messages      - 对话消息
//! 9. automation_rules   - 自动化规则
//! 10. github_snapshots  - GitHub 趋势快照元数据
//! 11. notification_logs - 通知日志

/// 完整初始化 SQL
///
/// Phase 1 一次性创建所有表，避免后续多次迁移。
/// 表设计基于 database.md v1.0。
pub const SCHEMA_SQL: &str = r#"
-- ============================================================
-- 1. migrations - 迁移版本表
-- ============================================================
CREATE TABLE IF NOT EXISTS migrations (
    version INTEGER PRIMARY KEY,
    name TEXT NOT NULL,
    applied_at TEXT NOT NULL DEFAULT (datetime('now')),
    checksum TEXT NOT NULL
);

-- ============================================================
-- 2. settings - 配置表
-- ============================================================
CREATE TABLE IF NOT EXISTS settings (
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL,
    value_type TEXT NOT NULL DEFAULT 'string',
    description TEXT,
    updated_at TEXT NOT NULL DEFAULT (datetime('now'))
);

-- ============================================================
-- 3. audit_logs - 审计日志表
-- ============================================================
CREATE TABLE IF NOT EXISTS audit_logs (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    timestamp TEXT NOT NULL DEFAULT (datetime('now')),
    session_id TEXT NOT NULL,
    user_input_hash TEXT,
    tool_name TEXT NOT NULL,
    tool_risk_level TEXT NOT NULL,
    arguments TEXT,
    permission_result TEXT NOT NULL,
    confirmation_source TEXT,
    success INTEGER NOT NULL,
    error_code TEXT,
    error_message TEXT,
    duration_ms INTEGER,
    result_summary TEXT,
    created_at TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE INDEX IF NOT EXISTS idx_audit_timestamp ON audit_logs(timestamp DESC);
CREATE INDEX IF NOT EXISTS idx_audit_tool_name ON audit_logs(tool_name);
CREATE INDEX IF NOT EXISTS idx_audit_risk_level ON audit_logs(tool_risk_level);
CREATE INDEX IF NOT EXISTS idx_audit_session_id ON audit_logs(session_id);
CREATE INDEX IF NOT EXISTS idx_audit_permission ON audit_logs(permission_result);

-- ============================================================
-- 4. trusted_apps - 可信应用白名单
-- ============================================================
CREATE TABLE IF NOT EXISTS trusted_apps (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    app_name TEXT NOT NULL UNIQUE,
    display_name TEXT,
    executable_path TEXT,
    reason TEXT,
    added_at TEXT NOT NULL DEFAULT (datetime('now')),
    added_by TEXT NOT NULL DEFAULT 'user'
);

CREATE UNIQUE INDEX IF NOT EXISTS idx_trusted_apps_name ON trusted_apps(app_name);

-- ============================================================
-- 5. allowed_directories - 允许目录白名单
-- ============================================================
CREATE TABLE IF NOT EXISTS allowed_directories (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    path TEXT NOT NULL UNIQUE,
    label TEXT,
    access_level TEXT NOT NULL DEFAULT 'read',
    is_default INTEGER NOT NULL DEFAULT 0,
    added_at TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE UNIQUE INDEX IF NOT EXISTS idx_allowed_dirs_path ON allowed_directories(path);

-- ============================================================
-- 6. system_events - 系统事件表
-- ============================================================
CREATE TABLE IF NOT EXISTS system_events (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    event_type TEXT NOT NULL,
    severity TEXT NOT NULL,
    payload TEXT,
    timestamp TEXT NOT NULL DEFAULT (datetime('now')),
    acknowledged INTEGER NOT NULL DEFAULT 0
);

CREATE INDEX IF NOT EXISTS idx_system_events_timestamp ON system_events(timestamp DESC);
CREATE INDEX IF NOT EXISTS idx_system_events_type ON system_events(event_type);
CREATE INDEX IF NOT EXISTS idx_system_events_severity ON system_events(severity);
CREATE INDEX IF NOT EXISTS idx_system_events_acked ON system_events(acknowledged);

-- ============================================================
-- 7. chat_sessions - 对话会话表
-- ============================================================
CREATE TABLE IF NOT EXISTS chat_sessions (
    id TEXT PRIMARY KEY,
    title TEXT NOT NULL DEFAULT '新对话',
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at TEXT NOT NULL DEFAULT (datetime('now')),
    message_count INTEGER NOT NULL DEFAULT 0,
    last_message TEXT,
    is_pinned INTEGER NOT NULL DEFAULT 0,
    is_archived INTEGER NOT NULL DEFAULT 0
);

CREATE INDEX IF NOT EXISTS idx_chat_sessions_updated ON chat_sessions(updated_at DESC);
CREATE INDEX IF NOT EXISTS idx_chat_sessions_pinned ON chat_sessions(is_pinned DESC, updated_at DESC);
CREATE INDEX IF NOT EXISTS idx_chat_sessions_archived ON chat_sessions(is_archived);

-- ============================================================
-- 8. chat_messages - 对话消息表
-- ============================================================
CREATE TABLE IF NOT EXISTS chat_messages (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    session_id TEXT NOT NULL,
    role TEXT NOT NULL,
    content TEXT NOT NULL,
    tool_calls TEXT,
    tool_call_id TEXT,
    timestamp TEXT NOT NULL DEFAULT (datetime('now')),
    tokens_used INTEGER,
    FOREIGN KEY (session_id) REFERENCES chat_sessions(id) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS idx_chat_messages_session ON chat_messages(session_id, timestamp ASC);
CREATE INDEX IF NOT EXISTS idx_chat_messages_role ON chat_messages(role);
CREATE INDEX IF NOT EXISTS idx_chat_messages_timestamp ON chat_messages(timestamp);

-- ============================================================
-- 9. automation_rules - 自动化规则表
-- ============================================================
CREATE TABLE IF NOT EXISTS automation_rules (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    name TEXT NOT NULL,
    description TEXT,
    rule_type TEXT NOT NULL,
    trigger_config TEXT NOT NULL,
    action_type TEXT NOT NULL,
    action_config TEXT NOT NULL,
    enabled INTEGER NOT NULL DEFAULT 1,
    last_run_at TEXT,
    next_run_at TEXT,
    run_count INTEGER NOT NULL DEFAULT 0,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE INDEX IF NOT EXISTS idx_automation_enabled ON automation_rules(enabled);
CREATE INDEX IF NOT EXISTS idx_automation_type ON automation_rules(rule_type);
CREATE INDEX IF NOT EXISTS idx_automation_next_run ON automation_rules(next_run_at);

-- ============================================================
-- 10. github_snapshots - GitHub 趋势快照元数据表
-- ============================================================
CREATE TABLE IF NOT EXISTS github_snapshots (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    snapshot_date TEXT NOT NULL,
    range TEXT NOT NULL,
    language TEXT,
    file_path TEXT NOT NULL,
    repo_count INTEGER NOT NULL,
    generated_at TEXT NOT NULL DEFAULT (datetime('now')),
    generation_ms INTEGER
);

CREATE UNIQUE INDEX IF NOT EXISTS idx_github_snapshots_date ON github_snapshots(snapshot_date, range, language);
CREATE INDEX IF NOT EXISTS idx_github_snapshots_generated ON github_snapshots(generated_at DESC);

-- ============================================================
-- 11. notification_logs - 通知日志表
-- ============================================================
CREATE TABLE IF NOT EXISTS notification_logs (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    title TEXT NOT NULL,
    message TEXT,
    notification_type TEXT NOT NULL,
    is_read INTEGER NOT NULL DEFAULT 0,
    created_at TEXT NOT NULL DEFAULT (datetime('now'))
);

-- ============================================================
-- 12. agent_tasks - Agent 任务状态表
-- ============================================================
CREATE TABLE IF NOT EXISTS agent_tasks (
    id TEXT PRIMARY KEY,
    session_id TEXT NOT NULL,
    goal TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'PENDING',
    plan_json TEXT NOT NULL DEFAULT '[]',
    current_step INTEGER NOT NULL DEFAULT 0,
    observations_json TEXT NOT NULL DEFAULT '[]',
    errors_json TEXT NOT NULL DEFAULT '[]',
    completed_steps_json TEXT NOT NULL DEFAULT '[]',
    is_simple INTEGER NOT NULL DEFAULT 0,
    summary TEXT,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE INDEX IF NOT EXISTS idx_agent_tasks_session ON agent_tasks(session_id, created_at DESC);
CREATE INDEX IF NOT EXISTS idx_agent_tasks_status ON agent_tasks(status);

-- ============================================================
-- 13. trusted_tools - 已信任工具白名单（Phase 4 安全层新增）
-- ============================================================
CREATE TABLE IF NOT EXISTS trusted_tools (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    tool_name TEXT NOT NULL,
    path_pattern TEXT,
    risk_level TEXT NOT NULL DEFAULT 'ALL',
    added_at TEXT NOT NULL DEFAULT (datetime('now')),
    added_by TEXT NOT NULL DEFAULT 'user',
    UNIQUE(tool_name, path_pattern)
);

CREATE INDEX IF NOT EXISTS idx_trusted_tools_name ON trusted_tools(tool_name);

-- ============================================================
-- 14. agent_observations - Agent 观察记录表（V13 新增）
-- ============================================================
CREATE TABLE IF NOT EXISTS agent_observations (
    id TEXT PRIMARY KEY,
    task_id TEXT NOT NULL,
    step_id TEXT,
    source TEXT NOT NULL,
    tool_name TEXT,
    success INTEGER NOT NULL DEFAULT 1,
    data TEXT,
    summary TEXT NOT NULL,
    timestamp INTEGER NOT NULL,
    created_at TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE INDEX IF NOT EXISTS idx_agent_obs_task ON agent_observations(task_id, timestamp DESC);
CREATE INDEX IF NOT EXISTS idx_agent_obs_tool ON agent_observations(tool_name);
CREATE INDEX IF NOT EXISTS idx_agent_obs_source ON agent_observations(source);
CREATE INDEX IF NOT EXISTS idx_agent_obs_success ON agent_observations(success);

-- ============================================================
-- 15. memory_items - 长期记忆表（V17 新增）
-- ============================================================
CREATE TABLE IF NOT EXISTS memory_items (
    id TEXT PRIMARY KEY,
    memory_type TEXT NOT NULL,
    content TEXT NOT NULL,
    keywords TEXT,
    importance INTEGER NOT NULL DEFAULT 50,
    access_count INTEGER NOT NULL DEFAULT 0,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    last_accessed_at TEXT NOT NULL DEFAULT (datetime('now')),
    expires_at TEXT,
    source TEXT NOT NULL DEFAULT 'auto'
);

CREATE INDEX IF NOT EXISTS idx_memory_type ON memory_items(memory_type);
CREATE INDEX IF NOT EXISTS idx_memory_importance ON memory_items(importance DESC);
CREATE INDEX IF NOT EXISTS idx_memory_created ON memory_items(created_at DESC);
CREATE INDEX IF NOT EXISTS idx_memory_source ON memory_items(source);

-- ============================================================
-- 16. proactive_rules - 主动助手规则表（V18 新增）
-- ============================================================
CREATE TABLE IF NOT EXISTS proactive_rules (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    trigger_type TEXT NOT NULL,
    trigger_config TEXT,
    action_type TEXT NOT NULL,
    action_config TEXT,
    enabled INTEGER NOT NULL DEFAULT 1,
    last_triggered TEXT,
    trigger_count INTEGER NOT NULL DEFAULT 0,
    created_at TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE INDEX IF NOT EXISTS idx_proactive_enabled ON proactive_rules(enabled);
CREATE INDEX IF NOT EXISTS idx_proactive_trigger_type ON proactive_rules(trigger_type);
CREATE INDEX IF NOT EXISTS idx_proactive_action_type ON proactive_rules(action_type);

-- ============================================================
-- 17. skills - 已安装技能表（V20 新增）
-- ============================================================
CREATE TABLE IF NOT EXISTS skills (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    description TEXT,
    version TEXT NOT NULL DEFAULT '1.0.0',
    author TEXT,
    skill_type TEXT NOT NULL DEFAULT 'local',
    entry_point TEXT,
    permissions TEXT,
    enabled INTEGER NOT NULL DEFAULT 1,
    installed_at TEXT NOT NULL DEFAULT (datetime('now')),
    skill_dir TEXT
);

CREATE INDEX IF NOT EXISTS idx_skills_type ON skills(skill_type);
CREATE INDEX IF NOT EXISTS idx_skills_enabled ON skills(enabled);
"#;

/// Phase 4 安全层数据库迁移 SQL
///
/// 添加 trusted_tools 表（兼容已存在的数据库）。
pub const MIGRATION_PHASE4_SQL: &str = r#"
-- 添加 trusted_tools 表（如果不存在）
CREATE TABLE IF NOT EXISTS trusted_tools (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    tool_name TEXT NOT NULL,
    path_pattern TEXT,
    risk_level TEXT NOT NULL DEFAULT 'ALL',
    added_at TEXT NOT NULL DEFAULT (datetime('now')),
    added_by TEXT NOT NULL DEFAULT 'user',
    UNIQUE(tool_name, path_pattern)
);

CREATE INDEX IF NOT EXISTS idx_trusted_tools_name ON trusted_tools(tool_name);

-- 更新 allowed_directories 的 access_level（如果已有数据）
UPDATE allowed_directories SET access_level = 'TRUSTED' WHERE is_default = 0;
"#;
