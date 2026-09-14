//! V18 Proactive Engine — 主动执行系统
//!
//! 定期监控系统状态，当满足预设规则时主动触发通知或操作。
//! 设计原则：
//! - 低风险操作（通知）可自动执行
//! - 高风险操作需用户确认（通过事件推送到前端）
//! - 所有自动执行的操作有审计记录
//! - 规则触发有冷却时间，避免刷屏

use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter, Manager};
use tauri_plugin_notification::NotificationExt;
use tokio::sync::Mutex;

use chrono::{DateTime, Datelike, Local};
use crate::monitoring::{get_system_metrics, SystemMetrics};
use crate::tools::executor::ToolCallRequest;

// ============================================================
// 数据模型
// ============================================================

/// 触发器类型
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum TriggerType {
    /// 系统指标阈值（CPU/内存/磁盘）
    SystemMetric,
    /// 定时触发（每天/每周特定时间）
    Schedule,
    /// 事件触发（如进程启动、窗口变化）
    Event,
    /// 进程检测（新进程/陌生进程）
    Process,
}

impl TriggerType {
    pub fn as_str(&self) -> &'static str {
        match self {
            TriggerType::SystemMetric => "system_metric",
            TriggerType::Schedule => "schedule",
            TriggerType::Event => "event",
            TriggerType::Process => "process",
        }
    }

    pub fn from_str(s: &str) -> Self {
        match s {
            "system_metric" => TriggerType::SystemMetric,
            "schedule" => TriggerType::Schedule,
            "event" => TriggerType::Event,
            "process" => TriggerType::Process,
            _ => TriggerType::SystemMetric,
        }
    }
}

/// 动作类型
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum ActionType {
    /// 弹出通知
    Notify,
    /// 执行工具（经过完整 Tool → Policy → Permission → Executor 流程）
    ExecuteTool,
    /// 打开应用
    OpenApp,
}

impl ActionType {
    pub fn as_str(&self) -> &'static str {
        match self {
            ActionType::Notify => "notify",
            ActionType::ExecuteTool => "execute_tool",
            ActionType::OpenApp => "open_app",
        }
    }

    pub fn from_str(s: &str) -> Self {
        match s {
            "notify" => ActionType::Notify,
            "execute_tool" => ActionType::ExecuteTool,
            "open_app" => ActionType::OpenApp,
            _ => ActionType::Notify,
        }
    }
}

/// 主动规则（与数据库 proactive_rules 表对应）
#[derive(Debug, Clone, Serialize)]
pub struct ProactiveRule {
    pub id: String,
    pub name: String,
    pub trigger_type: String,
    pub trigger_config: String,
    pub action_type: String,
    pub action_config: String,
    pub enabled: bool,
    pub last_triggered: Option<String>,
    pub trigger_count: i64,
    pub created_at: String,
}

/// 系统指标触发器配置（JSON 结构）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SystemMetricTrigger {
    /// 监控指标: cpu / memory / disk
    pub metric: String,
    /// 阈值（百分比或 GB）
    pub threshold: f64,
    /// 比较操作: gt / lt / gte / lte
    pub operator: String,
    /// 持续时间（秒），0 表示立即触发
    #[serde(default)]
    pub duration_sec: u64,
    /// 磁盘盘符（仅 disk 指标时使用）
    #[serde(default)]
    pub drive: Option<String>,
}

/// 通知动作配置（JSON 结构）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NotifyAction {
    pub title: String,
    pub message: String,
    /// 通知级别: info / warning / critical
    #[serde(default = "default_level")]
    pub level: String,
}

fn default_level() -> String {
    "info".to_string()
}

/// Proactive Engine 运行状态
#[derive(Debug, Clone, Serialize)]
pub struct ProactiveStatus {
    pub running: bool,
    pub rules_total: usize,
    pub rules_enabled: usize,
    pub last_check: Option<String>,
    pub checks_count: u64,
    pub triggers_count: u64,
}

// ============================================================
// 数据库操作
// ============================================================

/// 获取所有规则
pub fn get_rules(conn: &rusqlite::Connection) -> Result<Vec<ProactiveRule>, String> {
    let mut stmt = conn
        .prepare(
            "SELECT id, name, trigger_type, trigger_config, action_type, action_config, \
             enabled, last_triggered, trigger_count, created_at \
             FROM proactive_rules ORDER BY created_at DESC",
        )
        .map_err(|e| e.to_string())?;

    let rules = stmt
        .query_map([], |row| {
            Ok(ProactiveRule {
                id: row.get(0)?,
                name: row.get(1)?,
                trigger_type: row.get(2)?,
                trigger_config: row.get(3)?,
                action_type: row.get(4)?,
                action_config: row.get(5)?,
                enabled: row.get::<_, i64>(6)? != 0,
                last_triggered: row.get(7)?,
                trigger_count: row.get(8)?,
                created_at: row.get(9)?,
            })
        })
        .map_err(|e| e.to_string())?;

    rules.collect::<Result<Vec<_>, _>>().map_err(|e| e.to_string())
}

