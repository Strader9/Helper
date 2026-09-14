//! V19 Browser 工具集
//!
//! 13 个浏览器自动化工具，基于 CDP (Chrome DevTools Protocol)。
//! 风险等级：
//! - SAFE: browser_get_content, browser_get_title, browser_list_tabs
//! - LOW: browser_open, browser_close, browser_navigate, browser_screenshot,
//!        browser_scroll, browser_wait, browser_switch_tab
//! - MEDIUM: browser_click, browser_type
//! - HIGH: browser_execute_js

use async_trait::async_trait;
use serde_json::json;

use crate::browser::{global as browser, is_sensitive_url};
use crate::error::{AppError, AppResult};
use crate::tools::{AgentTool, RiskLevel, ToolResult};

// ============================================================
// browser_open — 打开浏览器
// ============================================================

pub struct BrowserOpenTool;

#[async_trait]
impl AgentTool for BrowserOpenTool {
    fn name(&self) -> &'static str { "browser_open" }
    fn description(&self) -> &'static str {
        "打开浏览器并访问指定 URL。自动选择系统已安装的浏览器（优先 Chrome，其次 Edge、Brave、Firefox 等），无需指定浏览器类型。如果指定的浏览器未安装，会自动回退到系统可用浏览器。"
    }
    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "url": { "type": "string", "description": "初始打开的 URL（可选，默认 about:blank）" },
                "browser": { "type": "string", "description": "浏览器类型（可选，不指定则自动选择系统默认浏览器。支持 chrome/edge/brave/firefox/chromium 等）" }
            }
        })
    }
    fn risk_level(&self) -> RiskLevel { RiskLevel::Low }

    async fn execute(&self, args: serde_json::Value) -> AppResult<ToolResult> {
        let url = args.get("url").and_then(|v| v.as_str());
        let browser_type = args.get("browser").and_then(|v| v.as_str());

        // 安全检查：敏感 URL
        if let Some(u) = url {
            if is_sensitive_url(u) {
                return Ok(ToolResult::err("SENSITIVE_URL_BLOCKED",
                    &format!("URL 包含敏感域名，已阻止: {}", u)));
            }
        }

        let tab_id = browser().open(url, browser_type).await?;
        let btype = browser().browser_type().await;
        let fallback = browser().take_fallback_message().await;

        let mut result = json!({
            "success": true,
            "tab_id": tab_id,
            "browser": btype,
            "message": "浏览器已启动"
        });

        // 如果发生了浏览器回退，在结果中说明
        if let Some(msg) = fallback {
            result["fallback"] = json!(msg);
            result["message"] = json!(format!("浏览器已启动（{}）", msg));
        }

        Ok(ToolResult::ok(result))
    }
}

// ============================================================
// browser_close — 关闭浏览器
// ============================================================

pub struct BrowserCloseTool;

#[async_trait]
impl AgentTool for BrowserCloseTool {
    fn name(&self) -> &'static str { "browser_close" }
    fn description(&self) -> &'static str {
        "关闭浏览器（结束整个浏览器进程），或关闭指定标签页。"
    }
    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "tab_id": { "type": "string", "description": "要关闭的标签页 ID（可选，不填则关闭整个浏览器）" }
            }
        })
    }
    fn risk_level(&self) -> RiskLevel { RiskLevel::Low }

    async fn execute(&self, args: serde_json::Value) -> AppResult<ToolResult> {
        let tab_id = args.get("tab_id").and_then(|v| v.as_str());

        if let Some(tid) = tab_id {
            browser().close_tab(Some(tid)).await?;
            Ok(ToolResult::ok(json!({ "success": true, "closed_tab": tid })))
        } else {
            browser().close().await?;
            Ok(ToolResult::ok(json!({ "success": true, "message": "浏览器已关闭" })))
        }
    }
}

// ============================================================
// browser_navigate — 导航到 URL
// ============================================================

pub struct BrowserNavigateTool;

