//! 能力契约 —— 每个工具必须声明"会做什么、风险多大、怎么算做完了"。
//!
//! 与 `RiskLevel` 的分工：`RiskLevel` 只回答"要不要弹窗"，本模块回答
//! "这次调用产生了什么类别的副作用、落在哪个范围、能否撤销、结束后拿什么
//! 证据来判定它真的完成了"。
//!
//! 设计原则：**宁可声明 none，也不假装有验证**。
//! `VerificationStrategy::None` 表示"没有可靠的机器可判据"，回执照样记录，
//! 但状态是 `NOT_REQUIRED` 而不是 `VERIFIED`。把无验证包装成已验证，
//! 比不验证更危险——上层会据此认为结果可信。

use super::RiskLevel;

/// 副作用的类别。
///
/// 判断依据是"这次调用改变了什么"，不是"这个工具听起来多危险"。
/// `LocalRead` / `NetworkRead` 没有实质副作用；其余都是实质副作用
/// （`is_material`），需要幂等键与回执验证。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SideEffect {
    /// 只读本地文件 / 系统信息
    LocalRead,
    /// 写本地文件
    LocalWrite,
    /// 改变本地运行状态（焦点、窗口、浏览器标签）
    LocalStateChange,
    /// 控制桌面程序（含结束进程）
    DesktopControl,
    /// 只读网络
    NetworkRead,
    /// 改变外部系统的状态（未来发布/提交类能力使用）
    ExternalStateChange,
    /// 向外部发送信息
    ExternalCommunication,
    /// 执行进程/命令
    ProcessExecution,
    /// 安装或卸载软件
    Installation,
    /// 无任何可观察副作用
    None,
}

impl SideEffect {
    pub fn as_str(&self) -> &'static str {
        match self {
            SideEffect::LocalRead => "local_read",
            SideEffect::LocalWrite => "local_write",
            SideEffect::LocalStateChange => "local_state_change",
            SideEffect::DesktopControl => "desktop_control",
            SideEffect::NetworkRead => "network_read",
            SideEffect::ExternalStateChange => "external_state_change",
            SideEffect::ExternalCommunication => "external_communication",
            SideEffect::ProcessExecution => "process_execution",
            SideEffect::Installation => "installation",
            SideEffect::None => "none",
        }
    }

    /// 是否属于"实质副作用"——需要幂等键、需要验证策略。
    ///
    /// 只读类不算：重放一次只读调用不会让世界变得更糟，
    /// 强制给它们配幂等键只会拖慢每一次读取。
    pub fn is_material(&self) -> bool {
        !matches!(
            self,
            SideEffect::LocalRead | SideEffect::NetworkRead | SideEffect::None
        )
    }

    /// 从落库的标签还原副作用类别。
    ///
    /// 回执表里存的是字符串（`local_write` 之类）。证据层要把回执重新
    /// 分类时必须走同一条映射，否则一旦将来改了标签写法，
    /// "哪些算实质副作用"就会在账本侧和聚合侧给出不同答案。
    pub fn from_label(label: &str) -> Option<Self> {
        Some(match label {
            "local_read" => SideEffect::LocalRead,
            "local_write" => SideEffect::LocalWrite,
            "local_state_change" => SideEffect::LocalStateChange,
            "desktop_control" => SideEffect::DesktopControl,
            "network_read" => SideEffect::NetworkRead,
            "external_state_change" => SideEffect::ExternalStateChange,
            "external_communication" => SideEffect::ExternalCommunication,
            "process_execution" => SideEffect::ProcessExecution,
            "installation" => SideEffect::Installation,
            "none" => SideEffect::None,
            _ => return None,
        })
    }
}

/// 验证策略 —— 用哪一类证据判定"这件事真的做完了"。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VerificationStrategy {
    /// 工具返回里含有可核对的终态字段（如 exit_code、path）
    TerminalReceipt,
    /// 本地状态发生了可核对的改变（如写入字节数、删除条数）
    StateDiff,
    /// 产出了真实存在的文件（路径必须存在且非空）
    Artifact,
    /// 被调用方（OS/浏览器/外部服务）回执确认
    ProviderAck,
    /// 没有可靠的机器可判据
    None,
}

impl VerificationStrategy {
    pub fn as_str(&self) -> &'static str {
        match self {
            VerificationStrategy::TerminalReceipt => "terminal_receipt",
            VerificationStrategy::StateDiff => "state_diff",
            VerificationStrategy::Artifact => "artifact",
            VerificationStrategy::ProviderAck => "provider_ack",
            VerificationStrategy::None => "none",
        }
    }
}

/// 单个能力的契约声明。
pub struct CapabilityContract {
    /// 工具名，必须与 `AgentTool::name()` 完全一致
    pub tool: &'static str,
    /// 副作用类别
    pub side_effect: SideEffect,
    /// 副作用落点（给人和审计看的范围描述）
    pub scope: &'static str,
    /// 是否可撤销/可重放
    pub reversible: bool,
    /// 验证策略
    pub verification: VerificationStrategy,
    /// 判定成功必须出现的回执字段（含 0 / false 这类值也算"出现"）
    pub required_fields: &'static [&'static str],
}

impl CapabilityContract {
    /// 该契约是否要求跑完做机器校验
    pub fn requires_verification(&self) -> bool {
        self.verification != VerificationStrategy::None
    }
}

