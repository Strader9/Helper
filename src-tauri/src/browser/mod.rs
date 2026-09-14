//! V19 Browser 模块 —— 基于 CDP (Chrome DevTools Protocol) 的浏览器自动化
//!
//! 方案：自实现轻量 CDP 客户端
//! - 启动 Chrome/Edge 带 --remote-debugging-port
//! - HTTP 端点用 reqwest（/json/list, /json/new, /json/close）
//! - WebSocket 用 tokio-tungstenite 发送 CDP 命令
//!
//! 不引入 chromiumoxide 等重型依赖，保持与现有 Rust 架构的轻量兼容。

use std::process::{Child, Command};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};

use serde::{Deserialize, Serialize};
use tokio::sync::{mpsc, oneshot, Mutex};
use tokio::time::{timeout, Duration};

use crate::error::{AppError, AppResult};

// ============================================================
// CDP 数据结构
// ============================================================

/// CDP 标签页信息
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CdpTarget {
    pub id: String,
    #[serde(rename = "type")]
    pub target_type: String,
    pub title: String,
    pub url: String,
    #[serde(rename = "webSocketDebuggerUrl")]
    pub ws_url: String,
}

/// CDP 请求
#[derive(Debug, Serialize)]
struct CdpRequest {
    id: u64,
    method: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    params: Option<serde_json::Value>,
}

/// CDP 响应（从 WebSocket 接收）
#[derive(Debug, Deserialize)]
struct CdpResponse {
    #[allow(dead_code)]
    id: Option<u64>,
    #[serde(default)]
    result: Option<serde_json::Value>,
    #[serde(default)]
    error: Option<CdpError>,
    #[serde(default)]
    #[allow(dead_code)]
    method: Option<String>,
    #[serde(default)]
    #[allow(dead_code)]
    params: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize)]
struct CdpError {
    #[allow(dead_code)]
    code: i64,
    message: String,
}

/// 内部命令：发送给 WS 后台任务
struct WsCommand {
    request: CdpRequest,
    response_tx: oneshot::Sender<Result<serde_json::Value, String>>,
}

// ============================================================
// 浏览器信息与路径检测
// ============================================================

/// 浏览器信息
#[derive(Debug, Clone)]
pub struct BrowserInfo {
    /// 浏览器类型标识（chrome/edge/firefox/brave/chromium 等）
    pub browser_type: String,
    /// 显示名称（"Google Chrome"、"Microsoft Edge" 等）
    pub display_name: String,
    /// 可执行文件路径
    pub path: String,
}

