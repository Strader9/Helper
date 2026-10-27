//! 零模型本地意图路由。
//!
//! 明确闲聊走精简对话提示，明确操作选择窄工具集；不确定请求直接交给
//! Jarvis 自适应执行器。路由阶段不进行额外模型调用。

pub use crate::tools::registry::ToolCategory;

// ============================================================
// 意图分类结果
// ============================================================

/// 意图类型
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IntentType {
    /// 纯闲聊 — 走精简对话路径
    Chat,
    /// 需要操作电脑 — 走 Jarvis 工具路径
    Action,
}

/// 完整意图分析结果
#[derive(Debug, Clone)]
pub struct IntentResult {
    pub intent: IntentType,
    /// 预测的工具类别（用于按需注入，仅 Action 时有意义）
    pub categories: Vec<ToolCategory>,
    /// 命中层级（用于调试和统计）
    pub layer: &'static str,
}

// ============================================================
// 第一层：关键词正则快速预判
// ============================================================

/// 闲聊白名单关键词（命中 → 快速路径）
/// 匹配时忽略大小写，使用词边界避免误匹配
const CHAT_KEYWORDS: &[&str] = &[
    "你好",
    "您好",
    "hi",
    "hello",
    "hey",
    "谢谢",
    "感谢",
    "thanks",
    "thank you",
    "再见",
    "拜拜",
    "bye",
    "goodbye",
    "你是谁",
    "你叫什么",
    "what are you",
    "who are you",
    "你能做什么",
    "你会什么",
    "help",
    "帮助",
    "早上好",
    "下午好",
    "晚上好",
    "good morning",
    "good afternoon",
    "good evening",
    "晚安",
    "good night",
    "讲个笑话",
    "说个笑话",
    "joke",
    "今天星期几",
    "今天几号",
    "what day",
    "你多大",
    "你几岁",
    "你喜欢什么",
    "你的爱好",
];