/// 根据 ID 获取规则
pub fn get_rule_by_id(conn: &rusqlite::Connection, id: &str) -> Result<Option<ProactiveRule>, String> {
    let mut stmt = conn
        .prepare(
            "SELECT id, name, trigger_type, trigger_config, action_type, action_config, \
             enabled, last_triggered, trigger_count, created_at \
             FROM proactive_rules WHERE id = ?1",
        )
        .map_err(|e| e.to_string())?;

    let mut rows = stmt.query([id]).map_err(|e| e.to_string())?;
    if let Some(row) = rows.next().map_err(|e| e.to_string())? {
        Ok(Some(ProactiveRule {
            id: row.get(0).map_err(|e| e.to_string())?,
            name: row.get(1).map_err(|e| e.to_string())?,
            trigger_type: row.get(2).map_err(|e| e.to_string())?,
            trigger_config: row.get(3).map_err(|e| e.to_string())?,
            action_type: row.get(4).map_err(|e| e.to_string())?,
            action_config: row.get(5).map_err(|e| e.to_string())?,
            enabled: row.get::<_, i64>(6).map_err(|e| e.to_string())? != 0,
            last_triggered: row.get(7).map_err(|e| e.to_string())?,
            trigger_count: row.get(8).map_err(|e| e.to_string())?,
            created_at: row.get(9).map_err(|e| e.to_string())?,
        }))
    } else {
        Ok(None)
    }
}

