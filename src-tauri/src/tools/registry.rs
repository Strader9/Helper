//! 工具注册中心
//!
//! ToolRegistry 是所有工具的唯一入口。
//! 工具注册时即声明风险等级，运行时不可更改。
//!
//! 三层闸门第一层：工具存在性校验 + 风险等级获取

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;

use crate::db::Database;
use crate::error::AppResult;

/// 工具类别（V21: 用于按需注入工具）
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolCategory {
    /// 文件操作
    File,
    /// 程序/应用管理
    Program,
    /// 进程与窗口
    ProcessWindow,
    /// 浏览器操作
    Browser,
    /// 系统信息与命令
    System,
    /// 长期记忆
    Memory,
    /// 本地文档知识库（索引与检索）
    Knowledge,
    /// Office 文档读写
    Office,
    /// 任务编排（计划、步骤、断点）
    Agents,
    /// 对外消息（邮件）
    Messaging,
    /// 图像与音视频
    Media,
    /// 自身状态 / 能力清单 / 自检
    Client,
    /// 动态技能
    Skill,
    /// 通用工具（始终加载）
    General,
}

impl ToolCategory {
    pub fn as_str(&self) -> &'static str {
        match self {
            ToolCategory::File => "file",
            ToolCategory::Program => "program",
            ToolCategory::ProcessWindow => "process_window",
            ToolCategory::Browser => "browser",
            ToolCategory::System => "system",
            ToolCategory::Memory => "memory",
            ToolCategory::Knowledge => "knowledge",
            ToolCategory::Office => "office",
            ToolCategory::Agents => "agents",
            ToolCategory::Messaging => "messaging",
            ToolCategory::Media => "media",
            ToolCategory::Client => "client",
            ToolCategory::Skill => "skill",
            ToolCategory::General => "general",
        }
    }
}

/// 风险等级
///
/// 与安全模块共享定义，确保全链路一致。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum RiskLevel {
    /// 纯只读、无副作用、无敏感信息 —— 直接放行
    Safe,
    /// 只读或轻微副作用、无安全风险 —— 直接放行
    Low,
    /// 有副作用、可能影响系统状态 —— 必须弹窗确认
    Medium,
    /// 高风险、可能造成数据丢失 —— 弹窗+二次确认+密码验证
    High,
    /// 致命风险、不可逆操作 —— 禁止执行
    Critical,
}

impl RiskLevel {
    pub fn as_str(&self) -> &'static str {
        match self {
            RiskLevel::Safe => "SAFE",
            RiskLevel::Low => "LOW",
            RiskLevel::Medium => "MEDIUM",
            RiskLevel::High => "HIGH",
            RiskLevel::Critical => "CRITICAL",
        }
    }
}

/// 工具执行结果
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolResult {
    pub success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_code: Option<String>,
}

impl ToolResult {
    pub fn ok(data: serde_json::Value) -> Self {
        Self {
            success: true,
            data: Some(data),
            error: None,
            error_code: None,
        }
    }

    pub fn err(code: &str, message: &str) -> Self {
        Self {
            success: false,
            data: None,
            error: Some(message.to_string()),
            error_code: Some(code.to_string()),
        }
    }
}

/// 工具 Trait
///
/// 所有工具必须实现此 trait。
/// 工具描述必须精确，禁止模糊表述。
#[async_trait::async_trait]
pub trait AgentTool: Send + Sync {
    /// 工具名称（唯一标识）
    fn name(&self) -> &'static str;

    /// 工具描述（用于 LLM 理解工具能力）
    fn description(&self) -> &'static str;

    /// 参数 Schema（JSON Schema 格式）
    fn parameters(&self) -> serde_json::Value;

    /// 风险等级（注册时确定，运行时不可更改）
    fn risk_level(&self) -> RiskLevel;

    /// 执行工具
    ///
    /// # Arguments
    /// * `args` - 工具参数（JSON 对象）
    ///
    /// # 注意
    /// 此方法只负责执行逻辑，不负责权限校验和审计日志。
    /// 权限校验由 PermissionManager 负责，审计由 AuditLogger 负责。
    async fn execute(&self, args: serde_json::Value) -> AppResult<ToolResult>;
}

/// 工具注册中心
///
/// 所有工具的注册表，提供按名称查找工具的能力。
/// V21: 支持工具分类，按需注入相关类别工具。
/// 实际注册名快照。工具层拿不到 registry 实例，只能读这里。
static REGISTERED_NAMES: std::sync::OnceLock<Vec<(String, String)>> = std::sync::OnceLock::new();