#[async_trait]
impl AgentTool for BrowserNavigateTool {
    fn name(&self) -> &'static str { "browser_navigate" }
    fn description(&self) -> &'static str {
        "在当前标签页导航到指定 URL。需要先调用 browser_open 启动浏览器。"
    }
    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "url": { "type": "string", "description": "目标 URL" }
            },
            "required": ["url"]
        })
    }
    fn risk_level(&self) -> RiskLevel { RiskLevel::Low }

    async fn execute(&self, args: serde_json::Value) -> AppResult<ToolResult> {
        let url = args.get("url").and_then(|v| v.as_str())
            .ok_or_else(|| AppError::InvalidArgument("url is required".to_string()))?;

        if is_sensitive_url(url) {
            return Ok(ToolResult::err("SENSITIVE_URL_BLOCKED",
                &format!("URL 包含敏感域名，已阻止: {}", url)));
        }

        browser().ensure_running().await?;

        browser().navigate(url).await?;
        let (title, current_url) = browser().get_title().await?;

        Ok(ToolResult::ok(json!({
            "success": true,
            "url": current_url,
            "title": title
        })))
    }
}

// ============================================================
// browser_screenshot — 截图
// ============================================================

pub struct BrowserScreenshotTool;

#[async_trait]
impl AgentTool for BrowserScreenshotTool {
    fn name(&self) -> &'static str { "browser_screenshot" }
    fn description(&self) -> &'static str {
        "截取当前页面的全屏截图，保存为 PNG 文件到临时目录，返回文件路径。"
    }
    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "filename": { "type": "string", "description": "保存文件名（可选，默认自动生成）" }
            }
        })
    }
    fn risk_level(&self) -> RiskLevel { RiskLevel::Low }

    async fn execute(&self, args: serde_json::Value) -> AppResult<ToolResult> {
        browser().ensure_running().await?;

        let bytes = browser().screenshot().await?;

        let filename = args.get("filename").and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .unwrap_or_else(|| format!("browser_screenshot_{}.png", chrono::Utc::now().timestamp()));
        let path = std::env::temp_dir().join(&filename);
        std::fs::write(&path, &bytes)?;

        Ok(ToolResult::ok(json!({
            "success": true,
            "path": path.to_string_lossy().to_string(),
            "size_bytes": bytes.len()
        })))
    }
}

// ============================================================
// browser_get_content — 获取页面内容
// ============================================================

pub struct BrowserGetContentTool;

#[async_trait]
impl AgentTool for BrowserGetContentTool {
    fn name(&self) -> &'static str { "browser_get_content" }
    fn description(&self) -> &'static str {
        "获取当前页面的 HTML 源码或纯文本内容。用于读取网页信息。"
    }
    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "as_text": { "type": "boolean", "description": "true 返回纯文本，false 返回 HTML（默认 false）" }
            }
        })
    }
    fn risk_level(&self) -> RiskLevel { RiskLevel::Safe }

    async fn execute(&self, args: serde_json::Value) -> AppResult<ToolResult> {
        browser().ensure_running().await?;

        let as_text = args.get("as_text").and_then(|v| v.as_bool()).unwrap_or(false);
        let content = browser().get_content(as_text).await?;

        // 截断超长内容（最多 8000 字符）
        let truncated = if content.len() > 8000 {
            format!("{}... [已截断，共 {} 字符]", &content[..8000], content.len())
        } else {
            content
        };

        Ok(ToolResult::ok(json!({
            "success": true,
            "content": truncated,
            "is_text": as_text
        })))
    }
}

// ============================================================
// browser_get_title — 获取标题和 URL
// ============================================================

pub struct BrowserGetTitleTool;

#[async_trait]
impl AgentTool for BrowserGetTitleTool {
    fn name(&self) -> &'static str { "browser_get_title" }
    fn description(&self) -> &'static str {
        "获取当前页面的标题和 URL。"
    }
    fn parameters(&self) -> serde_json::Value {
        json!({ "type": "object", "properties": {} })
    }
    fn risk_level(&self) -> RiskLevel { RiskLevel::Safe }

    async fn execute(&self, _args: serde_json::Value) -> AppResult<ToolResult> {
        browser().ensure_running().await?;

        let (title, url) = browser().get_title().await?;
        Ok(ToolResult::ok(json!({ "success": true, "title": title, "url": url })))
    }
}