/// 操作黑名单关键词（命中 → 完整路径）
const ACTION_KEYWORDS: &[&str] = &[
    "打开",
    "启动",
    "运行",
    "open",
    "launch",
    "run",
    "start",
    "关闭",
    "退出",
    "kill",
    "close",
    "quit",
    "exit",
    "查看",
    "列出",
    "list",
    "show",
    "查看一下",
    "读取",
    "读",
    "read",
    "cat",
    "写入",
    "写",
    "保存",
    "write",
    "save",
    "删除",
    "删掉",
    "remove",
    "delete",
    "del",
    "rm",
    "移动",
    "剪切",
    "move",
    "mv",
    "重命名",
    "改名",
    "rename",
    "复制",
    "copy",
    "cp",
    "创建",
    "新建",
    "create",
    "new",
    "mkdir",
    "执行",
    "运行命令",
    "execute",
    "exec",
    "command",
    "cmd",
    "截图",
    "截屏",
    "screenshot",
    "snap",
    "点击",
    "click",
    "press",
    "输入",
    "打字",
    "type",
    "导航",
    "跳转",
    "navigate",
    "goto",
    "go to",
    "滚动",
    "scroll",
    "等待",
    "wait",
    "获取内容",
    "get content",
    "抓取",
    "切换标签",
    "switch tab",
    "聚焦",
    "focus",
    "最小化",
    "minimize",
    "最大化",
    "maximize",
    "进程",
    "process",
    "窗口",
    "window",
    "浏览器",
    "browser",
    "chrome",
    "edge",
    "网页",
    "网站",
    "文件",
    "file",
    "文件夹",
    "directory",
    "folder",
    "目录",
    "桌面",
    "desktop",
    "下载",
    "download",
    "文档",
    "document",
    "图片",
    "image",
    "picture",
    "photo",
    "记住",
    "记忆",
    "remember",
    "memorize",
    "我喜欢",
    "我不喜欢",
    "我习惯",
    "我偏好",
    "以后请",
    "以后都",
    "以后不要",
    "每次都",
    "回答要",
    "叫我",
    "请称呼我",
    "总是用",
    "不要用",
    "回忆",
    "recall",
    "检索记忆",
    "忘记",
    "删除记忆",
    "forget",
    "系统信息",
    "系统状态",
    "system info",
    "system status",
    "cpu",
    "内存",
    "memory",
    "磁盘",
    "disk",
    "安装",
    "install",
    "卸载",
    "uninstall",
    "重启",
    "restart",
    "reboot",
    "关机",
    "shutdown",
    "power off",
    "睡眠",
    "sleep",
    "锁定",
    "lock",
    "音量",
    "volume",
    "亮度",
    "brightness",
    "wifi",
    "网络",
    "network",
    "蓝牙",
    "bluetooth",
    "打印机",
    "printer",
    "剪贴板",
    "clipboard",
    "通知",
    "notification",
    "任务",
    "task",
    "计划",
    "schedule",
    "自动化",
    "automation",
    "定时",
    "提醒",
    "remind",
    "无人值守",
    "每天",
    "每日",
    "每周",
    "每月",
    "每小时",
    "每隔",
    "备份",
    "backup",
    "恢复",
    "restore",
    "压缩",
    "zip",
    "compress",
    "解压",
    "unzip",
    "extract",
    "搜索",
    "search",
    "find",
    "查找",
    "天气",
    "weather",
    "排序",
    "sort",
    "过滤",
    "filter",
    "统计",
    "count",
    "统计一下",
    "对比",
    "比较",
    "compare",
    "diff",
    "转换",
    "convert",
    "格式化",
    "format",
    "清理",
    "clean",
    "清理垃圾",
    "优化",
    "optimize",
    "诊断",
    "diagnose",
    "修复",
    "fix",
    "repair",
    "更新",
    "update",
    "upgrade",
    "检查",
    "check",
    "scan",
    "扫描",
    "监控",
    "monitor",
    "记录",
    "log",
    "record",
    "导出",
    "export",
    "导入",
    "import",
    "分享",
    "share",
    "发送",
    "send",
    "打印",
    "print",
    "播放",
    "play",
    "暂停",
    "pause",
    "停止",
    "stop",
    "下一首",
    "next",
    "上一首",
    "previous",
    "prev",
    "静音",
    "mute",
    "取消静音",
    "unmute",
];

