//! V21: 三层精准意图识别
//!
//! 第一层：关键词正则快速预判（零延迟）
//! 第二层：轻量 LLM 意图分类（3秒超时兜底）
//! 第三层：快速路径 [NEED_TOOL] 标记兜底
//!
//! 设计原则：意图识别精准度优先，不确定走完整路径（安全兜底）

use crate::llm::{OllamaClient, OllamaMessage};
pub use crate::tools::registry::ToolCategory;
use tokio::time::{timeout, Duration};

// ============================================================
// 意图分类结果
// ============================================================

/// 意图类型
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IntentType {
    /// 纯闲聊 — 走快速路径
    Chat,
    /// 需要操作电脑 — 走完整 ReAct 路径
    Action,
    /// 不确定 — 默认走完整路径（安全兜底）
    Uncertain,
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
    "你好", "您好", "hi", "hello", "hey",
    "谢谢", "感谢", "thanks", "thank you",
    "再见", "拜拜", "bye", "goodbye",
    "你是谁", "你叫什么", "what are you", "who are you",
    "你能做什么", "你会什么", "help", "帮助",
    "早上好", "下午好", "晚上好", "good morning", "good afternoon", "good evening",
    "晚安", "good night",
    "今天天气", "天气怎么样", // 纯询问，不操作
    "讲个笑话", "说个笑话", "joke",
    "今天星期几", "今天几号", "what day",
    "你多大", "你几岁",
    "你喜欢什么", "你的爱好",
];

/// 操作黑名单关键词（命中 → 完整路径）
const ACTION_KEYWORDS: &[&str] = &[
    "打开", "启动", "运行", "open", "launch", "run", "start",
    "关闭", "退出", "kill", "close", "quit", "exit",
    "查看", "列出", "list", "show", "查看一下",
    "读取", "读", "read", "cat",
    "写入", "写", "保存", "write", "save",
    "删除", "删掉", "remove", "delete", "del", "rm",
    "移动", "剪切", "move", "mv",
    "重命名", "改名", "rename",
    "复制", "copy", "cp",
    "创建", "新建", "create", "new", "mkdir",
    "执行", "运行命令", "execute", "exec", "command", "cmd",
    "截图", "截屏", "screenshot", "snap",
    "点击", "click", "press",
    "输入", "打字", "type",
    "导航", "跳转", "navigate", "goto", "go to",
    "滚动", "scroll",
    "等待", "wait",
    "获取内容", "get content", "抓取",
    "切换标签", "switch tab",
    "聚焦", "focus",
    "最小化", "minimize",
    "最大化", "maximize",
    "进程", "process",
    "窗口", "window",
    "浏览器", "browser", "chrome", "edge", "网页", "网站",
    "文件", "file", "文件夹", "directory", "folder", "目录",
    "桌面", "desktop",
    "下载", "download",
    "文档", "document",
    "图片", "image", "picture", "photo",
    "记住", "记忆", "remember", "memorize",
    "回忆", "recall", "检索记忆",
    "忘记", "删除记忆", "forget",
    "系统信息", "系统状态", "system info", "system status",
    "cpu", "内存", "memory", "磁盘", "disk",
    "安装", "install",
    "卸载", "uninstall",
    "重启", "restart", "reboot",
    "关机", "shutdown", "power off",
    "睡眠", "sleep",
    "锁定", "lock",
    "音量", "volume",
    "亮度", "brightness",
    "wifi", "网络", "network",
    "蓝牙", "bluetooth",
    "打印机", "printer",
    "剪贴板", "clipboard",
    "通知", "notification",
    "任务", "task",
    "计划", "schedule",
    "备份", "backup",
    "恢复", "restore",
    "压缩", "zip", "compress",
    "解压", "unzip", "extract",
    "搜索", "search", "find", "查找",
    "排序", "sort",
    "过滤", "filter",
    "统计", "count", "统计一下",
    "对比", "比较", "compare", "diff",
    "转换", "convert",
    "格式化", "format",
    "清理", "clean", "清理垃圾",
    "优化", "optimize",
    "诊断", "diagnose",
    "修复", "fix", "repair",
    "更新", "update", "upgrade",
    "检查", "check", "scan", "扫描",
    "监控", "monitor",
    "记录", "log", "record",
    "导出", "export",
    "导入", "import",
    "分享", "share",
    "发送", "send",
    "打印", "print",
    "播放", "play",
    "暂停", "pause",
    "停止", "stop",
    "下一首", "next",
    "上一首", "previous", "prev",
    "静音", "mute",
    "取消静音", "unmute",
];