// ============================================================
// browser_click — 点击元素
// ============================================================

pub struct BrowserClickTool;

#[async_trait]
impl AgentTool for BrowserClickTool {
    fn name(&self) -> &'static str { "browser_click" }
    fn description(&self) -> &'static str {
        "点击页面元素。可以通过 CSS 选择器（如 'button.submit'）或坐标 (x, y) 点击。"
    }
    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "selector": { "type": "string", "description": "CSS 选择器（与 x/y 二选一）" },
                "x": { "type": "number", "description": "点击 X 坐标（与 selector 二选一）" },
                "y": { "type": "number", "description": "点击 Y 坐标（与 selector 二选一）" }
            }
        })
    }
    fn risk_level(&self) -> RiskLevel { RiskLevel::Medium }

    async fn execute(&self, args: serde_json::Value) -> AppResult<ToolResult> {
        browser().ensure_running().await?;

        let selector = args.get("selector").and_then(|v| v.as_str());
        let x = args.get("x").and_then(|v| v.as_f64());
        let y = args.get("y").and_then(|v| v.as_f64());

        browser().click(selector, x, y).await?;

        Ok(ToolResult::ok(json!({
            "success": true,
            "clicked": selector.unwrap_or("coordinate")
        })))
    }
}

// ============================================================
// browser_type — 输入文本
// ============================================================

pub struct BrowserTypeTool;

#[async_trait]
impl AgentTool for BrowserTypeTool {
    fn name(&self) -> &'static str { "browser_type" }
    fn description(&self) -> &'static str {
        "在指定输入框中输入文本。需要 CSS 选择器定位输入框。"
    }
    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "selector": { "type": "string", "description": "输入框的 CSS 选择器" },
                "text": { "type": "string", "description": "要输入的文本" }
            },
            "required": ["selector", "text"]
        })
    }
    fn risk_level(&self) -> RiskLevel { RiskLevel::Medium }

    async fn execute(&self, args: serde_json::Value) -> AppResult<ToolResult> {
        browser().ensure_running().await?;

        let selector = args.get("selector").and_then(|v| v.as_str())
            .ok_or_else(|| AppError::InvalidArgument("selector is required".to_string()))?;
        let text = args.get("text").and_then(|v| v.as_str())
            .ok_or_else(|| AppError::InvalidArgument("text is required".to_string()))?;

        browser().type_text(selector, text).await?;

        Ok(ToolResult::ok(json!({
            "success": true,
            "selector": selector,
            "chars_typed": text.chars().count()
        })))
    }
}

// ============================================================
// browser_scroll — 滚动页面
// ============================================================

pub struct BrowserScrollTool;

#[async_trait]
impl AgentTool for BrowserScrollTool {
    fn name(&self) -> &'static str { "browser_scroll" }
    fn description(&self) -> &'static str {
        "滚动页面，支持 up/down/left/right 四个方向。"
    }
    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "direction": { "type": "string", "enum": ["up", "down", "left", "right"], "description": "滚动方向（默认 down）" },
                "amount": { "type": "integer", "description": "滚动像素量（默认 500）" }
            }
        })
    }
    fn risk_level(&self) -> RiskLevel { RiskLevel::Low }

    async fn execute(&self, args: serde_json::Value) -> AppResult<ToolResult> {
        browser().ensure_running().await?;

        let direction = args.get("direction").and_then(|v| v.as_str()).unwrap_or("down");
        let amount = args.get("amount").and_then(|v| v.as_i64());

        browser().scroll(direction, amount).await?;

        Ok(ToolResult::ok(json!({ "success": true, "direction": direction })))
    }
}

// ============================================================
// browser_wait — 等待
// ============================================================

pub struct BrowserWaitTool;