/// 插入新规则
pub fn insert_rule(conn: &rusqlite::Connection, rule: &ProactiveRule) -> Result<(), String> {
    conn.execute(
        "INSERT INTO proactive_rules \
         (id, name, trigger_type, trigger_config, action_type, action_config, \
          enabled, last_triggered, trigger_count, created_at) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
        rusqlite::params![
            rule.id,
            rule.name,
            rule.trigger_type,
            rule.trigger_config,
            rule.action_type,
            rule.action_config,
            rule.enabled as i64,
            rule.last_triggered,
            rule.trigger_count,
            rule.created_at,
        ],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

/// 更新规则
pub fn update_rule(conn: &rusqlite::Connection, rule: &ProactiveRule) -> Result<(), String> {
    conn.execute(
        "UPDATE proactive_rules SET \
         name = ?2, trigger_type = ?3, trigger_config = ?4, \
         action_type = ?5, action_config = ?6, enabled = ?7 \
         WHERE id = ?1",
        rusqlite::params![
            rule.id,
            rule.name,
            rule.trigger_type,
            rule.trigger_config,
            rule.action_type,
            rule.action_config,
            rule.enabled as i64,
        ],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

/// 删除规则
pub fn delete_rule(conn: &rusqlite::Connection, id: &str) -> Result<(), String> {
    conn.execute("DELETE FROM proactive_rules WHERE id = ?1", [id])
        .map_err(|e| e.to_string())?;
    Ok(())
}

/// 启用/禁用规则
pub fn toggle_rule(conn: &rusqlite::Connection, id: &str, enabled: bool) -> Result<(), String> {
    conn.execute(
        "UPDATE proactive_rules SET enabled = ?2 WHERE id = ?1",
        rusqlite::params![id, enabled as i64],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

/// 更新规则触发统计（last_triggered + trigger_count）
pub fn record_trigger(conn: &rusqlite::Connection, id: &str) -> Result<(), String> {
    conn.execute(
        "UPDATE proactive_rules SET last_triggered = datetime('now'), trigger_count = trigger_count + 1 WHERE id = ?1",
        [id],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

/// 检查规则是否在冷却期内（返回 true 表示可以触发）
fn is_offline_cooldown_ok(last_triggered: &Option<String>, cooldown_sec: i64) -> bool {
    let last = match last_triggered {
        Some(t) => t.clone(),
        None => return true,
    };
    // 简单比较：SQLite datetime 格式，用当前时间减
    // 用 chrono 解析
    if let Ok(last_dt) = chrono::NaiveDateTime::parse_from_str(&last, "%Y-%m-%d %H:%M:%S") {
        let now = chrono::Utc::now().naive_utc();
        let diff = (now - last_dt).num_seconds();
        diff >= cooldown_sec
    } else {
        true
    }
}

// ============================================================
// 预设规则
// ============================================================

/// 确保预设规则存在（首次启动时插入）
pub fn ensure_preset_rules(conn: &rusqlite::Connection) -> Result<usize, String> {
    let presets = build_preset_rules();
    let mut inserted = 0;

    for rule in presets {
        match get_rule_by_id(conn, &rule.id)? {
            Some(_) => {} // 已存在，跳过
            None => {
                insert_rule(conn, &rule)?;
                inserted += 1;
            }
        }
    }
    Ok(inserted)
}

/// 构建预设规则列表
fn build_preset_rules() -> Vec<ProactiveRule> {
    let now = chrono::Utc::now().format("%Y-%m-%d %H:%M:%S").to_string();

    vec![
        // 1. 高内存提醒
        ProactiveRule {
            id: "preset_high_memory".to_string(),
            name: "高内存提醒".to_string(),
            trigger_type: TriggerType::SystemMetric.as_str().to_string(),
            trigger_config: serde_json::to_string(&SystemMetricTrigger {
                metric: "memory".to_string(),
                threshold: 90.0,
                operator: "gt".to_string(),
                duration_sec: 30,
                drive: None,
            })
            .unwrap(),
            action_type: ActionType::Notify.as_str().to_string(),
            action_config: serde_json::to_string(&NotifyAction {
                title: "⚠️ 内存占用过高".to_string(),
                message: "系统内存使用率已超过 90%，建议关闭不必要的应用程序。".to_string(),
                level: "warning".to_string(),
            })
            .unwrap(),
            enabled: true,
            last_triggered: None,
            trigger_count: 0,
            created_at: now.clone(),
        },
        // 2. CPU 过热提醒
        ProactiveRule {
            id: "preset_high_cpu".to_string(),
            name: "CPU 高负载提醒".to_string(),
            trigger_type: TriggerType::SystemMetric.as_str().to_string(),
            trigger_config: serde_json::to_string(&SystemMetricTrigger {
                metric: "cpu".to_string(),
                threshold: 95.0,
                operator: "gt".to_string(),
                duration_sec: 30,
                drive: None,
            })
            .unwrap(),
            action_type: ActionType::Notify.as_str().to_string(),
            action_config: serde_json::to_string(&NotifyAction {
                title: "🔥 CPU 负载过高".to_string(),
                message: "CPU 使用率已持续超过 95%，电脑可能出现卡顿，建议检查占用高的进程。".to_string(),
                level: "warning".to_string(),
            })
            .unwrap(),
            enabled: true,
            last_triggered: None,
            trigger_count: 0,
            created_at: now.clone(),
        },
        // 3. 磁盘空间不足（C盘）
        ProactiveRule {
            id: "preset_low_disk_c".to_string(),
            name: "C盘空间不足".to_string(),
            trigger_type: TriggerType::SystemMetric.as_str().to_string(),
            trigger_config: serde_json::to_string(&SystemMetricTrigger {
                metric: "disk".to_string(),
                threshold: 10.0, // 可用空间 < 10GB
                operator: "lt".to_string(),
                duration_sec: 0,
                drive: Some("C:".to_string()),
            })
            .unwrap(),
            action_type: ActionType::Notify.as_str().to_string(),
            action_config: serde_json::to_string(&NotifyAction {
                title: "💾 C盘空间不足".to_string(),
                message: "C盘可用空间已不足 10GB，建议清理临时文件或卸载不常用软件。".to_string(),
                level: "critical".to_string(),
            })
            .unwrap(),
            enabled: true,
            last_triggered: None,
            trigger_count: 0,
            created_at: now.clone(),
        },
        // 4. 新进程检测（默认关闭）
        ProactiveRule {
            id: "preset_new_process".to_string(),
            name: "新进程检测".to_string(),
            trigger_type: TriggerType::Process.as_str().to_string(),
            trigger_config: serde_json::json!({
                "mode": "unknown",
                "whitelist": []
            })
            .to_string(),
            action_type: ActionType::Notify.as_str().to_string(),
            action_config: serde_json::to_string(&NotifyAction {
                title: "🔍 检测到新进程".to_string(),
                message: "检测到未在白名单中的新进程启动，请确认是否为预期操作。".to_string(),
                level: "info".to_string(),
            })
            .unwrap(),
            enabled: false, // 默认关闭
            last_triggered: None,
            trigger_count: 0,
            created_at: now.clone(),
        },
        // 5. V21: 下班提醒（schedule 触发器）
        ProactiveRule {
            id: "preset_off_work_reminder".to_string(),
            name: "下班提醒".to_string(),
            trigger_type: TriggerType::Schedule.as_str().to_string(),
            trigger_config: serde_json::to_string(&ScheduleTrigger {
                time: "18:00".to_string(),
                repeat: "weekdays".to_string(),
            })
            .unwrap(),
            action_type: ActionType::Notify.as_str().to_string(),
            action_config: serde_json::to_string(&NotifyAction {
                title: "⏰ 下班时间到".to_string(),
                message: "已经18:00了，记得保存工作、关闭电脑，准备下班吧！".to_string(),
                level: "info".to_string(),
            })
            .unwrap(),
            enabled: false, // 默认关闭，用户可自行启用
            last_triggered: None,
            trigger_count: 0,
            created_at: now,
        },
    ]
}

// ============================================================
// 规则评估
// ============================================================

/// 评估系统指标规则是否满足触发条件
///
/// 返回 (是否触发, 当前值描述)
fn evaluate_system_metric_rule(
    trigger: &SystemMetricTrigger,
    metrics: &SystemMetrics,
) -> (bool, String) {
    let (current_value, value_desc) = match trigger.metric.as_str() {
        "cpu" => (metrics.cpu_usage as f64, format!("CPU {:.1}%", metrics.cpu_usage)),
        "memory" => (
            metrics.memory_usage_percent as f64,
            format!("内存 {:.1}% ({:.1}GB / {:.1}GB)", metrics.memory_usage_percent, metrics.memory_used_gb, metrics.memory_total_gb),
        ),
        "disk" => {
            let drive = trigger.drive.as_deref().unwrap_or("C:");
            if let Some(disk) = metrics.disks.iter().find(|d| d.drive == drive) {
                let free_gb = disk.total_gb - disk.used_gb;
                (free_gb as f64, format!("{} 可用 {:.1}GB", drive, free_gb))
            } else {
                return (false, format!("未找到磁盘 {}", drive));
            }
        }
        _ => return (false, format!("未知指标: {}", trigger.metric)),
    };

    let triggered = match trigger.operator.as_str() {
        "gt" => current_value > trigger.threshold,
        "lt" => current_value < trigger.threshold,
        "gte" => current_value >= trigger.threshold,
        "lte" => current_value <= trigger.threshold,
        _ => false,
    };

    (triggered, value_desc)
}

// ============================================================
// Proactive Engine
// ============================================================

// V21: Schedule 触发器配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScheduleTrigger {
    /// 触发时间，格式 "HH:MM"（24小时制）
    pub time: String,
    /// 重复模式: daily / weekdays / once
    #[serde(default = "default_daily")]
    pub repeat: String,
}

fn default_daily() -> String {
    "daily".to_string()
}

// V21: Process 触发器配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProcessTrigger {
    /// 检测模式: new（新进程）/ specific（特定进程）/ unknown（未知进程）
    #[serde(default = "default_new")]
    pub mode: String,
    /// 白名单进程名列表（mode=unknown 时使用）
    #[serde(default)]
    pub whitelist: Vec<String>,
    /// 目标进程名（mode=specific 时使用）
    #[serde(default)]
    pub target: Option<String>,
}

fn default_new() -> String {
    "new".to_string()
}

// V21: Event 触发器配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventTrigger {
    /// 事件类型: app_start / user_active / disk_insert
    pub event: String,
}

// V21: ExecuteTool 动作配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecuteToolAction {
    /// 工具名称
    pub tool_name: String,
    /// 工具参数
    pub arguments: serde_json::Value,
}

// V21: OpenApp 动作配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenAppAction {
    /// 应用名称或路径
    pub app_name: String,
}

/// V21: 获取当前进程名称列表
#[cfg(windows)]
fn get_process_names() -> Vec<String> {
    use std::process::Command;
    use std::os::windows::process::CommandExt;
    let output = Command::new("tasklist")
        .args(["/FO", "CSV", "/NH"])
        .creation_flags(0x08000000)
        .output();
    match output {
        Ok(out) => {
            let text = String::from_utf8_lossy(&out.stdout);
            text.lines()
                .filter_map(|line| {
                    // CSV 格式: "name.exe","pid","...","..."
                    line.split('"').nth(1).map(|s| s.to_lowercase())
                })
                .collect()
        }
        Err(_) => Vec::new(),
    }
}

#[cfg(not(windows))]
fn get_process_names() -> Vec<String> { Vec::new() }

/// V21: 评估 schedule 规则是否满足触发条件
fn evaluate_schedule_rule(rule: &ProactiveRule) -> bool {
    let trigger: ScheduleTrigger = match serde_json::from_str(&rule.trigger_config) {
        Ok(t) => t,
        Err(_) => return false,
    };

    let now = Local::now();
    let current_time = now.format("%H:%M").to_string();

    // 检查时间是否匹配（允许 ±2 分钟误差，因为每15秒检查一次）
    if current_time != trigger.time {
        return false;
    }

    // 检查重复模式
    match trigger.repeat.as_str() {
        "daily" => true,
        "weekdays" => {
            let weekday = now.weekday();
            !matches!(weekday, chrono::Weekday::Sat | chrono::Weekday::Sun)
        }
        "once" => {
            // 只触发一次，通过 last_triggered 控制
            rule.last_triggered.is_none()
        }
        _ => true,
    }
}

/// V21: 检测新进程
///
/// 返回 Some(新进程列表) 表示检测到符合条件的新进程，None 表示没有
fn detect_new_processes(
    prev: &[String],
    current: &[String],
    rule: &ProactiveRule,
) -> Option<Vec<String>> {
    let trigger: ProcessTrigger = match serde_json::from_str(&rule.trigger_config) {
        Ok(t) => t,
        Err(_) => return None,
    };

    let prev_set: std::collections::HashSet<&str> = prev.iter().map(|s| s.as_str()).collect();
    let new_procs: Vec<String> = current
        .iter()
        .filter(|p| !prev_set.contains(p.as_str()))
        .cloned()
        .collect();

    if new_procs.is_empty() {
        return None;
    }

    match trigger.mode.as_str() {
        "new" => Some(new_procs),
        "specific" => {
            let target = trigger.target.as_deref()?.to_lowercase();
            let matched: Vec<String> = new_procs
                .into_iter()
                .filter(|p| p.contains(&target))
                .collect();
            if matched.is_empty() { None } else { Some(matched) }
        }
        "unknown" => {
            let whitelist: std::collections::HashSet<String> = trigger
                .whitelist
                .iter()
                .map(|s| s.to_lowercase())
                .collect();
            let unknown: Vec<String> = new_procs
                .into_iter()
                .filter(|p| !whitelist.contains(p))
                .collect();
            if unknown.is_empty() { None } else { Some(unknown) }
        }
        _ => Some(new_procs),
    }
}

/// V21: 触发 event 类型的规则
async fn trigger_event_rules(
    app_handle: &AppHandle,
    event_type: &str,
    triggers_count: &std::sync::atomic::AtomicU64,
) {
    let rules = {
        let state = app_handle.state::<crate::AppState>();
        let db = state.db.lock().await;
        match get_rules(db.connection()) {
            Ok(r) => r,
            Err(_) => return,
        }
    };

    for rule in rules.iter().filter(|r| r.enabled) {
        if TriggerType::from_str(&rule.trigger_type) != TriggerType::Event {
            continue;
        }
        let trigger: EventTrigger = match serde_json::from_str(&rule.trigger_config) {
            Ok(t) => t,
            Err(_) => continue,
        };
        if trigger.event != event_type {
            continue;
        }
        let context = format!("事件触发: {}", event_type);
        execute_action(app_handle, rule, &context, triggers_count).await;
        {
            let state = app_handle.state::<crate::AppState>();
            let db = state.db.lock().await;
            let _ = record_trigger(db.connection(), &rule.id);
        }
    }
}

/// Proactive Engine — 主动监控引擎
///
/// 后台定期检查系统状态，评估规则并触发动作。
/// V21: 支持 schedule/event/process 触发器 + execute_tool/open_app 动作
pub struct ProactiveEngine {
    app_handle: AppHandle,
    running: Arc<AtomicBool>,
    checks_count: Arc<std::sync::atomic::AtomicU64>,
    triggers_count: Arc<std::sync::atomic::AtomicU64>,
    last_check: Arc<Mutex<Option<String>>>,
    /// 持续超阈值的规则状态：rule_id -> (首次超阈值时间, 当前是否仍超阈值)
    sustained_state: Arc<Mutex<std::collections::HashMap<String, Instant>>>,
    /// V21: 上次进程快照（用于 process 触发器检测进程变化）
    last_processes: Arc<Mutex<Option<Vec<String>>>>,
    /// V21: 引擎启动时间（用于 event 触发器的应用启动事件）
    started_at: Arc<Mutex<Option<DateTime<Local>>>>,
}

impl ProactiveEngine {
    /// 创建新的 Proactive Engine
    pub fn new(app_handle: AppHandle) -> Self {
        Self {
            app_handle,
            running: Arc::new(AtomicBool::new(false)),
            checks_count: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            triggers_count: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            last_check: Arc::new(Mutex::new(None)),
            sustained_state: Arc::new(Mutex::new(std::collections::HashMap::new())),
            last_processes: Arc::new(Mutex::new(None)),
            started_at: Arc::new(Mutex::new(None)),
        }
    }

    /// 启动后台监控循环
    pub fn start(&self) {
        if self.running.load(Ordering::Relaxed) {
            return;
        }
        self.running.store(true, Ordering::Relaxed);

        let app_handle = self.app_handle.clone();
        let running = self.running.clone();
        let checks_count = self.checks_count.clone();
        let triggers_count = self.triggers_count.clone();
        let last_check = self.last_check.clone();
        let sustained_state = self.sustained_state.clone();
        let last_processes = self.last_processes.clone();
        let started_at = self.started_at.clone();

        tauri::async_runtime::spawn(async move {
            eprintln!("[Proactive] Engine started, monitoring every 15s");
            let check_interval = Duration::from_secs(15);
            let cooldown_sec = 300; // 5 分钟冷却

            // V21: 记录启动时间，触发应用启动事件
            {
                let mut started = started_at.lock().await;
                *started = Some(Local::now());
            }

            // V21: 首次启动时触发 app_start event 规则
            trigger_event_rules(&app_handle, "app_start", &triggers_count).await;

            while running.load(Ordering::Relaxed) {
                tokio::time::sleep(check_interval).await;

                // 获取系统指标
                let metrics = get_system_metrics();
                checks_count.fetch_add(1, Ordering::Relaxed);
                *last_check.lock().await = Some(
                    chrono::Utc::now().format("%Y-%m-%d %H:%M:%S").to_string(),
                );

                // V21: 获取当前进程列表（用于 process 触发器）
                let current_processes = get_process_names();

                // 获取规则列表（从 AppState 的 db）
                let rules = {
                    let state = app_handle.state::<crate::AppState>();
                    let db = state.db.lock().await;
                    match get_rules(db.connection()) {
                        Ok(r) => r,
                        Err(e) => {
                            eprintln!("[Proactive] 获取规则失败: {}", e);
                            continue;
                        }
                    }
                };

                // 评估每条启用的规则
                for rule in rules.iter().filter(|r| r.enabled) {
                    let trigger_type = TriggerType::from_str(&rule.trigger_type);

                    match trigger_type {
                        TriggerType::SystemMetric => {
                            let trigger: SystemMetricTrigger =
                                match serde_json::from_str(&rule.trigger_config) {
                                    Ok(t) => t,
                                    Err(_) => continue,
                                };

                            let (currently_over, value_desc) =
                                evaluate_system_metric_rule(&trigger, &metrics);

                            // 处理持续时间要求
                            let should_trigger = if trigger.duration_sec > 0 {
                                let mut state_map = sustained_state.lock().await;
                                if currently_over {
                                    let entry = state_map
                                        .entry(rule.id.clone())
                                        .or_insert_with(Instant::now);
                                    entry.elapsed().as_secs() >= trigger.duration_sec
                                } else {
                                    state_map.remove(&rule.id);
                                    false
                                }
                            } else {
                                currently_over
                            };

                            if should_trigger {
                                // 检查冷却
                                if !is_offline_cooldown_ok(&rule.last_triggered, cooldown_sec) {
                                    continue;
                                }

                                // 执行动作
                                execute_action(
                                    &app_handle,
                                    rule,
                                    &value_desc,
                                    &triggers_count,
                                )
                                .await;

                                // 记录触发
                                {
                                    let state = app_handle.state::<crate::AppState>();
                                    let db = state.db.lock().await;
                                    let _ = record_trigger(db.connection(), &rule.id);
                                }

                                // 清除持续状态
                                if trigger.duration_sec > 0 {
                                    sustained_state.lock().await.remove(&rule.id);
                                }
                            }
                        }
                        // V21: Schedule 触发器（定时触发）
                        TriggerType::Schedule => {
                            if evaluate_schedule_rule(rule) {
                                if !is_offline_cooldown_ok(&rule.last_triggered, cooldown_sec) {
                                    continue;
                                }
                                let now = Local::now().format("%H:%M").to_string();
                                execute_action(&app_handle, rule, &format!("定时触发 {}", now), &triggers_count).await;
                                {
                                    let state = app_handle.state::<crate::AppState>();
                                    let db = state.db.lock().await;
                                    let _ = record_trigger(db.connection(), &rule.id);
                                }
                            }
                        }
                        // V21: Process 触发器（进程变化检测）
                        TriggerType::Process => {
                            if let Some(ref prev) = *last_processes.lock().await {
                                if let Some(new_procs) = detect_new_processes(prev, &current_processes, rule) {
                                    if !is_offline_cooldown_ok(&rule.last_triggered, cooldown_sec) {
                                        continue;
                                    }
                                    let context = format!("新进程: {}", new_procs.join(", "));
                                    execute_action(&app_handle, rule, &context, &triggers_count).await;
                                    {
                                        let state = app_handle.state::<crate::AppState>();
                                        let db = state.db.lock().await;
                                        let _ = record_trigger(db.connection(), &rule.id);
                                    }
                                }
                            }
                        }
                        // V21: Event 触发器（应用启动、用户活跃等）
                        TriggerType::Event => {
                            // event 触发器由事件驱动，不在轮询循环中处理
                            // app_start 在启动时触发，user_active 可由前端事件触发
                        }
                    }
                }

                // V21: 更新进程快照
                *last_processes.lock().await = Some(current_processes);
            }
            eprintln!("[Proactive] Engine stopped");
        });
    }

    /// 停止引擎
    pub fn stop(&self) {
        self.running.store(false, Ordering::Relaxed);
    }

    /// 获取运行状态（基础信息，规则统计由 Tauri 命令补充）
    pub fn get_status(&self) -> ProactiveStatus {
        let running = self.running.load(Ordering::Relaxed);
        let checks = self.checks_count.load(Ordering::Relaxed);
        let triggers = self.triggers_count.load(Ordering::Relaxed);

        ProactiveStatus {
            running,
            rules_total: 0,      // 由命令层从数据库补充
            rules_enabled: 0,    // 由命令层从数据库补充
            last_check: None,    // 由命令层补充
            checks_count: checks,
            triggers_count: triggers,
        }
    }
}

// ============================================================
// 动作执行
// ============================================================

/// 执行规则配置的动作
/// V21: 显示系统通知（同时发送 Tauri 事件供前端展示）
fn emit_proactive_notification(app_handle: &AppHandle, payload: &serde_json::Value, title: &str, message: &str) {
    // 1. 发送 Tauri 事件（前端 Settings 页面监听并展示）
    let _ = app_handle.emit("proactive:notification", payload.to_string());

    // 2. 显示系统通知（Windows 通知中心 / 托盘气泡）
    if let Err(e) = app_handle
        .notification()
        .builder()
        .title(title)
        .body(message)
        .show()
    {
        eprintln!("[Notification] 显示系统通知失败: {}", e);
    }
}

async fn execute_action(
    app_handle: &AppHandle,
    rule: &ProactiveRule,
    context: &str,
    triggers_count: &std::sync::atomic::AtomicU64,
) {
    let action_type = ActionType::from_str(&rule.action_type);

    match action_type {
        ActionType::Notify => {
            let action: NotifyAction = match serde_json::from_str(&rule.action_config) {
                Ok(a) => a,
                Err(e) => {
                    eprintln!("[Proactive] 解析通知动作失败: {}", e);
                    return;
                }
            };

            // 发送 Tauri 事件到前端
            let payload = serde_json::json!({
                "rule_id": rule.id,
                "rule_name": rule.name,
                "title": action.title,
                "message": action.message,
                "level": action.level,
                "context": context,
                "timestamp": chrono::Utc::now().to_rfc3339(),
            });

            // 发送通知（Tauri 事件 + 托盘气泡）
            emit_proactive_notification(app_handle, &payload, &action.title, &action.message);
            triggers_count.fetch_add(1, Ordering::Relaxed);
            eprintln!(
                "[Proactive] 触发规则 '{}': {} ({})",
                rule.name, action.title, context
            );

            // 写入审计日志
            write_audit_log(app_handle, rule, &action.title, context).await;
        }
        ActionType::ExecuteTool => {
            // V21: 自动执行工具（带权限控制）
            let action: ExecuteToolAction = match serde_json::from_str(&rule.action_config) {
                Ok(a) => a,
                Err(e) => {
                    eprintln!("[Proactive] 解析工具执行动作失败: {}", e);
                    return;
                }
            };

            // 从 AppState 获取工具注册中心和执行器
            let state = match app_handle.try_state::<crate::AppState>() {
                Some(s) => s,
                None => return,
            };

            let tool = match state.tool_registry.get(&action.tool_name) {
                Some(t) => t,
                None => {
                    eprintln!("[Proactive] 工具未注册: {}", action.tool_name);
                    return;
                }
            };

            let risk = tool.risk_level();
            eprintln!(
                "[Proactive] 规则 '{}' 请求执行工具 {} (风险: {:?})",
                rule.name, action.tool_name, risk
            );

            // 权限控制：SAFE/LOW 自动执行，MEDIUM/HIGH 降级为通知
            match risk {
                crate::tools::RiskLevel::Safe | crate::tools::RiskLevel::Low => {
                    // 自动执行
                    let request = ToolCallRequest {
                        tool_name: action.tool_name.clone(),
                        arguments: action.arguments.clone(),
                        session_id: Some("proactive_engine".to_string()),
                    };
                    let result = state.tool_executor.execute(request).await;
                    let (success, summary) = match result {
                        Ok(r) => (r.success, r.to_tool_message_content()),
                        Err(e) => (false, e.to_string()),
                    };

                    // 发送通知（Tauri 事件 + 托盘气泡）
                    let notif_title = format!("🔧 自动执行: {}", action.tool_name);
                    let notif_message = if success {
                        format!("执行成功: {}", summary)
                    } else {
                        format!("执行失败: {}", summary)
                    };
                    let payload = serde_json::json!({
                        "rule_id": rule.id,
                        "rule_name": rule.name,
                        "title": notif_title,
                        "message": notif_message,
                        "level": if success { "info" } else { "warning" },
                        "context": context,
                        "auto_approved": true,
                        "timestamp": chrono::Utc::now().to_rfc3339(),
                    });
                    emit_proactive_notification(app_handle, &payload, &notif_title, &notif_message);
                    triggers_count.fetch_add(1, Ordering::Relaxed);

                    // 写入审计日志（auto_approved）
                    write_proactive_audit_log(
                        app_handle, rule, &action.tool_name, &risk,
                        &action.arguments, true, success, &summary,
                    ).await;
                }
                _ => {
                    // MEDIUM/HIGH 风险：降级为通知，不自动执行
                    let notif_title = format!("⚠️ 需要确认: {}", action.tool_name);
                    let notif_message = format!(
                        "该规则请求执行 {}（风险等级: {:?}），出于安全考虑需手动确认。",
                        action.tool_name, risk
                    );
                    let payload = serde_json::json!({
                        "rule_id": rule.id,
                        "rule_name": rule.name,
                        "title": notif_title,
                        "message": notif_message,
                        "level": "warning",
                        "context": context,
                        "auto_approved": false,
                        "timestamp": chrono::Utc::now().to_rfc3339(),
                    });
                    emit_proactive_notification(app_handle, &payload, &notif_title, &notif_message);
                    triggers_count.fetch_add(1, Ordering::Relaxed);
                }
            }
        }
        ActionType::OpenApp => {
            // V21: 自动打开应用
            let action: OpenAppAction = match serde_json::from_str(&rule.action_config) {
                Ok(a) => a,
                Err(e) => {
                    eprintln!("[Proactive] 解析打开应用动作失败: {}", e);
                    return;
                }
            };

            // 调用 launch_program 工具
            let state = match app_handle.try_state::<crate::AppState>() {
                Some(s) => s,
                None => return,
            };

            let args = serde_json::json!({ "name_or_path": action.app_name });
            let request = ToolCallRequest {
                tool_name: "launch_program".to_string(),
                arguments: args.clone(),
                session_id: Some("proactive_engine".to_string()),
            };
            let result = state.tool_executor.execute(request).await;
            let (success, summary) = match result {
                Ok(r) => (r.success, r.to_tool_message_content()),
                Err(e) => (false, e.to_string()),
            };

            let notif_title = format!("🚀 自动启动: {}", action.app_name);
            let notif_message = if success {
                format!("已启动 {}", action.app_name)
            } else {
                format!("启动失败: {}", summary)
            };
            let payload = serde_json::json!({
                "rule_id": rule.id,
                "rule_name": rule.name,
                "title": notif_title,
                "message": notif_message,
                "level": if success { "info" } else { "warning" },
                "context": context,
                "auto_approved": true,
                "timestamp": chrono::Utc::now().to_rfc3339(),
            });
            emit_proactive_notification(app_handle, &payload, &notif_title, &notif_message);
            triggers_count.fetch_add(1, Ordering::Relaxed);

            write_proactive_audit_log(
                app_handle, rule, "launch_program", &crate::tools::RiskLevel::Low,
                &args, true, success, &summary,
            ).await;
        }
    }
}

/// V21: 写入 Proactive 自动执行的审计日志
async fn write_proactive_audit_log(
    app_handle: &AppHandle,
    rule: &ProactiveRule,
    tool_name: &str,
    risk: &crate::tools::RiskLevel,
    arguments: &serde_json::Value,
    auto_approved: bool,
    success: bool,
    summary: &str,
) {
    let state = match app_handle.try_state::<crate::AppState>() {
        Some(s) => s,
        None => return,
    };

    let db = state.db.lock().await;
    let result = db.connection().execute(
        "INSERT INTO audit_logs \
         (session_id, user_input_hash, tool_name, tool_risk_level, arguments, \
          permission_result, confirmation_source, success, result_summary, created_at) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, datetime('now'))",
        rusqlite::params![
            "proactive_engine",
            "",
            tool_name,
            format!("{:?}", risk),
            arguments.to_string(),
            if auto_approved { "auto_approved" } else { "requires_confirmation" },
            "proactive_engine",
            if success { 1 } else { 0 },
            format!("[Proactive:{}] {}", rule.name, summary),
        ],
    );

    if let Err(e) = result {
        eprintln!("[Proactive] 写入审计日志失败: {}", e);
    }
}

/// 写入审计日志（Proactive 自动触发的操作）
async fn write_audit_log(
    app_handle: &AppHandle,
    rule: &ProactiveRule,
    action: &str,
    context: &str,
) {
    let state = match app_handle.try_state::<crate::AppState>() {
        Some(s) => s,
        None => return,
    };

    let db = state.db.lock().await;
    let result = db.connection().execute(
        "INSERT INTO audit_logs \
         (session_id, user_input_hash, tool_name, tool_risk_level, arguments, \
          permission_result, confirmation_source, success, result_summary, created_at) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, datetime('now'))",
        rusqlite::params![
            "proactive_engine",
            "",
            format!("proactive:{}", rule.id),
            "LOW",
            serde_json::json!({"rule": rule.name, "trigger": rule.trigger_type, "context": context}).to_string(),
            "auto_approved",
            "proactive_engine",
            1,
            format!("Proactive notification: {}", action),
        ],
    );

    if let Err(e) = result {
        eprintln!("[Proactive] 写入审计日志失败: {}", e);
    }
}