/// 全部内置能力的契约表。
///
/// 新增工具时必须在这里登记，否则启动日志会报"未声明契约"，
/// `contracts_cover_all_registered_tools` 测试也会失败。
/// 这是刻意的：没有契约的工具等于"没人说清它到底做了什么"。
pub const CONTRACTS: &[CapabilityContract] = &[
    // ============ General ============
    CapabilityContract {
        tool: "ping",
        side_effect: SideEffect::None,
        scope: "-",
        reversible: true,
        verification: VerificationStrategy::None,
        required_fields: &[],
    },
    CapabilityContract {
        tool: "github_trending",
        side_effect: SideEffect::NetworkRead,
        scope: "api.github.com",
        reversible: true,
        verification: VerificationStrategy::None,
        required_fields: &[],
    },
    CapabilityContract {
        tool: "github_search",
        side_effect: SideEffect::NetworkRead,
        scope: "api.github.com",
        reversible: true,
        verification: VerificationStrategy::None,
        required_fields: &[],
    },
    // ============ File ============
    CapabilityContract {
        tool: "open_file",
        side_effect: SideEffect::DesktopControl,
        scope: "系统默认程序",
        reversible: true,
        verification: VerificationStrategy::None,
        required_fields: &[],
    },
    CapabilityContract {
        tool: "read_file",
        side_effect: SideEffect::LocalRead,
        scope: "可信目录",
        reversible: true,
        verification: VerificationStrategy::TerminalReceipt,
        required_fields: &["path", "content"],
    },
    CapabilityContract {
        tool: "write_file",
        side_effect: SideEffect::LocalWrite,
        scope: "可信目录",
        reversible: true,
        verification: VerificationStrategy::StateDiff,
        required_fields: &["path", "bytes_written"],
    },
    CapabilityContract {
        tool: "list_directory",
        side_effect: SideEffect::LocalRead,
        scope: "可信目录",
        reversible: true,
        verification: VerificationStrategy::TerminalReceipt,
        required_fields: &["path", "entries", "count"],
    },
    CapabilityContract {
        // verified_bytes 是复制完成后重新统计落点得到的字节数：
        // 与源字节数不一致时，这一次复制就没有真正完成。
        tool: "copy_path",
        side_effect: SideEffect::LocalWrite,
        scope: "文件系统",
        reversible: true,
        verification: VerificationStrategy::StateDiff,
        required_fields: &["source", "destination", "kind", "bytes", "verified_bytes"],
    },
    CapabilityContract {
        // source_gone 由工具在动作后重新探测源路径得出，是移动的终态判据。
        tool: "move_path",
        side_effect: SideEffect::LocalWrite,
        scope: "文件系统",
        reversible: true,
        verification: VerificationStrategy::StateDiff,
        required_fields: &["source", "destination", "mode", "source_gone"],
    },
    CapabilityContract {
        // 删除默认进回收站，可恢复；exists_after 由动作后重新探测得出。
        tool: "delete_path",
        side_effect: SideEffect::LocalWrite,
        scope: "文件系统（默认回收站）",
        reversible: false,
        verification: VerificationStrategy::StateDiff,
        required_fields: &["path", "kind", "exists_after"],
    },
    CapabilityContract {
        tool: "get_file_info",
        side_effect: SideEffect::LocalRead,
        scope: "文件系统元数据",
        reversible: true,
        verification: VerificationStrategy::TerminalReceipt,
        required_fields: &["path", "kind", "bytes"],
    },
    CapabilityContract {
        tool: "search_files",
        side_effect: SideEffect::LocalRead,
        scope: "可信目录",
        reversible: true,
        verification: VerificationStrategy::TerminalReceipt,
        required_fields: &["root", "pattern", "count"],
    },
    CapabilityContract {
        tool: "search_content",
        side_effect: SideEffect::LocalRead,
        scope: "可信目录内的文本文件",
        reversible: true,
        verification: VerificationStrategy::TerminalReceipt,
        required_fields: &["root", "query", "count"],
    },
    CapabilityContract {
        tool: "create_directory",
        side_effect: SideEffect::LocalWrite,
        scope: "文件系统",
        reversible: true,
        verification: VerificationStrategy::Artifact,
        required_fields: &["path", "is_dir"],
    },
    // ============ Program ============
    CapabilityContract {
        tool: "launch_program",
        side_effect: SideEffect::ProcessExecution,
        scope: "本机应用",
        reversible: true,
        verification: VerificationStrategy::ProviderAck,
        required_fields: &["program", "path", "pid"],
    },
    CapabilityContract {
        tool: "find_program",
        side_effect: SideEffect::LocalRead,
        scope: "开始菜单 / App Paths",
        reversible: true,
        verification: VerificationStrategy::TerminalReceipt,
        required_fields: &["name", "paths", "found"],
    },
    CapabilityContract {
        tool: "close_program",
        side_effect: SideEffect::DesktopControl,
        scope: "本机进程",
        reversible: false,
        verification: VerificationStrategy::ProviderAck,
        required_fields: &["program", "process"],
    },
    // ============ System ============
    CapabilityContract {
        tool: "execute_command",
        side_effect: SideEffect::ProcessExecution,
        scope: "cmd.exe",
        reversible: false,
        verification: VerificationStrategy::TerminalReceipt,
        required_fields: &["stdout", "exit_code"],
    },
    CapabilityContract {
        tool: "take_screenshot",
        side_effect: SideEffect::LocalWrite,
        scope: "临时目录",
        reversible: true,
        verification: VerificationStrategy::Artifact,
        required_fields: &["path"],
    },
    CapabilityContract {
        tool: "read_clipboard",
        side_effect: SideEffect::LocalRead,
        scope: "系统剪贴板",
        reversible: true,
        verification: VerificationStrategy::TerminalReceipt,
        required_fields: &["text", "chars"],
    },
    CapabilityContract {
        // readback_matched 是写完立刻读回来与输入比对的结果；
        // 对不上时工具本身返回失败，不会走到"已验证"。
        tool: "write_clipboard",
        side_effect: SideEffect::LocalStateChange,
        scope: "系统剪贴板",
        reversible: true,
        verification: VerificationStrategy::StateDiff,
        required_fields: &["chars", "written", "readback_matched"],
    },
    CapabilityContract {
        // 通知是否真的显示受"专注助手/免打扰"设置控制，
        // 工具只能拿到 toast 提交结果，拿不到展示回执。已登记欠账。
        tool: "send_notification",
        side_effect: SideEffect::LocalStateChange,
        scope: "Windows 通知中心",
        reversible: true,
        verification: VerificationStrategy::None,
        required_fields: &[],
    },
    CapabilityContract {
        tool: "get_network_status",
        side_effect: SideEffect::NetworkRead,
        scope: "本机网卡 / 223.5.5.5",
        reversible: true,
        verification: VerificationStrategy::TerminalReceipt,
        required_fields: &["hostname", "interfaces", "reachable"],
    },
    CapabilityContract {
        tool: "get_disk_usage",
        side_effect: SideEffect::LocalRead,
        scope: "本地磁盘",
        reversible: true,
        verification: VerificationStrategy::TerminalReceipt,
        required_fields: &["drives", "count"],
    },
    CapabilityContract {
        tool: "get_environment",
        side_effect: SideEffect::LocalRead,
        scope: "进程环境变量",
        reversible: true,
        verification: VerificationStrategy::TerminalReceipt,
        required_fields: &["count", "names"],
    },
    CapabilityContract {
        // 锁屏后桌面会话切到安全桌面，普通进程枚举不到状态，
        // 没有任何可用的机器判据。已登记欠账。
        tool: "lock_screen",
        side_effect: SideEffect::LocalStateChange,
        scope: "Windows 桌面会话",
        reversible: true,
        verification: VerificationStrategy::None,
        required_fields: &[],
    },
    CapabilityContract {
        // 链接由默认浏览器承接，工具只拿到 shell 受理结果。已登记欠账。
        tool: "open_url",
        side_effect: SideEffect::DesktopControl,
        scope: "系统默认浏览器",
        reversible: true,
        verification: VerificationStrategy::None,
        required_fields: &[],
    },
    // ============ ProcessWindow ============
    CapabilityContract {
        tool: "list_processes",
        side_effect: SideEffect::LocalRead,
        scope: "本机进程表",
        reversible: true,
        verification: VerificationStrategy::TerminalReceipt,
        required_fields: &["processes", "count"],
    },
    CapabilityContract {
        tool: "kill_process",
        side_effect: SideEffect::DesktopControl,
        scope: "本机进程",
        reversible: false,
        verification: VerificationStrategy::ProviderAck,
        required_fields: &["target"],
    },
    CapabilityContract {
        tool: "get_active_window",
        side_effect: SideEffect::LocalRead,
        scope: "前台窗口",
        reversible: true,
        verification: VerificationStrategy::None,
        required_fields: &[],
    },
    CapabilityContract {
        tool: "list_windows",
        side_effect: SideEffect::LocalRead,
        scope: "可见窗口",
        reversible: true,
        verification: VerificationStrategy::TerminalReceipt,
        required_fields: &["windows", "count"],
    },
    CapabilityContract {
        tool: "focus_window",
        side_effect: SideEffect::LocalStateChange,
        scope: "目标窗口",
        reversible: true,
        verification: VerificationStrategy::None,
        required_fields: &[],
    },
    CapabilityContract {
        tool: "minimize_window",
        side_effect: SideEffect::LocalStateChange,
        scope: "目标窗口",
        reversible: true,
        verification: VerificationStrategy::None,
        required_fields: &[],
    },
    CapabilityContract {
        tool: "close_window",
        side_effect: SideEffect::LocalStateChange,
        scope: "目标窗口",
        reversible: false,
        verification: VerificationStrategy::None,
        required_fields: &[],
    },
    // ============ Input（键鼠模拟）============
    // 这几个工具的副作用不在文件系统上，而在**别人家的窗口里**：点一下可能
    // 就是提交订单、发一条消息。所以一律记 DesktopControl，并且必填回执字段
    // 用来核对「确实发出去了」而不是「函数返回了」。
    CapabilityContract {
        tool: "mouse_move",
        side_effect: SideEffect::DesktopControl,
        scope: "鼠标光标位置",
        reversible: true,
        verification: VerificationStrategy::TerminalReceipt,
        required_fields: &["x", "y"],
    },
    CapabilityContract {
        tool: "mouse_click",
        side_effect: SideEffect::DesktopControl,
        scope: "屏幕坐标处的目标控件",
        reversible: false,
        verification: VerificationStrategy::TerminalReceipt,
        required_fields: &["button", "count"],
    },
    CapabilityContract {
        tool: "mouse_drag",
        side_effect: SideEffect::DesktopControl,
        scope: "拖拽起点到终点之间的目标",
        reversible: false,
        verification: VerificationStrategy::TerminalReceipt,
        required_fields: &["from", "to", "button"],
    },
    CapabilityContract {
        tool: "mouse_scroll",
        side_effect: SideEffect::DesktopControl,
        scope: "光标下的可滚动区域",
        reversible: true,
        verification: VerificationStrategy::TerminalReceipt,
        required_fields: &["delta"],
    },
    CapabilityContract {
        tool: "keyboard_type",
        side_effect: SideEffect::DesktopControl,
        scope: "当前焦点输入框",
        reversible: false,
        verification: VerificationStrategy::TerminalReceipt,
        required_fields: &["typed_chars"],
    },
    CapabilityContract {
        tool: "keyboard_press",
        side_effect: SideEffect::DesktopControl,
        scope: "当前焦点窗口",
        reversible: false,
        verification: VerificationStrategy::TerminalReceipt,
        required_fields: &["key", "parts"],
    },
    CapabilityContract {
        tool: "get_system_context",
        side_effect: SideEffect::LocalRead,
        scope: "系统信息",
        reversible: true,
        verification: VerificationStrategy::TerminalReceipt,
        required_fields: &["summary"],
    },
    // ============ Program（应用管理器）============
    CapabilityContract {
        tool: "is_application_running",
        side_effect: SideEffect::LocalRead,
        scope: "本机进程表",
        reversible: true,
        verification: VerificationStrategy::TerminalReceipt,
        required_fields: &["name", "running", "pids"],
    },
    CapabilityContract {
        tool: "get_application_status",
        side_effect: SideEffect::LocalRead,
        scope: "本机进程表",
        reversible: true,
        verification: VerificationStrategy::TerminalReceipt,
        required_fields: &["name", "status", "running"],
    },
    CapabilityContract {
        tool: "get_application_info",
        side_effect: SideEffect::LocalRead,
        scope: "本机进程表",
        reversible: true,
        verification: VerificationStrategy::TerminalReceipt,
        required_fields: &["name", "processes", "count"],
    },
    CapabilityContract {
        tool: "restart_application",
        side_effect: SideEffect::ProcessExecution,
        scope: "本机应用",
        reversible: true,
        verification: VerificationStrategy::ProviderAck,
        required_fields: &["name", "restarted"],
    },
    CapabilityContract {
        tool: "list_installed_applications",
        side_effect: SideEffect::LocalRead,
        scope: "注册表 / 开始菜单",
        reversible: true,
        verification: VerificationStrategy::TerminalReceipt,
        required_fields: &["applications", "total", "returned"],
    },
    CapabilityContract {
        tool: "close_all_applications",
        side_effect: SideEffect::DesktopControl,
        scope: "本机应用窗口",
        reversible: false,
        verification: VerificationStrategy::ProviderAck,
        required_fields: &["closed_count", "closed_windows"],
    },
    CapabilityContract {
        tool: "launch_workspace",
        side_effect: SideEffect::ProcessExecution,
        scope: "本机应用组",
        reversible: true,
        verification: VerificationStrategy::None,
        required_fields: &[],
    },
    // ============ Memory ============
    CapabilityContract {
        tool: "remember",
        side_effect: SideEffect::LocalWrite,
        scope: "长期记忆库",
        reversible: true,
        verification: VerificationStrategy::StateDiff,
        required_fields: &["memory_id", "content"],
    },
    CapabilityContract {
        tool: "recall",
        side_effect: SideEffect::LocalRead,
        scope: "长期记忆库",
        reversible: true,
        verification: VerificationStrategy::TerminalReceipt,
        required_fields: &["count", "memories"],
    },
    CapabilityContract {
        tool: "forget",
        side_effect: SideEffect::LocalWrite,
        scope: "长期记忆库",
        reversible: false,
        verification: VerificationStrategy::StateDiff,
        required_fields: &["deleted"],
    },
    CapabilityContract {
        tool: "list_memories",
        side_effect: SideEffect::LocalRead,
        scope: "长期记忆库",
        reversible: true,
        verification: VerificationStrategy::TerminalReceipt,
        required_fields: &["count", "memories"],
    },
    CapabilityContract {
        // updated 是 UPDATE 影响行数换算来的：未命中时为 false，不会假报成功
        tool: "update_memory",
        side_effect: SideEffect::LocalWrite,
        scope: "长期记忆库",
        reversible: true,
        verification: VerificationStrategy::StateDiff,
        required_fields: &["memory_id", "updated"],
    },
    CapabilityContract {
        tool: "memory_stats",
        side_effect: SideEffect::LocalRead,
        scope: "长期记忆库",
        reversible: true,
        verification: VerificationStrategy::TerminalReceipt,
        required_fields: &["total", "by_type"],
    },
    // ============ Messaging ============
    CapabilityContract {
        tool: "send_email",
        side_effect: SideEffect::ExternalCommunication,
        scope: "外部邮箱",
        // 邮件发出去就收不回来：不可撤销
        reversible: false,
        verification: VerificationStrategy::ProviderAck,
        required_fields: &["sent", "to", "smtp_code"],
    },
    CapabilityContract {
        tool: "compose_mail",
        side_effect: SideEffect::LocalStateChange,
        scope: "默认邮件客户端",
        reversible: true,
        // 只证明了"客户端进程起来了"，草稿是否真被填入由客户端决定
        verification: VerificationStrategy::None,
        required_fields: &["opened"],
    },
    CapabilityContract {
        tool: "check_mail_config",
        side_effect: SideEffect::NetworkRead,
        scope: "SMTP 服务器",
        reversible: true,
        verification: VerificationStrategy::TerminalReceipt,
        required_fields: &["configured"],
    },
    CapabilityContract {
        tool: "create_calendar_event",
        side_effect: SideEffect::LocalWrite,
        scope: "本地 .ics 文件",
        reversible: true,
        verification: VerificationStrategy::StateDiff,
        required_fields: &["created", "path", "title"],
    },
    CapabilityContract {
        tool: "send_webhook",
        side_effect: SideEffect::ExternalCommunication,
        scope: "外部群机器人",
        reversible: false,
        verification: VerificationStrategy::ProviderAck,
        required_fields: &["sent", "status"],
    },
    CapabilityContract {
        tool: "send_sms",
        side_effect: SideEffect::ExternalCommunication,
        scope: "短信网关",
        reversible: false,
        verification: VerificationStrategy::ProviderAck,
        required_fields: &["sent", "status"],
    },
    // ============ Media ============
    CapabilityContract {
        tool: "image_info",
        side_effect: SideEffect::LocalRead,
        scope: "本地图片",
        reversible: true,
        verification: VerificationStrategy::TerminalReceipt,
        required_fields: &["path", "width", "height", "bytes"],
    },
    CapabilityContract {
        tool: "image_resize",
        side_effect: SideEffect::LocalWrite,
        scope: "输出目录",
        reversible: true,
        verification: VerificationStrategy::Artifact,
        // 判据是重新打开输出文件读尺寸，不是内存里那个 Bitmap 对象
        required_fields: &["output.path", "output.width", "output.height", "output.dimensions_match"],
    },
    CapabilityContract {
        tool: "image_crop",
        side_effect: SideEffect::LocalWrite,
        scope: "输出目录",
        reversible: true,
        verification: VerificationStrategy::Artifact,
        required_fields: &["output.path", "output.width", "output.height", "output.dimensions_match"],
    },
    CapabilityContract {
        tool: "image_convert",
        side_effect: SideEffect::LocalWrite,
        scope: "输出目录",
        reversible: true,
        verification: VerificationStrategy::Artifact,
        required_fields: &["output.path", "output.bytes"],
    },
    CapabilityContract {
        tool: "image_rotate",
        side_effect: SideEffect::LocalWrite,
        scope: "输出目录",
        reversible: true,
        verification: VerificationStrategy::Artifact,
        required_fields: &["output.path", "output.dimensions_match"],
    },
    CapabilityContract {
        tool: "media_tools_status",
        side_effect: SideEffect::LocalRead,
        scope: "本机可执行文件",
        reversible: true,
        verification: VerificationStrategy::TerminalReceipt,
        required_fields: &["image", "video", "audio"],
    },
    CapabilityContract {
        tool: "video_info",
        side_effect: SideEffect::LocalRead,
        scope: "本地视频",
        reversible: true,
        verification: VerificationStrategy::TerminalReceipt,
        required_fields: &["path", "duration_seconds"],
    },
    CapabilityContract {
        tool: "audio_info",
        side_effect: SideEffect::LocalRead,
        scope: "本地音频",
        reversible: true,
        verification: VerificationStrategy::TerminalReceipt,
        required_fields: &["path", "duration_seconds"],
    },
    CapabilityContract {
        tool: "extract_audio",
        side_effect: SideEffect::LocalWrite,
        scope: "输出音频文件",
        reversible: true,
        verification: VerificationStrategy::StateDiff,
        required_fields: &["source", "output", "size_bytes"],
    },
    CapabilityContract {
        tool: "video_trim",
        side_effect: SideEffect::LocalWrite,
        scope: "输出视频文件",
        reversible: true,
        verification: VerificationStrategy::StateDiff,
        required_fields: &["source", "output", "size_bytes"],
    },
    // ============ Cad ============
    CapabilityContract {
        tool: "cad_generate_dxf",
        side_effect: SideEffect::LocalWrite,
        scope: "本地 dxf 文件",
        reversible: true,
        verification: VerificationStrategy::StateDiff,
        required_fields: &["created", "path", "entity_count"],
    },
    CapabilityContract {
        tool: "cad_dxf_info",
        side_effect: SideEffect::LocalRead,
        scope: "本地 dxf 文件",
        reversible: true,
        verification: VerificationStrategy::TerminalReceipt,
        required_fields: &["path", "entity_count"],
    },
    // ============ Industry ============
    CapabilityContract {
        tool: "industry_scenarios",
        side_effect: SideEffect::LocalRead,
        scope: "内置行业场景表",
        reversible: true,
        verification: VerificationStrategy::TerminalReceipt,
        required_fields: &["count", "scenarios"],
    },
    CapabilityContract {
        tool: "industry_guide",
        side_effect: SideEffect::LocalRead,
        scope: "内置行业场景表",
        reversible: true,
        verification: VerificationStrategy::TerminalReceipt,
        required_fields: &["found", "query"],
    },
    // ============ Mcp ============
    CapabilityContract {
        // 列出外部工具是纯网络读；返回的清单本身无副作用。
        tool: "mcp_list_tools",
        side_effect: SideEffect::NetworkRead,
        scope: "sidecar 已连接的 MCP server",
        reversible: true,
        verification: VerificationStrategy::TerminalReceipt,
        required_fields: &["servers", "tools"],
    },
    CapabilityContract {
        // 调用外部工具会改变外部系统状态，副作用落点完全由远端决定，
        // 本机契约无法穷举，只能拿到 sidecar 的调用回执作为 ProviderAck。
        tool: "mcp_call",
        side_effect: SideEffect::ExternalStateChange,
        scope: "MCP server 远端（由用户配置）",
        reversible: false,
        verification: VerificationStrategy::ProviderAck,
        required_fields: &["server", "tool", "isError"],
    },
    // ============ Web（联网检索）============
    CapabilityContract {
        tool: "web_search",
        side_effect: SideEffect::NetworkRead,
        scope: "公共搜索引擎",
        reversible: true,
        verification: VerificationStrategy::TerminalReceipt,
        required_fields: &["query", "results"],
    },
    CapabilityContract {
        tool: "url_fetch",
        side_effect: SideEffect::NetworkRead,
        scope: "用户指定的远端 URL",
        reversible: true,
        verification: VerificationStrategy::TerminalReceipt,
        required_fields: &["url", "status", "text"],
    },
    // ============ Automation（常驻定时任务）============
    CapabilityContract {
        // 建作业不改外部世界，只在本机写一条"未来要做什么"的授权记录。
        // 它真正的副作用发生在到点之后（那时每次工具调用仍走各自契约）。
        tool: "schedule_job",
        side_effect: SideEffect::LocalWrite,
        scope: "本机 scheduled_jobs 表",
        reversible: true,
        verification: VerificationStrategy::StateDiff,
        required_fields: &["created", "id", "next_run_at"],
    },
    CapabilityContract {
        tool: "list_scheduled_jobs",
        side_effect: SideEffect::LocalRead,
        scope: "本机 scheduled_jobs 表",
        reversible: true,
        verification: VerificationStrategy::TerminalReceipt,
        required_fields: &["count", "jobs"],
    },
    CapabilityContract {
        tool: "cancel_scheduled_job",
        side_effect: SideEffect::LocalWrite,
        scope: "本机 scheduled_jobs 表（连带运行历史）",
        reversible: false,
        verification: VerificationStrategy::StateDiff,
        required_fields: &["deleted", "id"],
    },
    CapabilityContract {
        tool: "set_job_enabled",
        side_effect: SideEffect::LocalWrite,
        scope: "本机 scheduled_jobs.enabled",
        reversible: true,
        verification: VerificationStrategy::StateDiff,
        required_fields: &["id", "enabled"],
    },
    CapabilityContract {
        // 立刻执行 = 把 next_run_at 提前到现在。作业本身由调度器异步接手，
        // 所以这里的终态只能是"已标记到期"，执行结果要另查 job_run_history。
        tool: "run_job_now",
        side_effect: SideEffect::LocalStateChange,
        scope: "本机调度队列",
        reversible: true,
        verification: VerificationStrategy::StateDiff,
        required_fields: &["triggered", "id"],
    },
    CapabilityContract {
        tool: "job_run_history",
        side_effect: SideEffect::LocalRead,
        scope: "本机 scheduled_job_runs 表",
        reversible: true,
        verification: VerificationStrategy::TerminalReceipt,
        required_fields: &["count", "runs"],
    },
    // ============ Client ============
    CapabilityContract {
        tool: "client_state",
        side_effect: SideEffect::LocalRead,
        scope: "自身运行状态",
        reversible: true,
        verification: VerificationStrategy::TerminalReceipt,
        required_fields: &["version", "db_path", "capabilities"],
    },
    CapabilityContract {
        tool: "client_capability_manifest",
        side_effect: SideEffect::LocalRead,
        scope: "能力契约表",
        reversible: true,
        verification: VerificationStrategy::TerminalReceipt,
        required_fields: &["count", "items"],
    },
    CapabilityContract {
        tool: "client_health_check",
        side_effect: SideEffect::LocalRead,
        scope: "自身运行状态",
        reversible: true,
        verification: VerificationStrategy::TerminalReceipt,
        required_fields: &["status", "integrity", "findings"],
    },
    CapabilityContract {
        tool: "client_usage_stats",
        side_effect: SideEffect::LocalRead,
        scope: "回执账本",
        reversible: true,
        verification: VerificationStrategy::TerminalReceipt,
        required_fields: &["total_receipts", "by_status"],
    },
    // ============ Agents ============
    CapabilityContract {
        tool: "create_task_plan",
        side_effect: SideEffect::LocalWrite,
        scope: "本地任务表",
        reversible: true,
        verification: VerificationStrategy::StateDiff,
        required_fields: &["task_id", "step_count", "persisted"],
    },
    CapabilityContract {
        tool: "list_agent_tasks",
        side_effect: SideEffect::LocalRead,
        scope: "本地任务表",
        reversible: true,
        verification: VerificationStrategy::TerminalReceipt,
        required_fields: &["count", "tasks"],
    },
    CapabilityContract {
        tool: "get_agent_task",
        side_effect: SideEffect::LocalRead,
        scope: "本地任务表",
        reversible: true,
        verification: VerificationStrategy::TerminalReceipt,
        required_fields: &["task_id", "steps"],
    },
    CapabilityContract {
        tool: "update_task_step",
        side_effect: SideEffect::LocalWrite,
        scope: "本地任务表",
        reversible: true,
        verification: VerificationStrategy::StateDiff,
        required_fields: &["task_id", "step_id", "persisted_status"],
    },
    CapabilityContract {
        tool: "cancel_agent_task",
        side_effect: SideEffect::LocalWrite,
        scope: "本地任务表",
        reversible: true,
        verification: VerificationStrategy::StateDiff,
        required_fields: &["task_id", "status"],
    },
    CapabilityContract {
        tool: "resume_task_context",
        side_effect: SideEffect::LocalRead,
        scope: "本地任务表",
        reversible: true,
        verification: VerificationStrategy::TerminalReceipt,
        required_fields: &["found"],
    },
    // ============ Office ============
    CapabilityContract {
        tool: "read_docx",
        side_effect: SideEffect::LocalRead,
        scope: "本地 docx 文件",
        reversible: true,
        verification: VerificationStrategy::TerminalReceipt,
        required_fields: &["path", "text", "chars"],
    },
    CapabilityContract {
        tool: "read_xlsx",
        side_effect: SideEffect::LocalRead,
        scope: "本地 xlsx 文件",
        reversible: true,
        verification: VerificationStrategy::TerminalReceipt,
        required_fields: &["path", "rows", "row_count"],
    },
    CapabilityContract {
        tool: "read_pptx",
        side_effect: SideEffect::LocalRead,
        scope: "本地 pptx 文件",
        reversible: true,
        verification: VerificationStrategy::TerminalReceipt,
        required_fields: &["path", "slides", "count"],
    },
    CapabilityContract {
        tool: "write_docx",
        side_effect: SideEffect::LocalWrite,
        scope: "本地 docx 文件",
        reversible: true,
        verification: VerificationStrategy::StateDiff,
        required_fields: &["path", "paragraphs", "size_bytes"],
    },
    CapabilityContract {
        tool: "write_xlsx",
        side_effect: SideEffect::LocalWrite,
        scope: "本地 xlsx 文件",
        reversible: true,
        verification: VerificationStrategy::StateDiff,
        required_fields: &["path", "rows", "size_bytes"],
    },
    CapabilityContract {
        tool: "write_pptx",
        side_effect: SideEffect::LocalWrite,
        scope: "本地 pptx 文件",
        reversible: true,
        verification: VerificationStrategy::StateDiff,
        required_fields: &["path", "slides", "size_bytes"],
    },
    // ============ Workflow ============
    CapabilityContract {
        // verified_step_count 是写库后重新读出来的步数：
        // 序列化失败时 INSERT 也可能成功，只有回读能证明定义真能用
        tool: "workflow_create",
        side_effect: SideEffect::LocalWrite,
        scope: "本地工作流定义",
        reversible: true,
        verification: VerificationStrategy::StateDiff,
        required_fields: &["id", "name", "step_count", "verified_step_count"],
    },
    CapabilityContract {
        tool: "workflow_list",
        side_effect: SideEffect::LocalRead,
        scope: "本地工作流定义",
        reversible: true,
        verification: VerificationStrategy::TerminalReceipt,
        required_fields: &["count", "workflows"],
    },
    CapabilityContract {
        tool: "workflow_get",
        side_effect: SideEffect::LocalRead,
        scope: "本地工作流定义",
        reversible: true,
        verification: VerificationStrategy::TerminalReceipt,
        required_fields: &["id", "name", "step_count", "steps"],
    },
    CapabilityContract {
        tool: "workflow_update",
        side_effect: SideEffect::LocalWrite,
        scope: "本地工作流定义",
        reversible: true,
        verification: VerificationStrategy::StateDiff,
        required_fields: &["id", "name", "step_count", "steps_replaced"],
    },
    CapabilityContract {
        // 删定义连带删运行历史，不可恢复
        tool: "workflow_delete",
        side_effect: SideEffect::LocalWrite,
        scope: "本地工作流定义 + 运行历史",
        reversible: false,
        verification: VerificationStrategy::StateDiff,
        required_fields: &["deleted", "name", "runs_deleted"],
    },
    CapabilityContract {
        tool: "workflow_validate",
        side_effect: SideEffect::None,
        scope: "-",
        reversible: true,
        verification: VerificationStrategy::TerminalReceipt,
        required_fields: &["valid", "step_count", "problems"],
    },
    CapabilityContract {
        // 实际副作用取决于步骤内容（可能写文件、执行命令），
        // 但每次步骤都走 ToolExecutor 的完整闸门，回执留在各自的记账里
        tool: "workflow_run",
        side_effect: SideEffect::LocalStateChange,
        scope: "由步骤定义决定",
        reversible: false,
        verification: VerificationStrategy::StateDiff,
        required_fields: &["workflow", "run_id", "status", "ok_count", "fail_count"],
    },
    CapabilityContract {
        tool: "workflow_runs",
        side_effect: SideEffect::LocalRead,
        scope: "本地运行历史",
        reversible: true,
        verification: VerificationStrategy::TerminalReceipt,
        required_fields: &["count", "runs"],
    },
    CapabilityContract {
        tool: "workflow_run_detail",
        side_effect: SideEffect::LocalRead,
        scope: "本地运行历史",
        reversible: true,
        verification: VerificationStrategy::TerminalReceipt,
        required_fields: &["run_id", "run", "steps"],
    },
    CapabilityContract {
        tool: "workflow_set_enabled",
        side_effect: SideEffect::LocalWrite,
        scope: "本地工作流定义",
        reversible: true,
        verification: VerificationStrategy::StateDiff,
        required_fields: &["id", "name", "enabled"],
    },
    CapabilityContract {
        tool: "workflow_clone",
        side_effect: SideEffect::LocalWrite,
        scope: "本地工作流定义",
        reversible: true,
        verification: VerificationStrategy::StateDiff,
        required_fields: &["id", "name", "source", "step_count"],
    },
    CapabilityContract {
        tool: "workflow_export",
        side_effect: SideEffect::LocalWrite,
        scope: "本地 json 文件（可选）",
        reversible: true,
        verification: VerificationStrategy::TerminalReceipt,
        required_fields: &["count"],
    },
    CapabilityContract {
        tool: "workflow_import",
        side_effect: SideEffect::LocalWrite,
        scope: "本地工作流定义",
        reversible: true,
        verification: VerificationStrategy::StateDiff,
        required_fields: &["imported_count", "skipped_count", "failed_count"],
    },
    CapabilityContract {
        tool: "workflow_templates",
        side_effect: SideEffect::None,
        scope: "-",
        reversible: true,
        verification: VerificationStrategy::TerminalReceipt,
        required_fields: &["count", "templates"],
    },
    // ============ Work Takeover（工作接管）============
    // 只动本地台账与本地产物，外部动作永远走其它工具的三层闸门。
    CapabilityContract {
        // persisted 是写库后回读得出的终态：INSERT 成功但回读不到不算完成
        tool: "work_takeover_task_create",
        side_effect: SideEffect::LocalWrite,
        scope: "本地 work_takeover_tasks 表",
        reversible: true,
        verification: VerificationStrategy::StateDiff,
        required_fields: &["id", "status", "persisted"],
    },
    CapabilityContract {
        tool: "work_takeover_task_list",
        side_effect: SideEffect::LocalRead,
        scope: "本地 work_takeover_tasks 表",
        reversible: true,
        verification: VerificationStrategy::TerminalReceipt,
        required_fields: &["count", "tasks"],
    },
    CapabilityContract {
        tool: "work_takeover_task_get",
        side_effect: SideEffect::LocalRead,
        scope: "本地 work_takeover_tasks 表",
        reversible: true,
        verification: VerificationStrategy::TerminalReceipt,
        required_fields: &["task"],
    },
    CapabilityContract {
        tool: "work_takeover_task_update",
        side_effect: SideEffect::LocalWrite,
        scope: "本地 work_takeover_tasks 表",
        reversible: true,
        verification: VerificationStrategy::StateDiff,
        required_fields: &["id", "status", "persisted"],
    },
    CapabilityContract {
        tool: "work_takeover_task_continue",
        side_effect: SideEffect::LocalRead,
        scope: "本地 work_takeover_tasks 表",
        reversible: true,
        verification: VerificationStrategy::TerminalReceipt,
        required_fields: &["ok", "task"],
    },
    CapabilityContract {
        tool: "work_takeover_task_advance",
        side_effect: SideEffect::LocalWrite,
        scope: "本地 work_takeover_tasks 表",
        reversible: true,
        verification: VerificationStrategy::StateDiff,
        required_fields: &["ok", "status", "task"],
    },
    CapabilityContract {
        // folder_path 是实际写出的目录；只有 task.json 与 summary.md 都落盘才算完成
        tool: "work_takeover_task_export_packet",
        side_effect: SideEffect::LocalWrite,
        scope: "本地任务包目录（task.json + summary.md）",
        reversible: true,
        verification: VerificationStrategy::Artifact,
        required_fields: &["ok", "folder_path"],
    },
    CapabilityContract {
        tool: "work_takeover_capability_reuse_probe",
        side_effect: SideEffect::None,
        scope: "-",
        reversible: true,
        verification: VerificationStrategy::TerminalReceipt,
        required_fields: &["available", "capability"],
    },
    CapabilityContract {
        tool: "work_takeover_evidence_start",
        side_effect: SideEffect::LocalWrite,
        scope: "本地 work_takeover_evidence 表",
        reversible: true,
        verification: VerificationStrategy::StateDiff,
        required_fields: &["ok", "workflow_id", "task_id"],
    },
    CapabilityContract {
        tool: "work_takeover_evidence_update",
        side_effect: SideEffect::LocalWrite,
        scope: "本地 work_takeover_evidence 表",
        reversible: true,
        verification: VerificationStrategy::StateDiff,
        required_fields: &["ok", "workflow_id", "status"],
    },
    CapabilityContract {
        tool: "work_takeover_evidence_status",
        side_effect: SideEffect::LocalRead,
        scope: "本地 work_takeover_evidence 表",
        reversible: true,
        verification: VerificationStrategy::TerminalReceipt,
        required_fields: &["workflow_id", "status", "evidence"],
    },
    CapabilityContract {
        tool: "work_takeover_evidence_promote",
        side_effect: SideEffect::LocalWrite,
        scope: "本地 work_takeover_evidence 表",
        reversible: true,
        verification: VerificationStrategy::StateDiff,
        required_fields: &["ok", "workflow_id", "status"],
    },
    CapabilityContract {
        tool: "work_takeover_evidence_list",
        side_effect: SideEffect::LocalRead,
        scope: "本地 work_takeover_evidence 表",
        reversible: true,
        verification: VerificationStrategy::TerminalReceipt,
        required_fields: &["count", "evidence"],
    },
    // ============ Skill（技能管理 + 市场）============
    CapabilityContract {
        tool: "skill_list",
        side_effect: SideEffect::LocalRead,
        scope: "本地技能目录",
        reversible: true,
        verification: VerificationStrategy::TerminalReceipt,
        required_fields: &["count", "skills"],
    },
    CapabilityContract {
        tool: "skill_get",
        side_effect: SideEffect::LocalRead,
        scope: "本地技能目录",
        reversible: true,
        verification: VerificationStrategy::TerminalReceipt,
        required_fields: &["skill"],
    },
    CapabilityContract {
        tool: "skill_toggle",
        side_effect: SideEffect::LocalWrite,
        scope: "本地技能启用状态",
        reversible: true,
        verification: VerificationStrategy::StateDiff,
        required_fields: &["ok", "id", "enabled"],
    },
    CapabilityContract {
        // 安装引入新代码；skill 回读证明它已落盘并被加载
        tool: "skill_install",
        side_effect: SideEffect::Installation,
        scope: "本地技能目录",
        reversible: true,
        verification: VerificationStrategy::StateDiff,
        required_fields: &["ok", "status", "skill"],
    },
    CapabilityContract {
        tool: "skill_uninstall",
        side_effect: SideEffect::Installation,
        scope: "本地技能目录",
        reversible: false,
        verification: VerificationStrategy::StateDiff,
        required_fields: &["ok", "status", "id"],
    },
    CapabilityContract {
        tool: "skill_marketplace_search",
        side_effect: SideEffect::LocalRead,
        scope: "内置官方技能目录",
        reversible: true,
        verification: VerificationStrategy::TerminalReceipt,
        required_fields: &["query", "count", "skills"],
    },
    CapabilityContract {
        // 只接受官方目录里存在的 skillId，不接受任意目录
        tool: "skill_marketplace_install",
        side_effect: SideEffect::Installation,
        scope: "本地技能目录",
        reversible: true,
        verification: VerificationStrategy::StateDiff,
        required_fields: &["ok", "status", "skill_id", "skill"],
    },
    // ============ Code Execution（JS 沙箱）============
    CapabilityContract {
        // 隔离堆，无 fs/network/require；唯一副作用是临时分配一块可回收的堆内存
        tool: "code_execution",
        side_effect: SideEffect::LocalStateChange,
        scope: "隔离 JS 引擎堆（可回收）",
        reversible: true,
        verification: VerificationStrategy::TerminalReceipt,
        required_fields: &["ok", "status", "output", "result"],
    },
    // ============ Extension（扩展注册表）============
    CapabilityContract {
        tool: "extension_list",
        side_effect: SideEffect::LocalRead,
        scope: "本地 extensions 表",
        reversible: true,
        verification: VerificationStrategy::TerminalReceipt,
        required_fields: &["count", "extensions"],
    },
    CapabilityContract {
        // 安装物化官方技能 + 记清单/回执；skill 物化结果回读证明完成
        tool: "extension_install",
        side_effect: SideEffect::Installation,
        scope: "本地 extensions 表 + 技能目录",
        reversible: true,
        verification: VerificationStrategy::StateDiff,
        required_fields: &["ok", "status", "extension"],
    },
    CapabilityContract {
        tool: "extension_disable",
        side_effect: SideEffect::LocalWrite,
        scope: "本地 extensions.status",
        reversible: true,
        verification: VerificationStrategy::StateDiff,
        required_fields: &["ok", "status", "extension"],
    },
    CapabilityContract {
        tool: "extension_enable",
        side_effect: SideEffect::LocalWrite,
        scope: "本地 extensions.status",
        reversible: true,
        verification: VerificationStrategy::StateDiff,
        required_fields: &["ok", "status", "extension"],
    },
    CapabilityContract {
        tool: "extension_receipts",
        side_effect: SideEffect::LocalRead,
        scope: "本地 extension_receipts 表",
        reversible: true,
        verification: VerificationStrategy::TerminalReceipt,
        required_fields: &["count", "receipts"],
    },
    // ============ Office 增量修改 ============
    CapabilityContract {
        // paragraphs_after 与 text_preview 都是写回后重新打开文件得出的：
        // 只报"写成功"没法证明改的是这一份文档的正文。
        tool: "modify_docx",
        side_effect: SideEffect::LocalWrite,
        scope: "本地 docx 文件（就地改写正文条目）",
        reversible: false,
        verification: VerificationStrategy::StateDiff,
        required_fields: &["path", "paragraphs_after", "size_bytes"],
    },
    CapabilityContract {
        // all_verified 是逐单元格回读比对的结果
        tool: "modify_xlsx",
        side_effect: SideEffect::LocalWrite,
        scope: "本地 xlsx 文件（就地改写工作表条目）",
        reversible: false,
        verification: VerificationStrategy::StateDiff,
        required_fields: &["path", "cells_written", "all_verified", "size_bytes"],
    },
    // ============ PDF ============
    CapabilityContract {
        // has_text_layer 是"这份 PDF 到底有没有文字层"的判据：
        // 抽出空文本不是失败，但没有它就没法区分"空文档"和"扫描件"。
        tool: "read_pdf",
        side_effect: SideEffect::LocalRead,
        scope: "本地 pdf 文件",
        reversible: true,
        verification: VerificationStrategy::TerminalReceipt,
        required_fields: &["path", "total_pages", "pages_read", "has_text_layer"],
    },
    CapabilityContract {
        tool: "pdf_to_text",
        side_effect: SideEffect::LocalWrite,
        scope: "本地 txt 文件",
        reversible: true,
        verification: VerificationStrategy::StateDiff,
        required_fields: &["output", "pages", "chars", "bytes"],
    },
    CapabilityContract {
        tool: "pdf_info",
        side_effect: SideEffect::LocalRead,
        scope: "本地 pdf 文件",
        reversible: true,
        verification: VerificationStrategy::TerminalReceipt,
        required_fields: &["path", "page_count", "encrypted"],
    },
    CapabilityContract {
        tool: "merge_pdf",
        side_effect: SideEffect::LocalWrite,
        scope: "本地 pdf 文件",
        reversible: true,
        verification: VerificationStrategy::Artifact,
        required_fields: &["output", "total_pages", "size_bytes"],
    },
    CapabilityContract {
        // pages 是回读生成物得到的页数：0 页说明只是写出了一个空壳
        tool: "create_pdf",
        side_effect: SideEffect::LocalWrite,
        scope: "本地 pdf 文件",
        reversible: true,
        verification: VerificationStrategy::Artifact,
        required_fields: &["path", "pages", "size_bytes"],
    },
    CapabilityContract {
        tool: "convert_to_pdf",
        side_effect: SideEffect::LocalWrite,
        scope: "本地 pdf 文件",
        reversible: true,
        verification: VerificationStrategy::Artifact,
        required_fields: &["output", "engine", "pages", "size_bytes"],
    },
    // ============ OCR ============
    CapabilityContract {
        // line_count 是引擎回传的行数：0 行说明这张图里没认出字，
        // 和"识别失败"是两件事，必须能分开。
        tool: "ocr_image_file",
        side_effect: SideEffect::LocalRead,
        scope: "本地图片文件",
        reversible: true,
        verification: VerificationStrategy::TerminalReceipt,
        required_fields: &["path", "language", "line_count", "text"],
    },
    CapabilityContract {
        // 截图会落到临时目录，算一次本地写
        tool: "ocr_screen",
        side_effect: SideEffect::LocalWrite,
        scope: "主屏幕 + 临时 png",
        reversible: true,
        verification: VerificationStrategy::TerminalReceipt,
        required_fields: &["line_count", "chars"],
    },
    CapabilityContract {
        tool: "ocr_languages",
        side_effect: SideEffect::None,
        scope: "-",
        reversible: true,
        verification: VerificationStrategy::TerminalReceipt,
        required_fields: &["count", "languages"],
    },
    // ============ Git ============
    CapabilityContract {
        tool: "git_status",
        side_effect: SideEffect::LocalRead,
        scope: "本地 git 仓库",
        reversible: true,
        verification: VerificationStrategy::TerminalReceipt,
        required_fields: &["repo", "clean", "changed_entries"],
    },
    CapabilityContract {
        tool: "git_diff",
        side_effect: SideEffect::LocalRead,
        scope: "本地 git 仓库",
        reversible: true,
        verification: VerificationStrategy::TerminalReceipt,
        required_fields: &["repo", "empty", "files_changed"],
    },
    CapabilityContract {
        tool: "git_log",
        side_effect: SideEffect::LocalRead,
        scope: "本地 git 仓库",
        reversible: true,
        verification: VerificationStrategy::TerminalReceipt,
        required_fields: &["repo", "count"],
    },
    CapabilityContract {
        // staged_count 是 add 之后重新查暂存区得到的：add 返回 0 不代表真进了暂存区
        tool: "git_stage",
        side_effect: SideEffect::LocalStateChange,
        scope: "本地 git 暂存区",
        reversible: true,
        verification: VerificationStrategy::StateDiff,
        required_fields: &["repo", "mode", "staged_count"],
    },
    CapabilityContract {
        // commit 是新增历史，无法用删除撤销；commit 哈希是硬证据
        tool: "git_commit",
        side_effect: SideEffect::LocalStateChange,
        scope: "本地 git 仓库历史",
        reversible: false,
        verification: VerificationStrategy::StateDiff,
        required_fields: &["repo", "commit", "message"],
    },
    // ============ Knowledge ============
    CapabilityContract {
        // indexed / unchanged 由逐文件索引结果累加得出，scanned 是实际扫到的文件数
        tool: "index_document",
        side_effect: SideEffect::LocalWrite,
        scope: "本地知识库索引",
        reversible: true,
        verification: VerificationStrategy::StateDiff,
        required_fields: &["path", "scanned", "indexed", "unchanged"],
    },
    CapabilityContract {
        tool: "search_knowledge",
        side_effect: SideEffect::LocalRead,
        scope: "本地知识库索引",
        reversible: true,
        verification: VerificationStrategy::TerminalReceipt,
        required_fields: &["query", "count", "hits"],
    },
    CapabilityContract {
        tool: "list_knowledge",
        side_effect: SideEffect::LocalRead,
        scope: "本地知识库索引",
        reversible: true,
        verification: VerificationStrategy::TerminalReceipt,
        required_fields: &["count", "total_documents"],
    },
    CapabilityContract {
        // removed 表示库里确有该路径；false 不是失败，而是"本来就没索引过"
        tool: "forget_document",
        side_effect: SideEffect::LocalWrite,
        scope: "本地知识库索引（不动磁盘文件）",
        reversible: true,
        verification: VerificationStrategy::StateDiff,
        required_fields: &["path", "removed"],
    },
    // ============ Browser ============
    CapabilityContract {
        tool: "browser_open",
        side_effect: SideEffect::LocalStateChange,
        scope: "受控浏览器实例",
        reversible: true,
        verification: VerificationStrategy::None,
        required_fields: &[],
    },
    CapabilityContract {
        tool: "browser_close",
        side_effect: SideEffect::LocalStateChange,
        scope: "受控浏览器实例",
        reversible: false,
        verification: VerificationStrategy::ProviderAck,
        required_fields: &["success"],
    },
    CapabilityContract {
        tool: "browser_navigate",
        side_effect: SideEffect::NetworkRead,
        scope: "受控浏览器标签",
        reversible: true,
        verification: VerificationStrategy::ProviderAck,
        required_fields: &["success", "url", "title"],
    },
    CapabilityContract {
        tool: "browser_screenshot",
        side_effect: SideEffect::LocalWrite,
        scope: "临时目录",
        reversible: true,
        verification: VerificationStrategy::Artifact,
        required_fields: &["success", "path"],
    },
    CapabilityContract {
        tool: "browser_get_content",
        side_effect: SideEffect::NetworkRead,
        scope: "当前页面",
        reversible: true,
        verification: VerificationStrategy::TerminalReceipt,
        required_fields: &["success", "content"],
    },
    CapabilityContract {
        tool: "browser_get_title",
        side_effect: SideEffect::NetworkRead,
        scope: "当前页面",
        reversible: true,
        verification: VerificationStrategy::TerminalReceipt,
        required_fields: &["success", "title", "url"],
    },
    CapabilityContract {
        tool: "browser_click",
        side_effect: SideEffect::LocalStateChange,
        scope: "当前页面",
        reversible: true,
        verification: VerificationStrategy::ProviderAck,
        required_fields: &["success", "clicked"],
    },
    CapabilityContract {
        tool: "browser_type",
        side_effect: SideEffect::LocalStateChange,
        scope: "当前页面输入框",
        reversible: true,
        verification: VerificationStrategy::ProviderAck,
        required_fields: &["success", "selector", "chars_typed"],
    },
    CapabilityContract {
        tool: "browser_scroll",
        side_effect: SideEffect::LocalStateChange,
        scope: "当前页面",
        reversible: true,
        verification: VerificationStrategy::TerminalReceipt,
        required_fields: &["success", "direction"],
    },
    CapabilityContract {
        tool: "browser_wait",
        side_effect: SideEffect::LocalStateChange,
        scope: "当前页面",
        reversible: true,
        verification: VerificationStrategy::None,
        required_fields: &[],
    },
    CapabilityContract {
        tool: "browser_execute_js",
        side_effect: SideEffect::LocalStateChange,
        scope: "当前页面脚本上下文",
        reversible: false,
        verification: VerificationStrategy::None,
        required_fields: &[],
    },
    CapabilityContract {
        tool: "browser_list_tabs",
        side_effect: SideEffect::LocalRead,
        scope: "受控浏览器标签",
        reversible: true,
        verification: VerificationStrategy::None,
        required_fields: &[],
    },
    CapabilityContract {
        tool: "browser_switch_tab",
        side_effect: SideEffect::LocalStateChange,
        scope: "受控浏览器标签",
        reversible: true,
        verification: VerificationStrategy::ProviderAck,
        required_fields: &["success"],
    },
    // ============ P3 运维簇 ============
    CapabilityContract {
        tool: "usage_get_summary",
        side_effect: SideEffect::LocalRead,
        scope: "本地回执账本与审计日志",
        reversible: true,
        verification: VerificationStrategy::TerminalReceipt,
        required_fields: &["total_calls", "by_tool"],
    },
    CapabilityContract {
        tool: "model_configuration_get",
        side_effect: SideEffect::LocalRead,
        scope: "本地 settings 表（凭据键掩码）",
        reversible: true,
        verification: VerificationStrategy::TerminalReceipt,
        required_fields: &["count", "settings"],
    },
    CapabilityContract {
        tool: "model_configuration_update",
        side_effect: SideEffect::LocalWrite,
        scope: "本地 settings 表（白名单键）",
        reversible: true,
        verification: VerificationStrategy::StateDiff,
        required_fields: &["ok", "key", "value"],
    },
    CapabilityContract {
        tool: "model_configuration_test",
        side_effect: SideEffect::NetworkRead,
        scope: "ollama / sidecar 本地服务",
        reversible: true,
        verification: VerificationStrategy::TerminalReceipt,
        required_fields: &["provider", "reachable"],
    },
    CapabilityContract {
        tool: "sleep_status",
        side_effect: SideEffect::LocalRead,
        scope: "本地 settings 表",
        reversible: true,
        verification: VerificationStrategy::TerminalReceipt,
        required_fields: &["mode"],
    },
    CapabilityContract {
        tool: "sleep_cycle",
        side_effect: SideEffect::LocalStateChange,
        scope: "本地 settings 表（休眠标记）",
        reversible: true,
        verification: VerificationStrategy::StateDiff,
        required_fields: &["ok", "mode"],
    },
    CapabilityContract {
        tool: "knowledge_file_stats",
        side_effect: SideEffect::LocalRead,
        scope: "本地知识库表",
        reversible: true,
        verification: VerificationStrategy::TerminalReceipt,
        required_fields: &["documents", "total_bytes", "chunks"],
    },
    CapabilityContract {
        tool: "knowledge_coverage_report",
        side_effect: SideEffect::LocalRead,
        scope: "本地知识库表",
        reversible: true,
        verification: VerificationStrategy::TerminalReceipt,
        required_fields: &["empty_count", "chunk_tiers"],
    },
    CapabilityContract {
        tool: "runtime_work_status",
        side_effect: SideEffect::LocalRead,
        scope: "本地 agent_tasks 表",
        reversible: true,
        verification: VerificationStrategy::TerminalReceipt,
        required_fields: &["count", "tasks"],
    },
    CapabilityContract {
        tool: "runtime_work_pause",
        side_effect: SideEffect::LocalStateChange,
        scope: "本地 agent_tasks 表",
        reversible: true,
        verification: VerificationStrategy::StateDiff,
        required_fields: &["ok", "affected"],
    },
    CapabilityContract {
        tool: "runtime_work_resume",
        side_effect: SideEffect::LocalStateChange,
        scope: "本地 agent_tasks 表",
        reversible: true,
        verification: VerificationStrategy::StateDiff,
        required_fields: &["ok", "affected"],
    },
    CapabilityContract {
        tool: "runtime_work_cancel",
        side_effect: SideEffect::LocalStateChange,
        scope: "本地 agent_tasks 表",
        reversible: true,
        verification: VerificationStrategy::StateDiff,
        required_fields: &["ok", "affected"],
    },
    CapabilityContract {
        tool: "external_cli_run",
        side_effect: SideEffect::ProcessExecution,
        scope: "白名单外部命令",
        reversible: false,
        verification: VerificationStrategy::TerminalReceipt,
        required_fields: &["ok", "exit_code"],
    },
    CapabilityContract {
        tool: "external_cli_status",
        side_effect: SideEffect::LocalRead,
        scope: "本地 settings 表",
        reversible: true,
        verification: VerificationStrategy::TerminalReceipt,
        required_fields: &["has_run"],
    },
    CapabilityContract {
        tool: "external_cli_get_run",
        side_effect: SideEffect::LocalRead,
        scope: "本地 settings 表",
        reversible: true,
        verification: VerificationStrategy::TerminalReceipt,
        required_fields: &["last_run"],
    },
    // ============ P3 本地日历台账 ============
    CapabilityContract {
        tool: "calendar_create",
        side_effect: SideEffect::LocalWrite,
        scope: "本地 calendar_events 表",
        reversible: true,
        verification: VerificationStrategy::StateDiff,
        required_fields: &["ok", "id", "start_at"],
    },
    CapabilityContract {
        tool: "calendar_today",
        side_effect: SideEffect::LocalRead,
        scope: "本地 calendar_events 表",
        reversible: true,
        verification: VerificationStrategy::TerminalReceipt,
        required_fields: &["count", "items"],
    },
    CapabilityContract {
        tool: "upcoming_events",
        side_effect: SideEffect::LocalRead,
        scope: "本地 calendar_events 表",
        reversible: true,
        verification: VerificationStrategy::TerminalReceipt,
        required_fields: &["days", "count", "items"],
    },
    CapabilityContract {
        tool: "calendar_modify",
        side_effect: SideEffect::LocalWrite,
        scope: "本地 calendar_events 表",
        reversible: true,
        verification: VerificationStrategy::StateDiff,
        required_fields: &["ok", "affected"],
    },
    CapabilityContract {
        tool: "calendar_delete",
        side_effect: SideEffect::LocalWrite,
        scope: "本地 calendar_events 表",
        reversible: false,
        verification: VerificationStrategy::StateDiff,
        required_fields: &["ok", "affected"],
    },
    // ============ P3 自治策略 ============
    CapabilityContract {
        tool: "autonomy_get_policy",
        side_effect: SideEffect::LocalRead,
        scope: "本地 settings 表",
        reversible: true,
        verification: VerificationStrategy::TerminalReceipt,
        required_fields: &["policy"],
    },
    CapabilityContract {
        tool: "autonomy_update_policy",
        side_effect: SideEffect::LocalWrite,
        scope: "本地 settings 表",
        reversible: true,
        verification: VerificationStrategy::StateDiff,
        required_fields: &["ok", "policy"],
    },
    // ============ P3 文档工具 ============
    CapabilityContract {
        tool: "split_pdf",
        side_effect: SideEffect::LocalWrite,
        scope: "输出 PDF 文件",
        reversible: true,
        verification: VerificationStrategy::Artifact,
        required_fields: &["output", "extracted_pages"],
    },
    CapabilityContract {
        tool: "diff_documents",
        side_effect: SideEffect::LocalRead,
        scope: "两个本地文本文件",
        reversible: true,
        verification: VerificationStrategy::TerminalReceipt,
        required_fields: &["added", "removed", "diff"],
    },
];