/// 第一层：关键词正则快速预判
///
/// 返回 Some(IntentResult) 表示明确命中，None 表示交给同一次自适应模型调用。
pub fn quick_classify(user_content: &str) -> Option<IntentResult> {
    let lower = user_content.to_lowercase();
    let trimmed = lower.trim();

    // 空消息视为闲聊
    if trimmed.is_empty() {
        return Some(IntentResult {
            intent: IntentType::Chat,
            categories: vec![],
            layer: "layer1_empty",
        });
    }

    // 检查是否命中闲聊白名单
    let mut chat_hit = false;
    for kw in CHAT_KEYWORDS {
        if contains_word(&lower, kw) {
            chat_hit = true;
            break;
        }
    }

    // 检查是否命中操作黑名单
    let mut action_hit = false;
    let mut detected_categories: Vec<ToolCategory> = Vec::new();
    let local_file_search = is_local_file_search(&lower);
    if local_file_search {
        action_hit = true;
        detected_categories.push(ToolCategory::File);
    }

    // URL、联网检索和“先上网学习”必须直接进入工具路径。它们不一定包含
    // ACTION_KEYWORDS（例如用户只粘贴一个 URL 再说“总结”），不能依赖后续兜底。
    if is_web_intent(&lower) {
        action_hit = true;
        detected_categories.push(ToolCategory::Web);
    }
    if is_browser_navigation_intent(&lower) {
        action_hit = true;
        detected_categories.push(ToolCategory::Browser);
    }

    // 组合请求要同时开放产物工具：例如“先上网学习，再做一个视频”既需要
    // Web，也需要 Media。单一类别路由会让模型只能完成半条指令。
    if is_media_intent(&lower) {
        action_hit = true;
        detected_categories.push(ToolCategory::Media);
    }
    // 先用完整句子判断组合意图。只把命中的单个关键词交给分类器时，
    // 「每天 + 汇总」会被拆开，永远无法满足定时任务的组合条件。
    if let Some(category) = keyword_to_category(&lower) {
        detected_categories.push(category);
    }
    for kw in ACTION_KEYWORDS {
        if contains_word(&lower, kw) {
            action_hit = true;
            // 根据关键词推断类别
            if let Some(cat) = keyword_to_category(kw) {
                // “搜索/查找”单独看默认属于互联网；但完整句子明确说本地文件时，
                // 不额外注入 Web，避免把“搜索 C 盘文件”发到搜索引擎。
                if cat == ToolCategory::Web && local_file_search {
                    continue;
                }
                if !detected_categories.contains(&cat) {
                    detected_categories.push(cat);
                }
            }
            // 继续扫描，组合指令可能同时需要文件、浏览器、窗口等多个类别。
        }
    }

    match (chat_hit, action_hit) {
        // 只命中闲聊 → 快速路径
        (true, false) => Some(IntentResult {
            intent: IntentType::Chat,
            categories: vec![],
            layer: "layer1_chat",
        }),
        // 只命中操作 → 完整路径
        (false, true) => Some(IntentResult {
            intent: IntentType::Action,
            categories: detected_categories,
            layer: "layer1_action",
        }),
        // 问候与动作同时出现时动作优先，例如“你好，帮我搜索一下天气”。
        (true, true) => Some(IntentResult {
            intent: IntentType::Action,
            categories: detected_categories,
            layer: "layer1_action_with_chat",
        }),
        // 都没命中 → 自适应路径
        (false, false) => None,
    }
}

fn is_local_file_search(text: &str) -> bool {
    let has_search = ["搜索", "查找", "search", "find"]
        .iter()
        .any(|word| contains_word(text, word));
    let has_local_target = [
        "本地",
        "文件",
        "文件夹",
        "目录",
        "磁盘",
        "硬盘",
        "桌面",
        "路径",
        "盘符",
        "file",
        "folder",
        "directory",
        "disk",
        "path",
        "c:\\",
        "d:\\",
        "e:\\",
    ]
    .iter()
    .any(|word| contains_word(text, word));
    has_search && has_local_target
}

fn is_web_intent(text: &str) -> bool {
    if is_local_file_search(text) {
        return false;
    }

    let has_url = text.contains("http://") || text.contains("https://");
    let explicit_web = [
        "联网",
        "上网",
        "互联网",
        "网络搜索",
        "网页搜索",
        "搜索引擎",
        "在线搜索",
        "查资料",
        "查一下资料",
        "最新消息",
        "最新信息",
        "网址",
        "链接",
        "url",
        "web search",
        "online search",
        "research",
    ]
    .iter()
    .any(|word| contains_word(text, word));
    let general_search = ["搜索", "查找", "搜一下", "搜一搜", "search"]
        .iter()
        .any(|word| contains_word(text, word));

    has_url || explicit_web || general_search || text.contains("天气") || text.contains("weather")
}

fn is_media_intent(text: &str) -> bool {
    [
        "视频", "音频", "图片", "图像", "照片", "剪辑", "配音", "字幕", "video", "audio", "image",
        "photo", "ffmpeg",
    ]
    .iter()
    .any(|word| contains_word(text, word))
}

fn is_browser_navigation_intent(text: &str) -> bool {
    let has_web_target = text.contains("http://")
        || text.contains("https://")
        || text.contains("链接")
        || text.contains("网址");
    let wants_navigation = ["打开", "访问", "进入", "跳转", "open", "navigate", "go to"]
        .iter()
        .any(|word| contains_word(text, word));
    has_web_target && wants_navigation
}