/// 启动时由 `ToolRegistry::new` 调用一次。
///
/// 存的是 (工具名, 类别) 对：自省清单要按类别分组，只存名字就分不出来。
pub fn publish_registry_snapshot(entries: Vec<(String, String)>) {
    let _ = REGISTERED_NAMES.set(entries);
}

/// 读注册快照；未初始化时返回空切片（而不是 panic）。
pub fn registry_snapshot() -> &'static [(String, String)] {
    REGISTERED_NAMES
        .get()
        .map(|entries| entries.as_slice())
        .unwrap_or(&[])
}

pub struct ToolRegistry {
    tools: HashMap<String, Arc<dyn AgentTool>>,
    /// 工具名 → 类别映射（V21）
    categories: HashMap<String, ToolCategory>,
}

impl ToolRegistry {
    /// 创建空的工具注册中心
    ///
    /// `db` 与 `app_dir` 用于需要持久化的工具（如 GitHub 趋势快照）。
    pub fn new(db: Arc<Mutex<Database>>, app_dir: PathBuf) -> Self {
        let mut registry = Self {
            tools: HashMap::new(),
            categories: HashMap::new(),
        };
        registry.register_builtin_tools();
        // GitHub 工具需要 DB 句柄以落快照
        use crate::tools::executors::github;
        registry.register_with_category(
            Arc::new(github::GithubTrendingTool::new(db.clone(), app_dir.clone())),
            ToolCategory::General,
        );
        registry.register_with_category(
            Arc::new(github::GithubSearchTool::new(db.clone(), app_dir.clone())),
            ToolCategory::General,
        );

        // 启动时核对能力契约覆盖率。
        // 缺契约不阻断启动（已经能跑的功能不该因为记账不全就罢工），
        // 但必须在日志里说清楚缺了谁——否则新增工具的人永远收不到提醒。
        let report = super::contract::coverage(&registry.list_names());
        if report.fully_declared() {
            eprintln!(
                "[Capability] 契约覆盖 {}/{}，实质副作用 {}/{} 已声明验证策略",
                report.declared, report.total_tools, report.material_verified, report.material
            );
        } else {
            eprintln!(
                "[Capability] 契约覆盖 {}/{}，缺少契约的工具: {}",
                report.declared,
                report.total_tools,
                report.missing.join(", ")
            );
        }
        if !report.material_without_verification.is_empty() {
            eprintln!(
                "[Capability] 以下实质副作用能力既无验证策略也无欠账说明: {}",
                report.material_without_verification.join(", ")
            );
        }
        if !report.material_with_verification_debt.is_empty() {
            eprintln!(
                "[Capability] 已验证 {} 个实质副作用能力，另有 {} 个登记为验证欠账: {}",
                report.material_verified,
                report.material_with_verification_debt.len(),
                report.material_with_verification_debt.join(", ")
            );
        }

        // 把"实际注册了什么"固定下来：自省类工具（client_*）拿不到 registry
        // 实例，只能读这份快照。少这一步，界面上的能力清单就只能靠猜。
        let snapshot: Vec<(String, String)> = registry
            .tools
            .keys()
            .map(|name| {
                let category = registry
                    .categories
                    .get(name)
                    .map(|c| c.as_str().to_string())
                    .unwrap_or_else(|| "general".to_string());
                (name.clone(), category)
            })
            .collect();
        publish_registry_snapshot(snapshot);

        registry
    }