/// 验证欠账表 —— 有实质副作用、但没有可靠机器判据的能力，以及为什么没有。
///
/// 这张表的存在意义是**不许沉默**：能力可以暂时没有验证策略，
/// 但必须写清"为什么没法验证"。没有理由的空白会被
/// `material_side_effects_are_either_verified_or_documented` 测试拦住。
///
/// 这些条目不是"永远不验证"的许可证——将来有了可靠判据（比如窗口操作
/// 之后重新枚举前台窗口比对），就要从这张表里移走并补上验证策略。
pub const VERIFICATION_DEBT: &[(&str, &str)] = &[
    (
        "open_file",
        "打开动作交给系统默认程序，工具只拿到 shell 受理结果，无法确认目标程序已呈现文件",
    ),
    (
        "focus_window",
        "窗口焦点由 OS 异步切换，工具只确认请求已投递，返回结构随 PowerShell 输出变化",
    ),
    (
        "minimize_window",
        "最小化由 OS 异步完成，返回结构随 PowerShell 输出变化，无可声明必填字段",
    ),
    (
        "close_window",
        "关闭请求可能被目标程序的保存确认框阻断，工具返回时无法判定窗口是否真的没了",
    ),
    (
        "launch_workspace",
        "批量启动，各应用的启动结果由自身进程决定，工具只汇总受理情况",
    ),
    (
        "browser_open",
        "浏览器进程启动后由 CDP 异步就绪，工具返回时不保证页面已可操作",
    ),
    (
        "browser_wait",
        "等待是时间语义，没有可核对的产物",
    ),
    (
        "browser_execute_js",
        "脚本返回值结构由页面决定，无法预先声明必填字段",
    ),
    (
        "send_notification",
        "toast 是否真的展示取决于专注助手与免打扰设置，提交成功不等于用户看见",
    ),
    (
        "lock_screen",
        "锁屏后桌面会话切到安全桌面，普通进程枚举不到会话状态，无可用判据",
    ),
    (
        "open_url",
        "链接由默认浏览器承接，工具只拿到 shell 受理结果，无法确认页面已打开",
    ),
    (
        "compose_mail",
        "只能确认客户端进程已启动；草稿是否真被填入由邮件客户端决定，无法回读",
    ),
];