/// 检查文本中是否包含某个词（简单子串匹配，忽略大小写）
/// 对于中文直接子串匹配，对于英文检查词边界
fn contains_word(text: &str, word: &str) -> bool {
    if word.chars().all(|c| c.is_ascii_alphanumeric() || c == ' ') {
        // 英文/数字：简单词边界检查（前后非字母数字）
        let lower_text = text.to_lowercase();
        let lower_word = word.to_lowercase();
        let bytes_text = lower_text.as_bytes();
        let bytes_word = lower_word.as_bytes();

        if bytes_word.is_empty() {
            return false;
        }

        let mut start = 0;
        while start + bytes_word.len() <= bytes_text.len() {
            if &bytes_text[start..start + bytes_word.len()] == bytes_word {
                // 检查左边界：start==0 或前一个字符不是字母数字
                let left_ok = start == 0 || !bytes_text[start - 1].is_ascii_alphanumeric();
                // 检查右边界
                let right_pos = start + bytes_word.len();
                let right_ok =
                    right_pos >= bytes_text.len() || !bytes_text[right_pos].is_ascii_alphanumeric();

                if left_ok && right_ok {
                    return true;
                }
            }
            start += 1;
        }
        false
    } else {
        // 中文：直接子串匹配
        text.contains(word)
    }
}