    /// 注册内置工具
    fn register_builtin_tools(&mut self) {
        // === General 类（始终加载）===
        self.register_with_category(Arc::new(PingTool), ToolCategory::General);

        // === File 类 ===
        use crate::tools::executors;
        self.register_with_category(Arc::new(executors::file_ops::OpenFileTool), ToolCategory::File);
        self.register_with_category(Arc::new(executors::file_ops::ReadFileTool), ToolCategory::File);
        self.register_with_category(Arc::new(executors::file_ops::WriteFileTool), ToolCategory::File);
        self.register_with_category(Arc::new(executors::file_ops::ListDirectoryTool), ToolCategory::File);

        // === File 类（文件管理扩容：复制/移动/删除/属性/检索/建目录）===
        self.register_with_category(Arc::new(executors::fs_extra::CopyPathTool), ToolCategory::File);
        self.register_with_category(Arc::new(executors::fs_extra::MovePathTool), ToolCategory::File);
        self.register_with_category(Arc::new(executors::fs_extra::DeletePathTool), ToolCategory::File);
        self.register_with_category(Arc::new(executors::fs_extra::GetFileInfoTool), ToolCategory::File);
        self.register_with_category(Arc::new(executors::fs_extra::SearchFilesTool), ToolCategory::File);
        self.register_with_category(Arc::new(executors::fs_extra::SearchContentTool), ToolCategory::File);
        self.register_with_category(Arc::new(executors::fs_extra::CreateDirectoryTool), ToolCategory::File);

        // === Program 类 ===
        self.register_with_category(Arc::new(executors::program::LaunchProgramTool), ToolCategory::Program);
        self.register_with_category(Arc::new(executors::program::FindProgramTool), ToolCategory::Program);
        self.register_with_category(Arc::new(executors::program::CloseProgramTool), ToolCategory::Program);

        // === System 类 ===
        self.register_with_category(Arc::new(executors::system::ExecuteCommandTool), ToolCategory::System);
        self.register_with_category(Arc::new(executors::system::TakeScreenshotTool), ToolCategory::System);

        // === System 类（系统设置扩容：剪贴板/通知/网络/磁盘/环境/锁屏/开链接）===
        self.register_with_category(Arc::new(executors::os_extra::ReadClipboardTool), ToolCategory::System);
        self.register_with_category(Arc::new(executors::os_extra::WriteClipboardTool), ToolCategory::System);
        self.register_with_category(Arc::new(executors::os_extra::SendNotificationTool), ToolCategory::System);
        self.register_with_category(Arc::new(executors::os_extra::GetNetworkStatusTool), ToolCategory::System);
        self.register_with_category(Arc::new(executors::os_extra::GetDiskUsageTool), ToolCategory::System);
        self.register_with_category(Arc::new(executors::os_extra::GetEnvironmentTool), ToolCategory::System);
        self.register_with_category(Arc::new(executors::os_extra::LockScreenTool), ToolCategory::System);
        self.register_with_category(Arc::new(executors::os_extra::OpenUrlTool), ToolCategory::System);

        // === ProcessWindow 类 ===
        use crate::computer;
        self.register_with_category(Arc::new(computer::processes::ListProcessesTool), ToolCategory::ProcessWindow);
        self.register_with_category(Arc::new(computer::processes::KillProcessTool), ToolCategory::ProcessWindow);
        self.register_with_category(Arc::new(computer::windows::GetActiveWindowTool), ToolCategory::ProcessWindow);
        self.register_with_category(Arc::new(computer::windows::ListWindowsTool), ToolCategory::ProcessWindow);
        self.register_with_category(Arc::new(computer::windows::FocusWindowTool), ToolCategory::ProcessWindow);
        self.register_with_category(Arc::new(computer::windows::MinimizeWindowTool), ToolCategory::ProcessWindow);
        self.register_with_category(Arc::new(computer::windows::CloseWindowTool), ToolCategory::ProcessWindow);

        // === System 类（系统信息）===
        self.register_with_category(Arc::new(computer::system_info::GetSystemContextTool), ToolCategory::System);

        // === Program 类（应用管理器）===
        self.register_with_category(Arc::new(computer::app_manager::IsApplicationRunningTool), ToolCategory::Program);
        self.register_with_category(Arc::new(computer::app_manager::GetApplicationStatusTool), ToolCategory::Program);
        self.register_with_category(Arc::new(computer::app_manager::GetApplicationInfoTool), ToolCategory::Program);
        self.register_with_category(Arc::new(computer::app_manager::RestartApplicationTool), ToolCategory::Program);
        self.register_with_category(Arc::new(computer::app_manager::ListInstalledApplicationsTool), ToolCategory::Program);
        self.register_with_category(Arc::new(computer::app_manager::CloseAllApplicationsTool), ToolCategory::Program);
        self.register_with_category(Arc::new(computer::app_manager::LaunchWorkspaceTool), ToolCategory::Program);

        // === Memory 类 ===
        self.register_with_category(Arc::new(executors::memory::RememberTool), ToolCategory::Memory);
        self.register_with_category(Arc::new(executors::memory::RecallTool), ToolCategory::Memory);
        self.register_with_category(Arc::new(executors::memory::ForgetTool), ToolCategory::Memory);
        self.register_with_category(Arc::new(executors::memory::ListMemoriesTool), ToolCategory::Memory);
        self.register_with_category(Arc::new(executors::memory::UpdateMemoryTool), ToolCategory::Memory);
        self.register_with_category(Arc::new(executors::memory::MemoryStatsTool), ToolCategory::Memory);

        // === Client 类（自身状态 / 能力清单 / 自检 / 使用统计）===
        self.register_with_category(Arc::new(executors::client::ClientStateTool), ToolCategory::Client);
        self.register_with_category(Arc::new(executors::client::CapabilityManifestTool), ToolCategory::Client);
        self.register_with_category(Arc::new(executors::client::ClientHealthCheckTool), ToolCategory::Client);
        self.register_with_category(Arc::new(executors::client::ClientUsageStatsTool), ToolCategory::Client);

        // === Media 类（图像处理 + 音视频能力探测）===
        self.register_with_category(Arc::new(executors::media::ImageInfoTool), ToolCategory::Media);
        self.register_with_category(Arc::new(executors::media::ImageResizeTool), ToolCategory::Media);
        self.register_with_category(Arc::new(executors::media::ImageCropTool), ToolCategory::Media);
        self.register_with_category(Arc::new(executors::media::ImageConvertTool), ToolCategory::Media);
        self.register_with_category(Arc::new(executors::media::ImageRotateTool), ToolCategory::Media);
        self.register_with_category(Arc::new(executors::media::MediaToolsStatusTool), ToolCategory::Media);

        // === Messaging 类（邮件：发送 / 草稿 / 配置体检）===
        self.register_with_category(Arc::new(executors::messaging::SendEmailTool), ToolCategory::Messaging);
        self.register_with_category(Arc::new(executors::messaging::ComposeMailTool), ToolCategory::Messaging);
        self.register_with_category(Arc::new(executors::messaging::CheckMailConfigTool), ToolCategory::Messaging);
        self.register_with_category(Arc::new(executors::messaging::CreateCalendarEventTool), ToolCategory::Messaging);
        self.register_with_category(Arc::new(executors::messaging::SendWebhookTool), ToolCategory::Messaging);
        self.register_with_category(Arc::new(executors::messaging::SendSmsTool), ToolCategory::Messaging);

        // === Agents 类（任务编排：计划 / 步骤 / 断点）===
        self.register_with_category(Arc::new(executors::tasks::CreateTaskPlanTool), ToolCategory::Agents);
        self.register_with_category(Arc::new(executors::tasks::ListAgentTasksTool), ToolCategory::Agents);
        self.register_with_category(Arc::new(executors::tasks::GetAgentTaskTool), ToolCategory::Agents);
        self.register_with_category(Arc::new(executors::tasks::UpdateTaskStepTool), ToolCategory::Agents);
        self.register_with_category(Arc::new(executors::tasks::CancelAgentTaskTool), ToolCategory::Agents);
        self.register_with_category(Arc::new(executors::tasks::ResumeTaskContextTool), ToolCategory::Agents);

        // === Office 类（Word / Excel / PowerPoint 读写）===
        self.register_with_category(Arc::new(executors::office::ReadDocxTool), ToolCategory::Office);
        self.register_with_category(Arc::new(executors::office::ReadXlsxTool), ToolCategory::Office);
        self.register_with_category(Arc::new(executors::office::ReadPptxTool), ToolCategory::Office);
        self.register_with_category(Arc::new(executors::office::WriteDocxTool), ToolCategory::Office);
        self.register_with_category(Arc::new(executors::office::WriteXlsxTool), ToolCategory::Office);
        self.register_with_category(Arc::new(executors::office::WritePptxTool), ToolCategory::Office);

        // === Knowledge 类（本地文档索引与检索）===
        self.register_with_category(Arc::new(executors::knowledge::IndexDocumentTool), ToolCategory::Knowledge);
        self.register_with_category(Arc::new(executors::knowledge::SearchKnowledgeTool), ToolCategory::Knowledge);
        self.register_with_category(Arc::new(executors::knowledge::ListKnowledgeTool), ToolCategory::Knowledge);
        self.register_with_category(Arc::new(executors::knowledge::ForgetDocumentTool), ToolCategory::Knowledge);

        // === Browser 类（13 个）===
        self.register_with_category(Arc::new(executors::browser::BrowserOpenTool), ToolCategory::Browser);
        self.register_with_category(Arc::new(executors::browser::BrowserCloseTool), ToolCategory::Browser);
        self.register_with_category(Arc::new(executors::browser::BrowserNavigateTool), ToolCategory::Browser);
        self.register_with_category(Arc::new(executors::browser::BrowserScreenshotTool), ToolCategory::Browser);
        self.register_with_category(Arc::new(executors::browser::BrowserGetContentTool), ToolCategory::Browser);
        self.register_with_category(Arc::new(executors::browser::BrowserGetTitleTool), ToolCategory::Browser);
        self.register_with_category(Arc::new(executors::browser::BrowserClickTool), ToolCategory::Browser);
        self.register_with_category(Arc::new(executors::browser::BrowserTypeTool), ToolCategory::Browser);
        self.register_with_category(Arc::new(executors::browser::BrowserScrollTool), ToolCategory::Browser);
        self.register_with_category(Arc::new(executors::browser::BrowserWaitTool), ToolCategory::Browser);
        self.register_with_category(Arc::new(executors::browser::BrowserExecuteJsTool), ToolCategory::Browser);
        self.register_with_category(Arc::new(executors::browser::BrowserListTabsTool), ToolCategory::Browser);
        self.register_with_category(Arc::new(executors::browser::BrowserSwitchTabTool), ToolCategory::Browser);
    }