/// 查某个工具的验证欠账理由
pub fn verification_debt(tool: &str) -> Option<&'static str> {
    VERIFICATION_DEBT
        .iter()
        .find(|(name, _)| *name == tool)
        .map(|(_, reason)| *reason)
}

/// 全部契约（自省类工具用它生成能力清单）。
pub fn contracts() -> &'static [CapabilityContract] {
    CONTRACTS
}

/// 按工具名查契约
pub fn contract_for(tool: &str) -> Option<&'static CapabilityContract> {
    CONTRACTS.iter().find(|c| c.tool == tool)
}

/// 契约覆盖率报告。
///
/// 只统计事实，不做"大概齐"：`material_without_verification` 里的每一条
/// 都是"有实质副作用、既没有验证策略、也没写清为什么"的能力，
/// 属于必须当天补掉的缺口；`material_with_verification_debt` 是已登记的
/// 已知欠账，可以排期但不能消失。
#[derive(Debug, Clone, serde::Serialize)]
pub struct CoverageReport {
    /// 已注册工具总数
    pub total_tools: usize,
    /// 已声明契约的工具数
    pub declared: usize,
    /// 已注册但没有契约的工具（必须为空）
    pub missing: Vec<String>,
    /// 有实质副作用的能力数
    pub material: usize,
    /// 实质副作用且声明了验证策略的能力数
    pub material_verified: usize,
    /// 有实质副作用、没有验证策略、也没有欠账说明的能力名（必须为空）
    pub material_without_verification: Vec<String>,
    /// 有实质副作用、没有验证策略但已登记理由的能力名
    pub material_with_verification_debt: Vec<String>,
}