/// 根据关键词推断工具类别
fn keyword_to_category(keyword: &str) -> Option<ToolCategory> {
    let k = keyword.to_lowercase();

    // 联网信息检索和链接内容读取属于 Web；网页交互（点击、滚动）仍属于 Browser。
    // 必须放在 File 的“搜索/查找”之前，否则日常搜索会被误送到本地文件工具。
    if k.contains("链接") || k.contains("网址") || k.contains("url") || k.contains("http") {
        return Some(ToolCategory::Web);
    }
    if is_web_intent(&k) {
        return Some(ToolCategory::Web);
    }

    // Office 文档同理：说"读一下这个 docx"时，"读"会先命中 File 分支，
    // 而真正能干这活的工具在 Office 类。
    if k.contains("docx")
        || k.contains("xlsx")
        || k.contains("pptx")
        || k.contains("word")
        || k.contains("excel")
        || k.contains("powerpoint")
        || k.contains("ppt")
        || k.contains("幻灯片")
    {
        return Some(ToolCategory::Office);
    }

    // 自省类前置：问"你会什么"时，"会"字不命中任何分支，模型只能空口回答。
    if k.contains("你会什么")
        || k.contains("你能做什么")
        || k.contains("有什么能力")
        || k.contains("能力清单")
        || k.contains("你的版本")
        || k.contains("自检")
        || k.contains("健康检查")
        || k.contains("capabilit")
    {
        return Some(ToolCategory::Client);
    }

    // 邮件前置：说"把报告发邮件给张三"时，"发"会命中 Program 分支。
    if k.contains("邮件")
        || k.contains("邮箱")
        || k.contains("email")
        || k.contains("mail")
        || k.contains("smtp")
        || k.contains("发信")
    {
        return Some(ToolCategory::Messaging);
    }

    // 图像前置：说"把这张图缩小一半"时，"图"会先命中 File 分支，
    // 而能干活的是 Media 类。
    if k.contains("图片")
        || k.contains("图像")
        || k.contains("照片")
        || k.contains("截图")
        || k.contains("缩放")
        || k.contains("裁剪")
        || k.contains("旋转")
        || k.contains("翻转")
        || k.contains("分辨率")
        || k.contains("转格式")
        || k.contains("格式转换")
        || k.contains("视频")
        || k.contains("音频")
        || k.contains("ffmpeg")
        || k.contains("image")
        || k.contains("photo")
        || k.contains("resize")
    {
        return Some(ToolCategory::Media);
    }

    // 定时任务前置：说「每天早上九点帮我汇总」时，「每天」不命中任何分支，
    // 而「任务/计划」会命中 Agents（会话内计划）或 System，模型拿到的工具
    // 只能排一次性的步骤，做不出跨重启的常驻任务。
    //
    // 只认「时间词 + 动作词」组合：「每天用的软件」不是要建定时任务，
    // 而「每天 9 点自动检查」是。
    let recurring_time_word = ["每天", "每日", "每周", "每月", "每小时", "每隔"]
        .iter()
        .any(|w| k.contains(w));
    let scheduling_intent = k.contains("定时")
        || k.contains("自动化")
        || k.contains("提醒")
        || k.contains("remind")
        || k.contains("cron")
        || k.contains("常驻")
        || k.contains("无人值守")
        || k.contains("schedule")
        || (recurring_time_word
            && [
                "帮", "提醒", "自动", "跑", "执行", "检查", "汇总", "生成", "发",
            ]
            .iter()
            .any(|v| k.contains(v)));
    if scheduling_intent {
        return Some(ToolCategory::Automation);
    }

    // 会话内计划放在常驻调度之后，避免「计划每天九点执行」只拿到任务拆解工具。
    if k.contains("计划")
        || k.contains("步骤")
        || k.contains("拆解")
        || k.contains("待办")
        || k.contains("进度")
        || k.contains("几步")
        || k.contains("plan")
        || k.contains("todo")
    {
        return Some(ToolCategory::Agents);
    }

    // 明确、可长期复用的偏好陈述必须进入带记忆工具的完整路径。
    // 否则“以后回答短一点”会被当成普通闲聊，模型当轮答应、下一轮就忘。
    if [
        "记住",
        "别忘",
        "我喜欢",
        "我不喜欢",
        "我习惯",
        "我偏好",
        "以后请",
        "以后都",
        "以后不要",
        "每次都",
        "回答要",
        "叫我",
        "请称呼我",
        "总是用",
        "不要用",
    ]
    .iter()
    .any(|phrase| k.contains(phrase))
    {
        return Some(ToolCategory::Memory);
    }

    // Program 类：打开/启动/运行/关闭/退出/安装/卸载/播放等
    if k.contains("打开")
        || k.contains("启动")
        || k.contains("运行")
        || k.contains("开启")
        || k.contains("launch")
        || k.contains("open")
        || k.contains("run")
        || k.contains("start")
        || k.contains("关闭")
        || k.contains("退出")
        || k.contains("quit")
        || k.contains("exit")
        || k.contains("install")
        || k.contains("uninstall")
        || k.contains("安装")
        || k.contains("卸载")
        || k.contains("restart")
        || k.contains("重启")
        || k.contains("reboot")
        || k.contains("播放")
        || k.contains("暂停")
        || k.contains("停止")
        || k.contains("play")
        || k.contains("pause")
        || k.contains("stop")
        || k.contains("program")
        || k.contains("app")
        || k.contains("应用")
    {
        Some(ToolCategory::Program)
    } else if k.contains("file")
        || k.contains("文件")
        || k.contains("文件夹")
        || k.contains("目录")
        || k.contains("read")
        || k.contains("write")
        || k.contains("delete")
        || k.contains("move")
        || k.contains("rename")
        || k.contains("copy")
        || k.contains("create")
        || k.contains("mkdir")
        || k.contains("list")
        || k.contains("桌面")
        || k.contains("下载")
        || k.contains("文档")
        || k.contains("压缩")
        || k.contains("解压")
        || k.contains("zip")
        || k.contains("读取")
        || k.contains("写入")
        || k.contains("删除")
        || k.contains("移动")
        || k.contains("重命名")
        || k.contains("复制")
        || k.contains("创建")
        || k.contains("新建")
        || k.contains("查看")
        || k.contains("列出")
        || k.contains("导出")
        || k.contains("导入")
        || k.contains("备份")
        || k.contains("恢复")
        || k.contains("图片")
        || k.contains("image")
        || k.contains("picture")
        || k.contains("photo")
    {
        Some(ToolCategory::File)
    } else if k.contains("process")
        || k.contains("进程")
        || k.contains("window")
        || k.contains("窗口")
        || k.contains("focus")
        || k.contains("minimize")
        || k.contains("maximize")
        || k.contains("kill")
        || k.contains("聚焦")
        || k.contains("最小化")
        || k.contains("最大化")
    {
        Some(ToolCategory::ProcessWindow)
    } else if k.contains("browser")
        || k.contains("浏览器")
        || k.contains("chrome")
        || k.contains("edge")
        || k.contains("网页")
        || k.contains("网站")
        || k.contains("天气")
        || k.contains("weather")
        || k.contains("navigate")
        || k.contains("click")
        || k.contains("type")
        || k.contains("scroll")
        || k.contains("screenshot")
        || k.contains("截图")
        || k.contains("截屏")
        || k.contains("tab")
        || k.contains("标签")
        || k.contains("点击")
        || k.contains("输入")
        || k.contains("导航")
        || k.contains("滚动")
        || k.contains("等待")
        || k.contains("wait")
        || k.contains("获取内容")
        || k.contains("切换标签")
    {
        Some(ToolCategory::Browser)
    } else if k.contains("system")
        || k.contains("系统")
        || k.contains("cpu")
        || k.contains("内存")
        || k.contains("磁盘")
        || k.contains("disk")
        || k.contains("关机")
        || k.contains("睡眠")
        || k.contains("锁定")
        || k.contains("音量")
        || k.contains("亮度")
        || k.contains("wifi")
        || k.contains("网络")
        || k.contains("蓝牙")
        || k.contains("bluetooth")
        || k.contains("打印机")
        || k.contains("printer")
        || k.contains("剪贴板")
        || k.contains("clipboard")
        || k.contains("通知")
        || k.contains("notification")
        || k.contains("任务")
        || k.contains("task")
        || k.contains("计划")
        || k.contains("schedule")
        || k.contains("清理")
        || k.contains("clean")
        || k.contains("优化")
        || k.contains("optimize")
        || k.contains("诊断")
        || k.contains("diagnose")
        || k.contains("修复")
        || k.contains("fix")
        || k.contains("更新")
        || k.contains("update")
        || k.contains("检查")
        || k.contains("check")
        || k.contains("监控")
        || k.contains("monitor")
        || k.contains("记录")
        || k.contains("log")
        || k.contains("分享")
        || k.contains("share")
        || k.contains("发送")
        || k.contains("send")
        || k.contains("打印")
        || k.contains("print")
        || k.contains("静音")
        || k.contains("mute")
        || k.contains("执行")
        || k.contains("execute")
        || k.contains("command")
        || k.contains("命令")
        || k.contains("cmd")
        || k.contains("统计")
        || k.contains("count")
        || k.contains("系统信息")
        || k.contains("系统状态")
        || k.contains("锁屏")
        || k.contains("环境变量")
        || k.contains("environment")
        || k.contains("空间")
        || k.contains("网关")
        || k.contains("ip 地址")
        || k.contains("dns")
    {
        Some(ToolCategory::System)
    } else if k.contains("知识库")
        || k.contains("索引")
        || k.contains("knowledge")
        || k.contains("资料")
        || k.contains("笔记")
        || k.contains("文档库")
    {
        Some(ToolCategory::Knowledge)
    } else if k.contains("remember")
        || k.contains("recall")
        || k.contains("forget")
        || k.contains("记忆")
        || k.contains("记住")
        || k.contains("回忆")
        || k.contains("忘记")
    {
        Some(ToolCategory::Memory)
    } else {
        None
    }
}