/// 检测系统中所有可用的浏览器，按优先级排序（Chrome → Edge → Brave → Firefox → Chromium → 其他）
pub fn detect_all_browsers() -> Vec<BrowserInfo> {
    let mut results: Vec<BrowserInfo> = Vec::new();
    let mut seen_paths = std::collections::HashSet::new();

    // 定义各浏览器的常见安装路径（按优先级）
    let browser_defs: Vec<(&str, &str, Vec<&str>)> = vec![
        ("chrome", "Google Chrome", vec![
            r"C:\Program Files\Google\Chrome\Application\chrome.exe",
            r"C:\Program Files (x86)\Google\Chrome\Application\chrome.exe",
            r"%LOCALAPPDATA%\Google\Chrome\Application\chrome.exe",
        ]),
        ("edge", "Microsoft Edge", vec![
            r"C:\Program Files (x86)\Microsoft\Edge\Application\msedge.exe",
            r"C:\Program Files\Microsoft\Edge\Application\msedge.exe",
            r"%LOCALAPPDATA%\Microsoft\Edge\Application\msedge.exe",
        ]),
        ("brave", "Brave Browser", vec![
            r"C:\Program Files\BraveSoftware\Brave-Browser\Application\brave.exe",
            r"C:\Program Files (x86)\BraveSoftware\Brave-Browser\Application\brave.exe",
            r"%LOCALAPPDATA%\BraveSoftware\Brave-Browser\Application\brave.exe",
        ]),
        ("firefox", "Mozilla Firefox", vec![
            r"C:\Program Files\Mozilla Firefox\firefox.exe",
            r"C:\Program Files (x86)\Mozilla Firefox\firefox.exe",
        ]),
        ("chromium", "Chromium", vec![
            r"C:\Program Files\Chromium\chrome.exe",
            r"C:\Program Files (x86)\Chromium\chrome.exe",
            r"%LOCALAPPDATA%\Chromium\chrome.exe",
        ]),
        ("arc", "Arc Browser", vec![
            r"%LOCALAPPDATA%\Programs\arc\Arc.exe",
            r"C:\Program Files\Arc\Arc.exe",
        ]),
        ("vivaldi", "Vivaldi", vec![
            r"C:\Program Files\Vivaldi\Application\vivaldi.exe",
            r"C:\Program Files (x86)\Vivaldi\Application\vivaldi.exe",
            r"%LOCALAPPDATA%\Vivaldi\Application\vivaldi.exe",
        ]),
        ("opera", "Opera", vec![
            r"C:\Program Files\Opera\opera.exe",
            r"C:\Program Files (x86)\Opera\opera.exe",
            r"%LOCALAPPDATA%\Programs\Opera\opera.exe",
        ]),
    ];

    for (btype, display_name, paths) in &browser_defs {
        for p in paths {
            let expanded = expand_env(p);
            if std::path::Path::new(&expanded).exists() {
                // 去重（同一物理路径不重复添加）
                let canonical = expanded.to_lowercase();
                if !seen_paths.contains(&canonical) {
                    seen_paths.insert(canonical);
                    results.push(BrowserInfo {
                        browser_type: btype.to_string(),
                        display_name: display_name.to_string(),
                        path: expanded,
                    });
                }
                break; // 该浏览器找到一个即可，不再尝试其他路径
            }
        }
    }

    results
}

/// 检测系统中可用的默认浏览器（优先级最高的那个）
/// 返回 (browser_type, path)
pub fn detect_browser_path() -> Option<(String, String)> {
    detect_all_browsers()
        .into_iter()
        .next()
        .map(|b| (b.browser_type, b.path))
}

/// 按类型查找浏览器
pub fn find_browser_by_type(browser_type: &str) -> Option<BrowserInfo> {
    detect_all_browsers()
        .into_iter()
        .find(|b| b.browser_type == browser_type)
}

/// 生成系统浏览器环境描述（用于 System Prompt 注入）
pub fn get_browser_environment_description() -> String {
    let browsers = detect_all_browsers();
    if browsers.is_empty() {
        return "当前系统未检测到任何可用浏览器".to_string();
    }
    let names: Vec<String> = browsers.iter().map(|b| b.display_name.clone()).collect();
    let default = &browsers[0];
    format!(
        "当前系统已安装的浏览器：{}（默认浏览器：{}）。浏览器工具会自动选择可用浏览器，无需指定类型。",
        names.join("、"),
        default.display_name
    )
}

fn expand_env(path: &str) -> String {
    let mut result = path.to_string();
    let mut start = 0;
    while let Some(pct1) = result[start..].find('%') {
        let abs_pct1 = start + pct1;
        if let Some(pct2) = result[abs_pct1 + 1..].find('%') {
            let abs_pct2 = abs_pct1 + 1 + pct2;
            let var_name = &result[abs_pct1 + 1..abs_pct2];
            if let Ok(val) = std::env::var(var_name) {
                result.replace_range(abs_pct1..=abs_pct2, &val);
                start = abs_pct1 + val.len();
            } else {
                start = abs_pct2 + 1;
            }
        } else {
            break;
        }
    }
    result
}

// ============================================================
// BrowserManager
// ============================================================

/// 浏览器管理器
///
/// 管理浏览器进程、CDP 连接、标签页。
/// 使用 Mutex 保护，所有操作串行化（避免 CDP 并发冲突）。
pub struct BrowserManager {
    inner: Mutex<BrowserManagerInner>,
}