impl CoverageReport {
    /// 是否全部覆盖（新增工具没登记契约时为 false）
    pub fn fully_declared(&self) -> bool {
        self.missing.is_empty()
    }
}

/// 计算覆盖率
pub fn coverage(registered_tools: &[String]) -> CoverageReport {
    let mut missing = Vec::new();
    let mut material = 0usize;
    let mut material_verified = 0usize;
    let mut material_without_verification = Vec::new();
    let mut material_with_verification_debt = Vec::new();

    for name in registered_tools {
        match contract_for(name) {
            None => missing.push(name.clone()),
            Some(c) => {
                if c.side_effect.is_material() {
                    material += 1;
                    if c.requires_verification() {
                        material_verified += 1;
                    } else if verification_debt(c.tool).is_some() {
                        material_with_verification_debt.push(c.tool.to_string());
                    } else {
                        material_without_verification.push(c.tool.to_string());
                    }
                }
            }
        }
    }

    missing.sort();
    material_without_verification.sort();
    material_with_verification_debt.sort();

    CoverageReport {
        total_tools: registered_tools.len(),
        declared: registered_tools.len() - missing.len(),
        missing,
        material,
        material_verified,
        material_without_verification,
        material_with_verification_debt,
    }
}

/// 把风险等级与副作用放在一起做一次自洽性检查。
///
/// 只检查两个方向，因为只有这两条是硬道理：
/// - **SAFE 不能有实质副作用**：否则界面上会出现"零风险地改动了东西"。
/// - **MEDIUM 以上必须有实质副作用**：否则每次都弹窗，用户很快学会闭眼点确认，
///   真正的危险操作反而被淹没。
///
/// LOW 刻意不约束：它是"做过但代价只是重做一次"的桶，open_file（用默认程序
/// 打开文档）就落在这里——它有副作用，但不该为它弹窗。用副作用类别反推 LOW
/// 的边界，只会把产品拖进"每步都确认"的泥潭。
pub fn risk_is_consistent(contract: &CapabilityContract, risk: RiskLevel) -> bool {
    match risk {
        RiskLevel::Safe => !contract.side_effect.is_material(),
        RiskLevel::Low => true,
        RiskLevel::Medium | RiskLevel::High | RiskLevel::Critical => {
            contract.side_effect.is_material()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::ToolRegistry;

    #[test]
    fn every_registered_tool_declares_a_contract() {
        let registry = ToolRegistry::default();
        let report = coverage(&registry.list_names());
        assert!(
            report.missing.is_empty(),
            "以下工具没有能力契约: {:?}",
            report.missing
        );
        assert_eq!(report.declared, report.total_tools);
    }

    #[test]
    fn contract_table_has_no_duplicates() {
        let mut names: Vec<&str> = CONTRACTS.iter().map(|c| c.tool).collect();
        let before = names.len();
        names.sort_unstable();
        names.dedup();
        assert_eq!(before, names.len(), "契约表里存在重复的工具名");
    }

    #[test]
    fn material_side_effects_declare_verification() {
        let report = coverage(&ToolRegistry::default().list_names());
        assert!(
            report.material_without_verification.is_empty(),
            "以下实质副作用能力既没有验证策略，也没登记欠账理由: {:?}",
            report.material_without_verification
        );
    }

    #[test]
    fn verification_debt_points_at_real_gaps() {
        for (tool, reason) in VERIFICATION_DEBT {
            let contract = contract_for(tool).unwrap_or_else(|| panic!("{tool} 没有契约"));
            assert!(
                contract.side_effect.is_material(),
                "{tool} 没有实质副作用，不该登记验证欠账"
            );
            assert!(
                !contract.requires_verification(),
                "{tool} 已经有验证策略了，欠账登记应删除"
            );
            assert!(!reason.trim().is_empty(), "{tool} 的欠账理由不能为空");
        }
    }

    #[test]
    fn declared_required_fields_are_not_empty_when_verification_is_on() {
        for c in CONTRACTS {
            if c.requires_verification() {
                assert!(
                    !c.required_fields.is_empty(),
                    "{} 声明了验证策略却没有必填回执字段",
                    c.tool
                );
            }
        }
    }

    #[test]
    fn risk_levels_match_side_effect_materiality() {
        let registry = ToolRegistry::default();
        for name in registry.list_names() {
            let Some(tool) = registry.get(&name) else {
                continue;
            };
            let Some(contract) = contract_for(&name) else {
                continue;
            };
            assert!(
                risk_is_consistent(contract, tool.risk_level()),
                "{} 的风险等级与副作用类别不自洽: {:?} / {}",
                name,
                tool.risk_level(),
                contract.side_effect.as_str()
            );
        }
    }
}