/// 判断不出意图时的安全工具子集。
///
/// 原先的兜底是「开放全部类别」，把 33 个工具的完整 schema（实测约 4000 token）
/// 全塞进上下文 —— 光工具就吃掉 8192 窗口的一半，剩下的空间装不下历史与记忆。
///
/// 这里刻意**不含 Browser**（13 个工具，是最大的一类）和 **Skill**：
/// - 浏览器操作需要用户明确要求（"打开网页""点一下这个按钮"），
///   用户没说时不会突然需要开浏览器；真的需要时，用户的话里会带浏览器相关词，
///   那时 `quick_classify` 会走 Action 路径并把 Browser 类别带进来。
/// - 保留 Program / File / ProcessWindow / System / Memory / General 覆盖绝大多数
///   日常操作（开关程序、读写文件、看系统状态、记忆管理）。
pub fn fallback_categories() -> Vec<ToolCategory> {
    vec![
        ToolCategory::File,
        ToolCategory::Program,
        ToolCategory::ProcessWindow,
        ToolCategory::System,
        ToolCategory::Memory,
        ToolCategory::Knowledge,
        // 未知表达里也可能是“总结这个页面”或“了解最新信息”。Web 只有两个
        // 只读工具，放入安全兜底不会引入浏览器控制等副作用。
        ToolCategory::Web,
    ]
}