struct BrowserManagerInner {
    /// 浏览器子进程
    process: Option<Child>,
    /// CDP 调试端口
    debug_port: u16,
    /// 当前活跃标签页 ID
    current_tab_id: Option<String>,
    /// 当前标签页的 WS 命令发送端
    ws_tx: Option<mpsc::UnboundedSender<WsCommand>>,
    /// WS 后台任务句柄
    ws_handle: Option<tokio::task::JoinHandle<()>>,
    /// 命令 ID 计数器
    cmd_id: Arc<AtomicU64>,
    /// 浏览器类型（chrome/edge 等）
    browser_type: String,
    /// 浏览器可执行路径
    browser_path: String,
    /// 系统中所有可用浏览器列表（按优先级排序）
    available_browsers: Vec<BrowserInfo>,
    /// 最近一次浏览器回退说明（如"未找到 Chrome，已自动使用 Edge"）
    fallback_message: Option<String>,
}

impl BrowserManager {
    /// 创建新的浏览器管理器
    pub fn new() -> Self {
        let available = detect_all_browsers();
        let (browser_type, browser_path) = available
            .first()
            .map(|b| (b.browser_type.clone(), b.path.clone()))
            .unwrap_or_else(|| ("none".to_string(), String::new()));

        Self {
            inner: Mutex::new(BrowserManagerInner {
                process: None,
                debug_port: 9222,
                current_tab_id: None,
                ws_tx: None,
                ws_handle: None,
                cmd_id: Arc::new(AtomicU64::new(1)),
                browser_type,
                browser_path,
                available_browsers: available,
                fallback_message: None,
            }),
        }
    }

    /// 检查系统是否有可用浏览器
    pub async fn has_available_browser(&self) -> bool {
        let inner = self.inner.lock().await;
        !inner.available_browsers.is_empty()
    }

    /// 获取系统中所有可用浏览器列表
    pub async fn get_available_browsers(&self) -> Vec<BrowserInfo> {
        let inner = self.inner.lock().await;
        inner.available_browsers.clone()
    }

    /// 获取并清除最近一次回退消息
    pub async fn take_fallback_message(&self) -> Option<String> {
        let mut inner = self.inner.lock().await;
        inner.fallback_message.take()
    }

    /// 检查浏览器是否正在运行
    pub async fn is_running(&self) -> bool {
        let inner = self.inner.lock().await;
        inner.process.is_some()
    }

    /// 确保浏览器正在运行，如果未启动则自动启动默认浏览器
    /// 用于其他浏览器工具（navigate/screenshot/click 等）被调用时自动启动
    pub async fn ensure_running(&self) -> AppResult<()> {
        if self.is_running().await {
            return Ok(());
        }
        eprintln!("[Browser] 浏览器未启动，自动启动默认浏览器...");
        self.open(None, None).await?;
        Ok(())
    }

    /// 获取当前浏览器类型
    pub async fn browser_type(&self) -> String {
        let inner = self.inner.lock().await;
        inner.browser_type.clone()
    }