    /// 注册一个工具（带类别）
    pub fn register_with_category(&mut self, tool: Arc<dyn AgentTool>, category: ToolCategory) {
        let name = tool.name().to_string();
        self.categories.insert(name.clone(), category);
        self.tools.insert(name, tool);
    }

    /// 注册一个工具（默认 General 类别）
    pub fn register(&mut self, tool: Arc<dyn AgentTool>) {
        self.register_with_category(tool, ToolCategory::General);
    }

    /// 按名称查找工具
    pub fn get(&self, name: &str) -> Option<Arc<dyn AgentTool>> {
        self.tools.get(name).cloned()
    }

    /// 获取工具的类别
    pub fn get_category(&self, name: &str) -> Option<ToolCategory> {
        self.categories.get(name).copied()
    }

    /// 获取所有已注册工具的名称列表
    pub fn list_names(&self) -> Vec<String> {
        self.tools.keys().cloned().collect()
    }

    /// 按类别获取工具名称列表
    ///
    /// V21: 按需注入工具。始终包含 General 类。
    /// 如果 categories 为空，返回安全默认子集（General + Program + System），避免 LLM 看到所有工具导致误选。
    pub fn list_names_by_categories(&self, categories: &[ToolCategory]) -> Vec<String> {
        let effective_categories: Vec<ToolCategory> = if categories.is_empty() {
            // 安全默认子集：通用 + 程序 + 系统
            vec![ToolCategory::General, ToolCategory::Program, ToolCategory::System]
        } else {
            categories.to_vec()
        };

        let mut result: Vec<String> = Vec::new();
        for (name, cat) in &self.categories {
            if effective_categories.contains(cat) || *cat == ToolCategory::General {
                result.push(name.clone());
            }
        }
        result.sort();
        result
    }