// ============================================================
// 快速路径 System Prompt
// ============================================================

/// 快速路径的精简 System Prompt
///
/// 不包含工具列表，告知模型如果需要操作电脑则输出 [NEED_TOOL] 标记
pub fn fast_path_system_prompt(memory_context: &str) -> String {
    let memory_section = if memory_context.is_empty() {
        String::new()
    } else {
        format!(
            "\n## 用户适应上下文\n以下内容来自用户可查看、可修改的长期记忆。稳定偏好应默认遵守；相关经历仅在当前问题确实相关时使用。不要据此推断未记录的人格或敏感属性：\n{}\n",
            memory_context
        )
    };

    format!(
        r#"你是 E-Assisant，一个本地优先的 Windows 桌面 AI 助手。

## 当前模式：快速闲聊模式
你正在以快速模式响应用户，此模式下你**没有工具调用能力**。

## 核心规则
1. 用简洁、友好的方式回答用户的问题
2. 如果用户只是闲聊、问候、感谢、告别、问答等，直接回答
3. **重要：如果你判断用户的请求需要操作电脑（打开程序、查看文件、执行命令、浏览器操作等），不要尝试执行，而是在回复的最开头输出标记 [NEED_TOOL]，然后简单说明你需要切换到完整模式来执行操作。**
4. 不要编造你能执行操作，不要给出操作步骤让用户自己做
5. 保持对话自然，不要提及"快速模式"等技术术语

## 示例
用户："你好"
回复：你好！有什么可以帮你的吗？

用户："打开记事本"
回复：[NEED_TOOL] 好的，我来帮你打开记事本，正在切换到操作模式...

用户："今天天气怎么样"
回复：我无法直接获取天气信息，不过你可以告诉我你所在的城市，我可以帮你打开天气网站查看。{memory_section}
当前日期: {date}"#,
        memory_section = memory_section,
        date = chrono::Local::now().format("%Y-%m-%d %H:%M:%S"),
    )
}

/// 检测快速路径回复中是否包含 [NEED_TOOL] 标记
///
/// 如果包含，说明模型判断需要操作电脑，应切换到完整路径重跑
pub fn detect_need_tool(content: &str) -> bool {
    content.trim_start().starts_with("[NEED_TOOL]")
}