    /// 启动浏览器
    ///
    /// # Arguments
    /// * `url` - 可选的初始 URL
    /// * `browser_type_override` - 可选的浏览器类型覆盖（chrome/edge/firefox 等）
    ///   如果指定的浏览器未安装，自动回退到系统可用浏览器，并记录回退消息
    pub async fn open(&self, url: Option<&str>, browser_type_override: Option<&str>) -> AppResult<String> {
        let mut inner = self.inner.lock().await;

        // 如果已在运行，直接导航
        if inner.process.is_some() {
            if let Some(tab_id) = &inner.current_tab_id {
                let tab_id_clone = tab_id.clone();
                drop(inner);
                if let Some(u) = url {
                    self.navigate(u).await?;
                }
                return Ok(tab_id_clone);
            }
        }

        // 检查是否有可用浏览器
        if inner.available_browsers.is_empty() {
            return Err(AppError::Internal(
                "未检测到任何可用浏览器，请安装 Chrome、Edge、Firefox 或其他 Chromium 内核浏览器".to_string()
            ));
        }

        // 确定浏览器路径：优先使用指定的类型，不可用时自动回退
        let (btype, bpath, fallback_msg) = if let Some(bt) = browser_type_override {
            // 尝试查找指定类型的浏览器
            let specified = inner.available_browsers.iter().find(|b| b.browser_type == bt);
            if let Some(found) = specified {
                // 指定的浏览器可用
                (found.browser_type.clone(), found.path.clone(), None)
            } else {
                // 指定的浏览器不可用，回退到默认（优先级最高的）
                let default = &inner.available_browsers[0];
                let msg = format!(
                    "未找到 {}，已自动使用 {}",
                    bt, default.display_name
                );
                eprintln!("[Browser] {}", msg);
                (default.browser_type.clone(), default.path.clone(), Some(msg))
            }
        } else {
            // 未指定，使用默认浏览器
            let default = &inner.available_browsers[0];
            (default.browser_type.clone(), default.path.clone(), None)
        };

        inner.browser_type = btype.clone();
        inner.browser_path = bpath.clone();
        inner.fallback_message = fallback_msg;

        // 临时用户数据目录（避免与用户正常浏览器冲突）
        let temp_dir = std::env::temp_dir().join(format!("pc-guardian-browser-{}", std::process::id()));
        std::fs::create_dir_all(&temp_dir).map_err(|e| AppError::Internal(format!("创建临时目录失败: {}", e)))?;

        let port = inner.debug_port;
        let initial_url = url.unwrap_or("about:blank");

        // 启动浏览器进程
        let mut cmd = Command::new(&bpath);
        cmd.arg(format!("--remote-debugging-port={}", port))
            .arg(format!("--user-data-dir={}", temp_dir.to_string_lossy()))
            .arg("--no-first-run")
            .arg("--no-default-browser-check")
            .arg("--disable-popup-blocking")
            .arg("--window-size=1280,800")
            .arg(initial_url);

        let child = cmd.spawn().map_err(|e| {
            AppError::Internal(format!("启动浏览器失败 ({}): {}", bpath, e))
        })?;

        inner.process = Some(child);

        // 等待 CDP 端点就绪
        drop(inner);
        self.wait_for_cdp_ready(port, 10).await?;

        // 获取第一个标签页或创建新标签页
        let tabs = self.list_tabs_http(port).await?;
        let tab = if let Some(first) = tabs.into_iter().next() {
            first
        } else {
            self.new_tab_http(port, initial_url).await?
        };

        let tab_id = tab.id.clone();
        let ws_url = tab.ws_url.clone();

        // 建立 WebSocket 连接
        let (ws_tx, ws_handle) = self.spawn_ws_task(&ws_url).await?;

        let mut inner = self.inner.lock().await;
        inner.current_tab_id = Some(tab_id.clone());
        inner.ws_tx = Some(ws_tx);
        inner.ws_handle = Some(ws_handle);

        Ok(tab_id)
    }

    /// 关闭浏览器
    pub async fn close(&self) -> AppResult<()> {
        let mut inner = self.inner.lock().await;

        // 关闭 WS 连接
        inner.ws_tx = None;
        if let Some(handle) = inner.ws_handle.take() {
            handle.abort();
        }

        // 终止浏览器进程
        if let Some(mut proc) = inner.process.take() {
            let _ = proc.kill();
            let _ = proc.wait();
        }

        inner.current_tab_id = None;
        Ok(())
    }

    /// 导航到 URL
    pub async fn navigate(&self, url: &str) -> AppResult<()> {
        self.send_cdp("Page.navigate", Some(serde_json::json!({ "url": url }))).await?;
        // 等待页面加载完成
        tokio::time::sleep(Duration::from_millis(500)).await;
        Ok(())
    }