/// 第一层：关键词正则快速预判
///
/// 返回 Some(IntentResult) 表示明确命中，None 表示需要进入第二层
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
    for kw in ACTION_KEYWORDS {
        if contains_word(&lower, kw) {
            action_hit = true;
            // 根据关键词推断类别
            if let Some(cat) = keyword_to_category(kw) {
                if !detected_categories.contains(&cat) {
                    detected_categories.push(cat);
                }
            }
            break; // 只要命中一个操作词就判定为 Action
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
        // 都命中（冲突）→ 进入第二层
        (true, true) => None,
        // 都没命中 → 进入第二层
        (false, false) => None,
    }
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
                let left_ok = start == 0
                    || !bytes_text[start - 1].is_ascii_alphanumeric();
                // 检查右边界
                let right_pos = start + bytes_word.len();
                let right_ok = right_pos >= bytes_text.len()
                    || !bytes_text[right_pos].is_ascii_alphanumeric();

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
    // Program 类：打开/启动/运行/关闭/退出/安装/卸载/播放等
    if k.contains("打开") || k.contains("启动") || k.contains("运行") || k.contains("开启")
        || k.contains("launch") || k.contains("open") || k.contains("run") || k.contains("start")
        || k.contains("关闭") || k.contains("退出") || k.contains("quit") || k.contains("exit")
        || k.contains("install") || k.contains("uninstall") || k.contains("安装") || k.contains("卸载")
        || k.contains("restart") || k.contains("重启") || k.contains("reboot")
        || k.contains("播放") || k.contains("暂停") || k.contains("停止") || k.contains("play") || k.contains("pause") || k.contains("stop")
        || k.contains("program") || k.contains("app") || k.contains("应用")
    {
        Some(ToolCategory::Program)
    } else if k.contains("file") || k.contains("文件") || k.contains("文件夹") || k.contains("目录")
        || k.contains("read") || k.contains("write") || k.contains("delete") || k.contains("move")
        || k.contains("rename") || k.contains("copy") || k.contains("create") || k.contains("mkdir")
        || k.contains("list") || k.contains("桌面") || k.contains("下载") || k.contains("文档")
        || k.contains("压缩") || k.contains("解压") || k.contains("zip")
        || k.contains("读取") || k.contains("写入") || k.contains("删除") || k.contains("移动")
        || k.contains("重命名") || k.contains("复制") || k.contains("创建") || k.contains("新建")
        || k.contains("查看") || k.contains("列出") || k.contains("搜索") || k.contains("查找")
        || k.contains("导出") || k.contains("导入") || k.contains("备份") || k.contains("恢复")
        || k.contains("图片") || k.contains("image") || k.contains("picture") || k.contains("photo")
    {
        Some(ToolCategory::File)
    } else if k.contains("process") || k.contains("进程") || k.contains("window")
        || k.contains("窗口") || k.contains("focus") || k.contains("minimize")
        || k.contains("maximize") || k.contains("kill") || k.contains("聚焦")
        || k.contains("最小化") || k.contains("最大化")
    {
        Some(ToolCategory::ProcessWindow)
    } else if k.contains("browser") || k.contains("浏览器") || k.contains("chrome")
        || k.contains("edge") || k.contains("网页") || k.contains("网站")
        || k.contains("navigate") || k.contains("click") || k.contains("type")
        || k.contains("scroll") || k.contains("screenshot") || k.contains("截图")
        || k.contains("截屏") || k.contains("tab") || k.contains("标签")
        || k.contains("点击") || k.contains("输入") || k.contains("导航") || k.contains("滚动")
        || k.contains("等待") || k.contains("wait") || k.contains("获取内容") || k.contains("切换标签")
    {
        Some(ToolCategory::Browser)
    } else if k.contains("system") || k.contains("系统") || k.contains("cpu")
        || k.contains("内存") || k.contains("磁盘") || k.contains("disk")
        || k.contains("关机") || k.contains("睡眠") || k.contains("锁定")
        || k.contains("音量") || k.contains("亮度") || k.contains("wifi") || k.contains("网络")
        || k.contains("蓝牙") || k.contains("bluetooth") || k.contains("打印机") || k.contains("printer")
        || k.contains("剪贴板") || k.contains("clipboard") || k.contains("通知") || k.contains("notification")
        || k.contains("任务") || k.contains("task") || k.contains("计划") || k.contains("schedule")
        || k.contains("清理") || k.contains("clean") || k.contains("优化") || k.contains("optimize")
        || k.contains("诊断") || k.contains("diagnose") || k.contains("修复") || k.contains("fix")
        || k.contains("更新") || k.contains("update") || k.contains("检查") || k.contains("check")
        || k.contains("监控") || k.contains("monitor") || k.contains("记录") || k.contains("log")
        || k.contains("分享") || k.contains("share") || k.contains("发送") || k.contains("send")
        || k.contains("打印") || k.contains("print") || k.contains("静音") || k.contains("mute")
        || k.contains("execute") || k.contains("command") || k.contains("命令") || k.contains("cmd")
        || k.contains("统计") || k.contains("count") || k.contains("系统信息") || k.contains("系统状态")
    {
        Some(ToolCategory::System)
    } else if k.contains("remember") || k.contains("recall") || k.contains("forget")
        || k.contains("记忆") || k.contains("记住") || k.contains("回忆") || k.contains("忘记")
    {
        Some(ToolCategory::Memory)
    } else {
        None
    }
}

