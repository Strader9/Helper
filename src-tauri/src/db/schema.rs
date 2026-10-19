//! 数据库 Schema 定义
//!
//! 所有表结构和索引定义。
//! 设计原则：写少读多、预建索引、数据最小化。
//!
//! 共 18 张表：
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
//! 12. agent_tasks       - Agent 任务状态
//! 13. trusted_tools     - 已信任工具
//! 14. agent_observations - Agent 观察记录
//! 15. memory_items      - 长期记忆
//! 16. proactive_rules   - 主动助手规则
//! 17. skills            - 已安装技能
//! 18. tool_receipts     - 工具回执账本（事实层）

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
-- min_value / max_value：数值型配置的合法区间，供前端做即时校验、
-- 供后端在保存前拒绝越界输入。NULL 表示该配置项没有区间约束。
-- unit：数值单位（%、ms、s、GB 等），仅用于界面展示，避免用户在
-- "30000" 面前猜这是秒还是毫秒。
CREATE TABLE IF NOT EXISTS settings (
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL,
    value_type TEXT NOT NULL DEFAULT 'string',
    description TEXT,
    min_value REAL,
    max_value REAL,
    unit TEXT,
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
    is_archived INTEGER NOT NULL DEFAULT 0,
    -- 远期对话的压缩摘要（由 agent::history 生成并缓存）。
    -- summary_upto_id 记录摘要覆盖到哪一条消息，避免重复压缩同一段。
    summary TEXT,
    summary_upto_id INTEGER
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
    source TEXT NOT NULL DEFAULT 'auto',
    tier TEXT NOT NULL DEFAULT 'episodic',
    consolidated_into TEXT
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

-- ============================================================
-- 18. tool_receipts - 工具回执账本（能力契约层新增）
-- ============================================================
-- 与 audit_logs 的分工：audit_logs 记"谁被允许做了什么"（安全审计），
-- tool_receipts 记"这次调用实际改变了什么、拿什么证据判定它完成了"（事实）。
-- 模型说"已经做完"不算事实，verification_status = 'VERIFIED' 才算。
CREATE TABLE IF NOT EXISTS tool_receipts (
    receipt_id TEXT PRIMARY KEY,
    session_id TEXT NOT NULL,
    run_id TEXT,
    tool_name TEXT NOT NULL,
    capability_id TEXT NOT NULL,
    side_effect TEXT NOT NULL,
    scope TEXT NOT NULL,
    reversible INTEGER NOT NULL DEFAULT 1,
    risk_level TEXT NOT NULL,
    verification_strategy TEXT NOT NULL,
    verification_status TEXT NOT NULL,
    verification_evidence TEXT,
    missing_fields TEXT,
    idempotency_key TEXT,
    request_digest TEXT NOT NULL,
    result_digest TEXT,
    permission_result TEXT NOT NULL,
    success INTEGER NOT NULL,
    error_code TEXT,
    duration_ms INTEGER,
    created_at TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE INDEX IF NOT EXISTS idx_receipts_session ON tool_receipts(session_id, created_at DESC);
CREATE INDEX IF NOT EXISTS idx_receipts_tool ON tool_receipts(tool_name);
CREATE INDEX IF NOT EXISTS idx_receipts_status ON tool_receipts(verification_status);
CREATE INDEX IF NOT EXISTS idx_receipts_idempotency ON tool_receipts(idempotency_key);
CREATE INDEX IF NOT EXISTS idx_receipts_created ON tool_receipts(created_at DESC);
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

/// 常驻调度迁移 SQL：定时作业 + 每次运行的记录。
///
/// 与 `proactive_rules` 的分工：
/// - `proactive_rules` 回答"系统指标越线了要不要提醒我"，动作是通知/开机/开程序；
/// - `scheduled_jobs` 回答"每天早上九点帮我把这件事做完"，动作是一次完整的
///   Agent 循环（规划 → 调工具 → 给结论）。后者需要目标、次数上限、运行历史，
///   前者只需要触发条件。
///
/// 时间一律存**本地时间**字符串（`%Y-%m-%d %H:%M:%S`），因为定时是给人看的：
/// 用户说"每天九点"指的就是本地九点。UTC 会在跨时区/夏令时上骗人。
pub const MIGRATION_SCHEDULER_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS scheduled_jobs (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    kind TEXT NOT NULL,
    schedule TEXT NOT NULL,
    goal TEXT NOT NULL,
    enabled INTEGER NOT NULL DEFAULT 1,
    next_run_at TEXT,
    last_run_at TEXT,
    run_count INTEGER NOT NULL DEFAULT 0,
    max_runs INTEGER,
    last_status TEXT,
    last_summary TEXT,
    last_error TEXT,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE INDEX IF NOT EXISTS idx_jobs_due ON scheduled_jobs(enabled, next_run_at);
CREATE INDEX IF NOT EXISTS idx_jobs_created ON scheduled_jobs(created_at DESC);

CREATE TABLE IF NOT EXISTS scheduled_job_runs (
    id TEXT PRIMARY KEY,
    job_id TEXT NOT NULL,
    session_id TEXT,
    started_at TEXT NOT NULL DEFAULT (datetime('now')),
    finished_at TEXT,
    status TEXT NOT NULL DEFAULT 'running',
    summary TEXT,
    error TEXT,
    steps INTEGER NOT NULL DEFAULT 0,
    tools_used TEXT,
    FOREIGN KEY (job_id) REFERENCES scheduled_jobs(id) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS idx_job_runs_job ON scheduled_job_runs(job_id, started_at DESC);
CREATE INDEX IF NOT EXISTS idx_job_runs_status ON scheduled_job_runs(status);
"#;

/// 多端（设备）迁移 SQL：已配对设备 + 一次性配对码。
///
/// 只存令牌的 SHA-256，不存明文：数据库文件在用户目录下，能读到库的人不该
/// 因此拿到一台已配对设备的访问权。`token_hint` 存末 4 位，仅用于让人
/// 分辨"这条设备记录对应我手机上的哪个令牌"。
pub const MIGRATION_DEVICES_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS devices (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    platform TEXT,
    token_hash TEXT NOT NULL,
    token_hint TEXT,
    paired_at TEXT NOT NULL DEFAULT (datetime('now')),
    last_seen_at TEXT,
    revoked INTEGER NOT NULL DEFAULT 0
);

CREATE INDEX IF NOT EXISTS idx_devices_active ON devices(revoked);

CREATE TABLE IF NOT EXISTS device_pairing_codes (
    code TEXT PRIMARY KEY,
    created_at TEXT NOT NULL,
    expires_at TEXT NOT NULL,
    used INTEGER NOT NULL DEFAULT 0
);
"#;

/// 本地知识库迁移 SQL
///
/// 与记忆表的区别：`memory_items` 存"助手记住的事实"，会被注入 System Prompt；
/// 这两张表存"用户文档的原文分块"，只在检索时按需取出——
/// 原文进上下文会挤掉对话历史，所以必须分层。
pub const MIGRATION_KNOWLEDGE_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS knowledge_documents (
    id TEXT PRIMARY KEY,
    path TEXT NOT NULL UNIQUE,
    title TEXT NOT NULL,
    ext TEXT,
    bytes INTEGER NOT NULL DEFAULT 0,
    sha256 TEXT NOT NULL,
    chunk_count INTEGER NOT NULL DEFAULT 0,
    indexed_at TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE INDEX IF NOT EXISTS idx_kdocs_indexed ON knowledge_documents(indexed_at DESC);

CREATE TABLE IF NOT EXISTS knowledge_chunks (
    id TEXT PRIMARY KEY,
    document_id TEXT NOT NULL,
    ordinal INTEGER NOT NULL,
    content TEXT NOT NULL,
    chars INTEGER NOT NULL DEFAULT 0
);

CREATE INDEX IF NOT EXISTS idx_kchunks_doc ON knowledge_chunks(document_id, ordinal);
"#;

/// 工作流引擎迁移 SQL
///
/// 与 `agent_tasks` / `scheduled_jobs` 的分工：
/// - `agent_tasks`：一次对话里"我要做这几件事"的临时台账，随对话推进；
/// - `scheduled_jobs`：到点跑**一次完整 Agent 循环**，目标由模型自己拆；
/// - 这里：**步骤与依赖写死的固定流程**，每步调哪个工具、谁依赖谁都是确定的。
///   适合"每天备份某目录 → 压缩 → 上传 → 发通知"这类不需要临场判断的活。
///
/// `steps_json` 存的是 DAG（每步含 id / kind / tool / arguments / depends_on）。
/// 结构一旦被引用就不该原地改语义，所以更新时按"整体替换 + runs 保留旧快照"
/// 处理：历史运行记录里的步骤结果不随定义变更而失真。
pub const MIGRATION_WORKFLOW_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS workflow_defs (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL UNIQUE,
    description TEXT NOT NULL DEFAULT '',
    steps_json TEXT NOT NULL DEFAULT '[]',
    variables_json TEXT NOT NULL DEFAULT '{}',
    enabled INTEGER NOT NULL DEFAULT 1,
    run_count INTEGER NOT NULL DEFAULT 0,
    last_run_at TEXT,
    last_status TEXT,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE INDEX IF NOT EXISTS idx_wf_defs_updated ON workflow_defs(updated_at DESC);

CREATE TABLE IF NOT EXISTS workflow_runs (
    id TEXT PRIMARY KEY,
    workflow_id TEXT NOT NULL,
    workflow_name TEXT NOT NULL DEFAULT '',
    session_id TEXT,
    trigger_kind TEXT NOT NULL DEFAULT 'manual',
    status TEXT NOT NULL DEFAULT 'running',
    started_at TEXT NOT NULL DEFAULT (datetime('now')),
    finished_at TEXT,
    step_count INTEGER NOT NULL DEFAULT 0,
    ok_count INTEGER NOT NULL DEFAULT 0,
    fail_count INTEGER NOT NULL DEFAULT 0,
    summary TEXT,
    error TEXT,
    FOREIGN KEY (workflow_id) REFERENCES workflow_defs(id) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS idx_wf_runs_flow ON workflow_runs(workflow_id, started_at DESC);

CREATE TABLE IF NOT EXISTS workflow_steps (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    run_id TEXT NOT NULL,
    step_id TEXT NOT NULL,
    name TEXT NOT NULL DEFAULT '',
    kind TEXT NOT NULL DEFAULT 'tool',
    seq INTEGER NOT NULL DEFAULT 0,
    status TEXT NOT NULL DEFAULT 'pending',
    started_at TEXT,
    finished_at TEXT,
    output_json TEXT,
    error TEXT,
    FOREIGN KEY (run_id) REFERENCES workflow_runs(id) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS idx_wf_steps_run ON workflow_steps(run_id, seq);
"#;

/// 工作接管（work_takeover）迁移 SQL
///
/// 与 `agent_tasks` / `workflow_defs` 的分工：
/// - `agent_tasks`：一次对话里"我要做这几件事"的临时台账，随对话推进；
/// - `workflow_defs`：步骤与依赖写死的固定流程，每次跑顺序一样；
/// - 这里：**接管用户的一件具体工作**（客户、店铺、账号、案件、交付），
///   把"当前动作 / 草稿 / 确认边界 / 阻塞 / 风险 / 产物"持久化下来，随时断点续做。
///   和 workflow 的关键区别是它**带着确认边界**（allowedNow / confirmationRequired /
///   blockedBy），明确"哪些现在能做、哪些要等用户点头、哪些被卡住"。
///
/// `next_actions_json` 等数组字段统一存 JSON 字符串，避免为"要不要多一张表"
/// 纠结——这些数组是任务的附属状态，不是要单独查询的实体。
pub const MIGRATION_WORK_TAKEOVER_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS work_takeover_tasks (
    id TEXT PRIMARY KEY,
    title TEXT NOT NULL DEFAULT '',
    category TEXT NOT NULL DEFAULT 'general_work',
    source TEXT NOT NULL DEFAULT 'manual',
    source_message TEXT NOT NULL DEFAULT '',
    contact TEXT NOT NULL DEFAULT '',
    summary TEXT NOT NULL DEFAULT '',
    urgency TEXT NOT NULL DEFAULT 'normal',
    priority INTEGER NOT NULL DEFAULT 50,
    recommended_workflow TEXT NOT NULL DEFAULT '',
    next_actions_json TEXT NOT NULL DEFAULT '[]',
    current_action_index INTEGER NOT NULL DEFAULT 0,
    draft_reply TEXT NOT NULL DEFAULT '',
    artifacts_json TEXT NOT NULL DEFAULT '[]',
    allowed_now_json TEXT NOT NULL DEFAULT '[]',
    confirmation_required_json TEXT NOT NULL DEFAULT '[]',
    blocked_by_json TEXT NOT NULL DEFAULT '[]',
    risks_json TEXT NOT NULL DEFAULT '[]',
    result TEXT NOT NULL DEFAULT '',
    note TEXT NOT NULL DEFAULT '',
    status TEXT NOT NULL DEFAULT 'queued',
    metadata_json TEXT NOT NULL DEFAULT '{}',
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE INDEX IF NOT EXISTS idx_wt_tasks_status ON work_takeover_tasks(status, priority DESC);

CREATE TABLE IF NOT EXISTS work_takeover_evidence (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    task_id TEXT NOT NULL,
    workflow_id TEXT NOT NULL,
    step_id TEXT NOT NULL DEFAULT '',
    status TEXT NOT NULL DEFAULT 'active',
    evidence_json TEXT NOT NULL DEFAULT '{}',
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE INDEX IF NOT EXISTS idx_wt_evidence_task ON work_takeover_evidence(task_id);
"#;