/// 去除 [NEED_TOOL] 标记，返回纯文本内容
pub fn strip_need_tool_marker(content: &str) -> String {
    content
        .trim_start()
        .strip_prefix("[NEED_TOOL]")
        .unwrap_or(content)
        .trim()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 路由必须优先命中"更具体的域"。
    ///
    /// 这些断言防的是同一类退化：后加的分支被前面的大词（"打开""文件""做"）
    /// 抢走，模型拿到的是错类别的工具，然后开始瞎编。
    #[test]
    fn routing_prefers_the_more_specific_domain() {
        assert_eq!(
            keyword_to_category("帮我发邮件给张三"),
            Some(ToolCategory::Messaging),
            "「发」会先命中 Program，邮件必须前置"
        );
        assert_eq!(
            keyword_to_category("把这张图片缩小一半"),
            Some(ToolCategory::Media),
            "「图」会先命中 File，图像必须前置"
        );
        assert_eq!(keyword_to_category("你会什么"), Some(ToolCategory::Client));
        assert_eq!(
            keyword_to_category("把这件事拆成几步"),
            Some(ToolCategory::Agents)
        );
        assert_eq!(
            keyword_to_category("读一下这个 docx"),
            Some(ToolCategory::Office),
            "「读」会先命中 File，Office 必须前置"
        );
        assert_eq!(
            keyword_to_category("打开这个链接"),
            Some(ToolCategory::Web),
            "「打开」会先命中 Program，链接必须前置"
        );
    }

    #[test]
    fn unrelated_keyword_falls_through_without_guessing() {
        // 认不出就别硬猜到一个类别上：宁可返回 None 让上层走通用注入
        assert_eq!(keyword_to_category("嗯"), None);
    }

    #[test]
    fn explicit_preferences_route_to_memory_tools() {
        assert_eq!(
            keyword_to_category("以后请先给结论，回答要简洁"),
            Some(ToolCategory::Memory)
        );
        assert_eq!(
            keyword_to_category("请称呼我为小林"),
            Some(ToolCategory::Memory)
        );
    }

    #[test]
    fn natural_language_automation_routes_to_scheduler_tools() {
        for prompt in [
            "每天早上九点帮我汇总昨天的工作",
            "提醒我明天下午三点开会",
            "创建一个自动化，CPU 超过 90% 就通知我",
            "计划每周一自动生成周报",
        ] {
            let result = quick_classify(prompt).expect("应识别为动作");
            assert_eq!(result.intent, IntentType::Action, "prompt={prompt}");
            assert!(
                result.categories.contains(&ToolCategory::Automation),
                "prompt={prompt}, categories={:?}",
                result.categories
            );
        }
    }

    #[test]
    fn web_search_and_pasted_links_route_to_web_tools() {
        for prompt in [
            "搜索一下今天的 AI 新闻",
            "帮我联网查一下 Rust 最新版本",
            "总结这个链接 https://example.com/article",
            "https://example.com/article",
            "你好，帮我搜索上海天气",
        ] {
            let result = quick_classify(prompt).expect("应识别为联网动作");
            assert_eq!(result.intent, IntentType::Action, "prompt={prompt}");
            assert!(
                result.categories.contains(&ToolCategory::Web),
                "prompt={prompt}, categories={:?}",
                result.categories
            );
        }
    }

    #[test]
    fn local_file_search_does_not_leak_into_web_search() {
        for prompt in [
            "搜索 C:\\Users 里的报告.docx",
            "查找本地文件",
            "find file on disk",
        ] {
            let result = quick_classify(prompt).expect("应识别为本地搜索动作");
            assert!(
                result.categories.contains(&ToolCategory::File),
                "prompt={prompt}"
            );
            assert!(
                !result.categories.contains(&ToolCategory::Web),
                "本地路径不应发到互联网，prompt={prompt}, categories={:?}",
                result.categories
            );
        }
    }

    #[test]
    fn research_before_video_gets_both_tool_domains() {
        let result = quick_classify("先上网学习低空经济的资料，然后帮我做一个短视频")
            .expect("组合请求应进入工具路径");
        assert!(result.categories.contains(&ToolCategory::Web));
        assert!(result.categories.contains(&ToolCategory::Media));
    }

    #[test]
    fn opening_a_link_exposes_browser_and_web_tools() {
        let result =
            quick_classify("打开这个链接 https://example.com").expect("打开链接应进入工具路径");
        assert!(result.categories.contains(&ToolCategory::Web));
        assert!(result.categories.contains(&ToolCategory::Browser));
    }

    #[test]
    fn need_tool_marker_is_detected_and_stripped() {
        assert!(detect_need_tool("[NEED_TOOL] 需要查一下文件"));
        assert!(!detect_need_tool("需要查一下文件"));
        assert_eq!(
            strip_need_tool_marker("[NEED_TOOL] 需要查文件"),
            "需要查文件"
        );
        // 没有标记时原样返回，不能吞掉首字符
        assert_eq!(strip_need_tool_marker("普通回复"), "普通回复");
    }
}