// ============================================================
// 第二层：轻量 LLM 意图分类
// ============================================================

/// 第二层：轻量 LLM 意图分类
///
/// 极简 prompt，让模型只输出 "chat" 或 "action"
/// 超时3秒或解析失败 → 默认完整路径（安全兜底）
pub async fn llm_classify(
    ollama: &OllamaClient,
    model: &str,
    user_content: &str,
    temperature: f32,
) -> IntentResult {
    let prompt = format!(
        r#"你是一个意图分类器。判断用户消息属于"闲聊"还是"操作电脑"。

闲聊：问候、感谢、告别、问答、聊天、笑话、日期询问等不需要操作电脑的内容。
操作：需要打开/关闭/查看/修改/执行任何电脑操作的内容。

用户消息："{}"

只回答一个单词：chat 或 action"#,
        user_content
    );

    let messages = vec![OllamaMessage::user(&prompt)];

    // 3秒超时兜底
    let result = timeout(
        Duration::from_secs(3),
        ollama.chat(model, messages, None, temperature),
    )
    .await;

    match result {
        Ok(Ok((content, _))) => {
            let lower = content.to_lowercase();
            if lower.contains("chat") || lower.contains("闲聊") {
                IntentResult {
                    intent: IntentType::Chat,
                    categories: vec![],
                    layer: "layer2_chat",
                }
            } else if lower.contains("action") || lower.contains("操作") {
                // 尝试从回复中提取类别
                let categories = extract_categories_from_llm(&content);
                IntentResult {
                    intent: IntentType::Action,
                    categories,
                    layer: "layer2_action",
                }
            } else {
                // 解析失败 → 安全兜底走完整路径
                IntentResult {
                    intent: IntentType::Action,
                    categories: vec![],
                    layer: "layer2_fallback",
                }
            }
        }
        // 超时或错误 → 安全兜底走完整路径
        _ => IntentResult {
            intent: IntentType::Action,
            categories: vec![],
            layer: "layer2_timeout",
        },
    }
}

/// 从 LLM 回复中尝试提取工具类别（如果模型输出了额外信息）
fn extract_categories_from_llm(content: &str) -> Vec<ToolCategory> {
    let lower = content.to_lowercase();
    let mut cats = Vec::new();

    let category_keywords = [
        ("file", ToolCategory::File),
        ("文件", ToolCategory::File),
        ("program", ToolCategory::Program),
        ("应用", ToolCategory::Program),
        ("process", ToolCategory::ProcessWindow),
        ("window", ToolCategory::ProcessWindow),
        ("进程", ToolCategory::ProcessWindow),
        ("窗口", ToolCategory::ProcessWindow),
        ("browser", ToolCategory::Browser),
        ("浏览器", ToolCategory::Browser),
        ("system", ToolCategory::System),
        ("系统", ToolCategory::System),
        ("memory", ToolCategory::Memory),
        ("记忆", ToolCategory::Memory),
    ];

    for (kw, cat) in category_keywords.iter() {
        if lower.contains(kw) && !cats.contains(cat) {
            cats.push(cat.clone());
        }
    }

    cats
}

// ============================================================
// 统一入口：三层意图识别
// ============================================================

/// 统一意图识别入口
///
/// 第一层：关键词快速预判（零延迟）
/// 第二层：LLM 分类（3秒超时）
/// 第三层：由快速路径运行时通过 [NEED_TOOL] 标记兜底
pub async fn classify_intent(
    ollama: &OllamaClient,
    model: &str,
    user_content: &str,
    temperature: f32,
) -> IntentResult {
    // 第一层：关键词快速预判
    if let Some(result) = quick_classify(user_content) {
        eprintln!(
            "[Intent] 第一层命中: {} → {:?} (categories: {:?})",
            result.layer, result.intent, result.categories
        );
        return result;
    }

    // 第二层：LLM 分类
    let result = llm_classify(ollama, model, user_content, temperature).await;
    eprintln!(
        "[Intent] 第二层结果: {} → {:?} (categories: {:?})",
        result.layer, result.intent, result.categories
    );
    result
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
            "\n## 关于用户的记忆\n以下是从长期记忆中检索到的与当前对话相关的信息，请在回答时参考：\n{}\n",
            memory_context
        )
    };

    format!(
        r#"你是 PC Guardian AI，一个本地优先的 Windows 桌面 AI 助手。

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