    /// 获取页面标题和 URL
    pub async fn get_title(&self) -> AppResult<(String, String)> {
        let result = self.evaluate_js("JSON.stringify({title: document.title, url: location.href})").await?;
        let val: serde_json::Value = serde_json::from_str(&result)
            .map_err(|e| AppError::Internal(format!("解析标题结果失败: {}", e)))?;
        let title = val.get("title").and_then(|v| v.as_str()).unwrap_or("").to_string();
        let url = val.get("url").and_then(|v| v.as_str()).unwrap_or("").to_string();
        Ok((title, url))
    }

    /// 获取页面 HTML 内容
    pub async fn get_content(&self, as_text: bool) -> AppResult<String> {
        let js = if as_text {
            "document.body ? document.body.innerText : ''"
        } else {
            "document.documentElement ? document.documentElement.outerHTML : ''"
        };
        self.evaluate_js(js).await
    }

    /// 截取页面截图
    pub async fn screenshot(&self) -> AppResult<Vec<u8>> {
        let result = self.send_cdp("Page.captureScreenshot", Some(serde_json::json!({
            "format": "png"
        }))).await?;

        let data_b64 = result
            .get("data")
            .and_then(|v| v.as_str())
            .ok_or_else(|| AppError::Internal("截图返回数据为空".to_string()))?;

        use base64::Engine;
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(data_b64)
            .map_err(|e| AppError::Internal(format!("解码截图失败: {}", e)))?;

        Ok(bytes)
    }