#[async_trait]
impl AgentTool for BrowserWaitTool {
    fn name(&self) -> &'static str { "browser_wait" }
    fn description(&self) -> &'static str {
        "等待元素出现（CSS 选择器）或等待指定毫秒数。用于页面加载后等待动态内容。"
    }
    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "selector": { "type": "string", "description": "等待出现的 CSS 选择器（可选）" },
                "timeout": { "type": "integer", "description": "超时毫秒数（默认 5000）" }
            }
        })
    }
    fn risk_level(&self) -> RiskLevel { RiskLevel::Low }

    async fn execute(&self, args: serde_json::Value) -> AppResult<ToolResult> {
        browser().ensure_running().await?;

        let selector = args.get("selector").and_then(|v| v.as_str());
        let timeout_ms = args.get("timeout").and_then(|v| v.as_u64());

        let found = browser().wait(selector, timeout_ms).await?;

        Ok(ToolResult::ok(json!({
            "success": true,
            "element_found": found,
            "waited_for": selector.unwrap_or("fixed_time")
        })))
    }
}

// ============================================================
// browser_execute_js — 执行 JavaScript
// ============================================================

pub struct BrowserExecuteJsTool;

#[async_trait]
impl AgentTool for BrowserExecuteJsTool {
    fn name(&self) -> &'static str { "browser_execute_js" }
    fn description(&self) -> &'static str {
        "在当前页面执行 JavaScript 代码并返回结果。高风险操作，需权限确认。"
    }
    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "code": { "type": "string", "description": "要执行的 JavaScript 代码" }
            },
            "required": ["code"]
        })
    }
    fn risk_level(&self) -> RiskLevel { RiskLevel::High }

    async fn execute(&self, args: serde_json::Value) -> AppResult<ToolResult> {
        browser().ensure_running().await?;

        let code = args.get("code").and_then(|v| v.as_str())
            .ok_or_else(|| AppError::InvalidArgument("code is required".to_string()))?;

        let result = browser().execute_js(code).await?;

        Ok(ToolResult::ok(json!({
            "success": true,
            "result": result
        })))
    }
}

// ============================================================
// browser_list_tabs — 列出标签页
// ============================================================

pub struct BrowserListTabsTool;

#[async_trait]
impl AgentTool for BrowserListTabsTool {
    fn name(&self) -> &'static str { "browser_list_tabs" }
    fn description(&self) -> &'static str {
        "列出浏览器所有打开的标签页（ID、标题、URL）。"
    }
    fn parameters(&self) -> serde_json::Value {
        json!({ "type": "object", "properties": {} })
    }
    fn risk_level(&self) -> RiskLevel { RiskLevel::Safe }

    async fn execute(&self, _args: serde_json::Value) -> AppResult<ToolResult> {
        browser().ensure_running().await?;

        let tabs = browser().list_tabs().await?;
        let tabs_json: Vec<serde_json::Value> = tabs.iter().map(|t| json!({
            "id": t.id,
            "title": t.title,
            "url": t.url
        })).collect();

        Ok(ToolResult::ok(json!({ "success": true, "tabs": tabs_json, "count": tabs_json.len() })))
    }
}

// ============================================================
// browser_switch_tab — 切换标签页
// ============================================================

pub struct BrowserSwitchTabTool;

#[async_trait]
impl AgentTool for BrowserSwitchTabTool {
    fn name(&self) -> &'static str { "browser_switch_tab" }
    fn description(&self) -> &'static str {
        "切换到指定标签页（通过 tab_id）。"
    }
    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "tab_id": { "type": "string", "description": "目标标签页 ID" }
            },
            "required": ["tab_id"]
        })
    }
    fn risk_level(&self) -> RiskLevel { RiskLevel::Low }

    async fn execute(&self, args: serde_json::Value) -> AppResult<ToolResult> {
        browser().ensure_running().await?;

        let tab_id = args.get("tab_id").and_then(|v| v.as_str())
            .ok_or_else(|| AppError::InvalidArgument("tab_id is required".to_string()))?;

        browser().switch_tab(tab_id).await?;

        Ok(ToolResult::ok(json!({ "success": true, "switched_to": tab_id })))
    }
}