    /// 获取工具数量
    pub fn len(&self) -> usize {
        self.tools.len()
    }

    pub fn is_empty(&self) -> bool {
        self.tools.is_empty()
    }
}

impl Default for ToolRegistry {
    fn default() -> Self {
        let db = Arc::new(Mutex::new(
            Database::new(std::path::Path::new(":memory:"))
                .expect("failed to open in-memory database"),
        ));
        let app_dir = dirs::data_dir().unwrap_or_else(|| std::path::PathBuf::from("."));
        Self::new(db, app_dir)
    }
}

// ============================================================
// 内置工具：Ping
// ============================================================

/// Ping 工具 —— 测试连通性
///
/// 风险等级：SAFE
/// 用途：验证 Agent Loop + Tool Calling 链路是否正常工作
struct PingTool;

#[async_trait::async_trait]
impl AgentTool for PingTool {
    fn name(&self) -> &'static str {
        "ping"
    }

    fn description(&self) -> &'static str {
        "测试工具连通性，返回 pong 和当前时间戳。用于验证工具调用链路是否正常。"
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "message": {
                    "type": "string",
                    "description": "可选的回显消息，默认 'hello'"
                }
            },
            "additionalProperties": false
        })
    }

    fn risk_level(&self) -> RiskLevel {
        RiskLevel::Safe
    }

    async fn execute(&self, args: serde_json::Value) -> AppResult<ToolResult> {
        let message = args
            .get("message")
            .and_then(|v| v.as_str())
            .unwrap_or("hello")
            .to_string();

        Ok(ToolResult::ok(serde_json::json!({
            "message": message,
            "timestamp": chrono::Utc::now().timestamp()
        })))
    }
}