    /// 点击页面元素
    pub async fn click(&self, selector: Option<&str>, x: Option<f64>, y: Option<f64>) -> AppResult<()> {
        if let Some(sel) = selector {
            // 通过 CSS 选择器点击
            let js = format!(
                r#"(function() {{
                    const el = document.querySelector('{}');
                    if (!el) return 'ELEMENT_NOT_FOUND';
                    const rect = el.getBoundingClientRect();
                    el.click();
                    return JSON.stringify({{x: rect.left + rect.width/2, y: rect.top + rect.height/2}});
                }})()"#,
                sel.replace('\'', "\\'")
            );
            let result = self.evaluate_js(&js).await?;
            if result == "ELEMENT_NOT_FOUND" {
                return Err(AppError::ToolExecution(format!("未找到元素: {}", sel)));
            }
            Ok(())
        } else if let (Some(px), Some(py)) = (x, y) {
            // 通过坐标点击
            self.send_cdp("Input.dispatchMouseEvent", Some(serde_json::json!({
                "type": "mousePressed",
                "x": px,
                "y": py,
                "button": "left",
                "clickCount": 1
            }))).await?;
            tokio::time::sleep(Duration::from_millis(50)).await;
            self.send_cdp("Input.dispatchMouseEvent", Some(serde_json::json!({
                "type": "mouseReleased",
                "x": px,
                "y": py,
                "button": "left",
                "clickCount": 1
            }))).await?;
            Ok(())
        } else {
            Err(AppError::InvalidArgument("click 需要 selector 或 x/y 参数".to_string()))
        }
    }

    /// 在输入框中输入文本
    pub async fn type_text(&self, selector: &str, text: &str) -> AppResult<()> {
        // 先聚焦元素
        let focus_js = format!(
            r#"(function() {{
                const el = document.querySelector('{}');
                if (!el) return 'ELEMENT_NOT_FOUND';
                el.focus();
                el.value = '';
                return 'OK';
            }})()"#,
            selector.replace('\'', "\\'")
        );
        let result = self.evaluate_js(&focus_js).await?;
        if result == "ELEMENT_NOT_FOUND" {
            return Err(AppError::ToolExecution(format!("未找到元素: {}", selector)));
        }

        // 通过 CDP 输入事件逐字符输入
        for ch in text.chars() {
            self.send_cdp("Input.insertText", Some(serde_json::json!({
                "text": ch.to_string()
            }))).await?;
        }

        Ok(())
    }

    /// 滚动页面
    pub async fn scroll(&self, direction: &str, amount: Option<i64>) -> AppResult<()> {
        let delta = amount.unwrap_or(500);
        let (dx, dy) = match direction {
            "up" => (0, -delta),
            "down" => (0, delta),
            "left" => (-delta, 0),
            "right" => (delta, 0),
            _ => (0, delta),
        };

        self.send_cdp("Input.dispatchMouseEvent", Some(serde_json::json!({
            "type": "mouseWheel",
            "x": 640,
            "y": 400,
            "deltaX": dx,
            "deltaY": dy
        }))).await?;
        Ok(())
    }

    /// 等待元素出现或等待指定时间
    pub async fn wait(&self, selector: Option<&str>, timeout_ms: Option<u64>) -> AppResult<bool> {
        let wait_ms = timeout_ms.unwrap_or(5000);

        if let Some(sel) = selector {
            // 轮询等待元素出现
            let start = std::time::Instant::now();
            let check_js = format!(
                "document.querySelector('{}') ? 'EXISTS' : 'NOT_FOUND'",
                sel.replace('\'', "\\'")
            );
            while start.elapsed().as_millis() < wait_ms as u128 {
                let result = self.evaluate_js(&check_js).await?;
                if result == "EXISTS" {
                    return Ok(true);
                }
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
            Ok(false)
        } else {
            // 等待指定时间
            tokio::time::sleep(Duration::from_millis(wait_ms)).await;
            Ok(true)
        }
    }

    /// 执行 JavaScript
    pub async fn execute_js(&self, code: &str) -> AppResult<String> {
        self.evaluate_js(code).await
    }

    /// 列出所有标签页
    pub async fn list_tabs(&self) -> AppResult<Vec<CdpTarget>> {
        let inner = self.inner.lock().await;
        let port = inner.debug_port;
        drop(inner);
        self.list_tabs_http(port).await
    }

    /// 切换到指定标签页
    pub async fn switch_tab(&self, tab_id: &str) -> AppResult<()> {
        let tabs = self.list_tabs().await?;
        let tab = tabs
            .iter()
            .find(|t| t.id == tab_id)
            .ok_or_else(|| AppError::ToolExecution(format!("标签页不存在: {}", tab_id)))?;

        let ws_url = tab.ws_url.clone();
        let (ws_tx, ws_handle) = self.spawn_ws_task(&ws_url).await?;

        let mut inner = self.inner.lock().await;
        // 关闭旧连接
        inner.ws_tx = None;
        if let Some(h) = inner.ws_handle.take() {
            h.abort();
        }
        inner.current_tab_id = Some(tab_id.to_string());
        inner.ws_tx = Some(ws_tx);
        inner.ws_handle = Some(ws_handle);

        Ok(())
    }

    /// 关闭指定标签页
    pub async fn close_tab(&self, tab_id: Option<&str>) -> AppResult<()> {
        let inner = self.inner.lock().await;
        let port = inner.debug_port;
        let target = tab_id
            .map(|s| s.to_string())
            .unwrap_or_else(|| inner.current_tab_id.clone().unwrap_or_default());
        drop(inner);

        let client = reqwest::Client::new();
        let _ = client
            .get(format!("http://localhost:{}/json/close/{}", port, target))
            .send()
            .await;

        // 如果关闭的是当前标签页，清理状态
        let mut inner = self.inner.lock().await;
        if inner.current_tab_id.as_deref() == Some(target.as_str()) {
            inner.ws_tx = None;
            if let Some(h) = inner.ws_handle.take() {
                h.abort();
            }
            inner.current_tab_id = None;
        }

        Ok(())
    }

    // ============================================================
    // 内部方法
    // ============================================================

    /// 等待 CDP 端点就绪
    async fn wait_for_cdp_ready(&self, port: u16, max_retries: u32) -> AppResult<()> {
        let client = reqwest::Client::new();
        for i in 0..max_retries {
            match client.get(format!("http://localhost:{}/json/version", port)).send().await {
                Ok(resp) if resp.status().is_success() => return Ok(()),
                _ => {
                    tokio::time::sleep(Duration::from_millis(500)).await;
                }
            }
            eprintln!("[Browser] CDP 端点未就绪，重试 {}/{}", i + 1, max_retries);
        }
        Err(AppError::Internal("CDP 端点启动超时".to_string()))
    }

    /// HTTP: 列出标签页
    async fn list_tabs_http(&self, port: u16) -> AppResult<Vec<CdpTarget>> {
        let client = reqwest::Client::new();
        let resp = client
            .get(format!("http://localhost:{}/json/list", port))
            .send()
            .await
            .map_err(|e| AppError::Network(e))?;

        let tabs: Vec<CdpTarget> = resp
            .json()
            .await
            .map_err(|e| AppError::Internal(format!("解析标签页列表失败: {}", e)))?;

        Ok(tabs.into_iter().filter(|t| t.target_type == "page").collect())
    }

    /// HTTP: 创建新标签页
    async fn new_tab_http(&self, port: u16, url: &str) -> AppResult<CdpTarget> {
        let client = reqwest::Client::new();
        let resp = client
            .put(format!("http://localhost:{}/json/new?{}", port, url))
            .send()
            .await
            .map_err(|e| AppError::Network(e))?;

        let tab: CdpTarget = resp
            .json()
            .await
            .map_err(|e| AppError::Internal(format!("创建标签页失败: {}", e)))?;

        Ok(tab)
    }

    /// 启动 WebSocket 后台任务
    async fn spawn_ws_task(
        &self,
        ws_url: &str,
    ) -> AppResult<(mpsc::UnboundedSender<WsCommand>, tokio::task::JoinHandle<()>)> {
        use futures_util::{SinkExt, StreamExt};
        use tokio_tungstenite::tungstenite::Message;

        let (ws_stream, _) = tokio_tungstenite::connect_async(ws_url)
            .await
            .map_err(|e| AppError::Internal(format!("WebSocket 连接失败: {}", e)))?;

        let (mut write, mut read) = ws_stream.split();
        let (tx, mut rx) = mpsc::unbounded_channel::<WsCommand>();

        // 响应路由：id -> oneshot sender
        let pending = Arc::new(Mutex::new(std::collections::HashMap::<u64, oneshot::Sender<Result<serde_json::Value, String>>>::new()));

        let pending_clone = pending.clone();

        // 发送任务
        let send_handle = tokio::spawn(async move {
            while let Some(cmd) = rx.recv().await {
                let json = match serde_json::to_string(&cmd.request) {
                    Ok(s) => s,
                    Err(e) => {
                        let _ = cmd.response_tx.send(Err(format!("序列化失败: {}", e)));
                        continue;
                    }
                };
                {
                    let mut map = pending_clone.lock().await;
                    map.insert(cmd.request.id, cmd.response_tx);
                }
                if write.send(Message::Text(json.into())).await.is_err() {
                    let mut map = pending_clone.lock().await;
                    if let Some(tx) = map.remove(&cmd.request.id) {
                        let _ = tx.send(Err("WebSocket 发送失败".to_string()));
                    }
                    break;
                }
            }
        });

        // 接收任务
        let pending_recv = pending.clone();
        let recv_handle = tokio::spawn(async move {
            while let Some(msg) = read.next().await {
                match msg {
                    Ok(Message::Text(text)) => {
                        if let Ok(resp) = serde_json::from_str::<CdpResponse>(&text) {
                            if let Some(id) = resp.id {
                                let mut map = pending_recv.lock().await;
                                if let Some(tx) = map.remove(&id) {
                                    if let Some(err) = resp.error {
                                        let _ = tx.send(Err(err.message));
                                    } else {
                                        let _ = tx.send(Ok(resp.result.unwrap_or(serde_json::Value::Null)));
                                    }
                                }
                            }
                            // 事件（无 id）暂时忽略
                        }
                    }
                    Ok(Message::Close(_)) => break,
                    Err(_) => break,
                    _ => {}
                }
            }
            // 清理所有 pending
            let mut map = pending_recv.lock().await;
            for (_, tx) in map.drain() {
                let _ = tx.send(Err("WebSocket 连接已关闭".to_string()));
            }
        });

        // 合并句柄（两个任务任一结束都算结束）
        let merged_handle = tokio::spawn(async move {
            tokio::select! {
                _ = send_handle => {},
                _ = recv_handle => {},
            }
        });

        Ok((tx, merged_handle))
    }

    /// 发送 CDP 命令并等待响应
    async fn send_cdp(&self, method: &str, params: Option<serde_json::Value>) -> AppResult<serde_json::Value> {
        let (cmd_id, ws_tx_opt) = {
            let inner = self.inner.lock().await;
            if inner.ws_tx.is_none() {
                return Err(AppError::Internal("浏览器未启动或 WebSocket 未连接".to_string()));
            }
            let id = inner.cmd_id.fetch_add(1, Ordering::SeqCst);
            (id, inner.ws_tx.clone())
        };

        let ws_tx = ws_tx_opt.ok_or_else(|| AppError::Internal("WebSocket 未连接".to_string()))?;

        let (resp_tx, resp_rx) = oneshot::channel();
        let request = CdpRequest {
            id: cmd_id,
            method: method.to_string(),
            params,
        };

        ws_tx
            .send(WsCommand {
                request,
                response_tx: resp_tx,
            })
            .map_err(|_| AppError::Internal("WebSocket 任务已结束".to_string()))?;

        // 超时 15 秒
        let result = timeout(Duration::from_secs(15), resp_rx)
            .await
            .map_err(|_| AppError::Internal("CDP 命令超时".to_string()))?
            .map_err(|_| AppError::Internal("响应通道已关闭".to_string()))?;

        match result {
            Ok(val) => Ok(val),
            Err(e) => Err(AppError::ToolExecution(format!("CDP 错误: {}", e))),
        }
    }

    /// 执行 JavaScript 并返回结果字符串
    async fn evaluate_js(&self, expression: &str) -> AppResult<String> {
        let result = self.send_cdp("Runtime.evaluate", Some(serde_json::json!({
            "expression": expression,
            "returnByValue": true,
            "awaitPromise": true
        }))).await?;

        // 解析返回值
        if let Some(obj) = result.get("result") {
            if let Some(val) = obj.get("value") {
                return Ok(match val {
                    serde_json::Value::String(s) => s.clone(),
                    other => other.to_string(),
                });
            }
            if let Some(desc) = obj.get("description") {
                return Ok(desc.as_str().unwrap_or("").to_string());
            }
        }
        if let Some(exc) = result.get("exceptionDetails") {
            let text = exc.get("text").and_then(|v| v.as_str()).unwrap_or("JS 异常");
            return Err(AppError::ToolExecution(format!("JS 执行异常: {}", text)));
        }

        Ok("undefined".to_string())
    }
}

impl Default for BrowserManager {
    fn default() -> Self {
        Self::new()
    }
}

// ============================================================
// URL 黑名单检查（安全）
// ============================================================

/// 检查 URL 是否在敏感域名黑名单中（银行、支付等）
pub fn is_sensitive_url(url: &str) -> bool {
    let lower = url.to_lowercase();
    let sensitive_keywords = [
        "bank", "pay", "alipay", "weixin", "wx.qq",
        "creditcard", "loan", "finance", "money",
        "taobao.com", "jd.com", "pinduoduo",
        "account", "login", "signin",
        "github.com/login", "google.com/accounts",
    ];
    sensitive_keywords.iter().any(|k| lower.contains(k))
}

// ============================================================
// 全局单例
// ============================================================

static BROWSER_MANAGER: OnceLock<BrowserManager> = OnceLock::new();

/// 获取全局 BrowserManager 实例
pub fn global() -> &'static BrowserManager {
    BROWSER_MANAGER.get_or_init(BrowserManager::new)
}
