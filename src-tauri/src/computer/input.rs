//! 键鼠模拟工具
//!
//! 通过 Win32 `SendInput` 直接注入输入事件。刻意不走 PowerShell：起一个
//! PowerShell 进程约 150ms，连续点击和逐字输入完全不可用。
//!
//! 约定：
//! - 坐标一律是**屏幕绝对像素**，左上角为原点；内部换算成 SendInput 要求的
//!   0..65535 归一化值（这是 SendInput 的历史遗留设计，不是 bug）。
//! - 打字走 `KEYEVENTF_UNICODE`，所以中文、emoji 都能直接输入，且不受当前
//!   输入法状态影响（不会出现"我想打 abc 结果输入法给出阿波茨"）。
//! - 本机进程若未提权，无法向提权窗口注入事件（系统 UIPI 拦截）。这种情况
//!   返回明确错误，而不是静默失败。

use serde_json::json;

use crate::error::{AppError, AppResult};
use crate::tools::{AgentTool, RiskLevel, ToolResult};
use async_trait::async_trait;

#[cfg(windows)]
pub(crate) mod win {
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, INPUT_MOUSE, KEYBDINPUT, KEYEVENTF_KEYUP,
        KEYEVENTF_UNICODE, MOUSEEVENTF_ABSOLUTE, MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP,
        MOUSEEVENTF_MIDDLEDOWN, MOUSEEVENTF_MIDDLEUP, MOUSEEVENTF_MOVE, MOUSEEVENTF_RIGHTDOWN,
        MOUSEEVENTF_RIGHTUP, MOUSEEVENTF_WHEEL, MOUSEINPUT, VIRTUAL_KEY,
    };
    use windows::Win32::UI::WindowsAndMessaging::{GetSystemMetrics, SM_CXSCREEN, SM_CYSCREEN};

    /// 主屏逻辑尺寸。SendInput 的绝对坐标以主屏为基准归一化，
    /// 所以多屏环境下副屏坐标可能落在 [0,1] 之外，属正常现象。
    pub fn screen_size() -> (i32, i32) {
        unsafe { (GetSystemMetrics(SM_CXSCREEN), GetSystemMetrics(SM_CYSCREEN)) }
    }

    fn send(inputs: &[INPUT]) -> Result<(), String> {
        if inputs.is_empty() {
            return Ok(());
        }
        let sent = unsafe { SendInput(inputs, std::mem::size_of::<INPUT>() as i32) };
        if sent == 0 {
            return Err(
                "SendInput 被系统拒绝：目标窗口可能是提权进程（UIPI 拦截），\
                 或当前会话没有可交互的输入桌面"
                    .to_string(),
            );
        }
        Ok(())
    }

    fn mouse(dx: i32, dy: i32, data: u32, flags: u32) -> INPUT {
        INPUT {
            r#type: INPUT_MOUSE,
            Anonymous: INPUT_0 {
                mi: MOUSEINPUT {
                    dx,
                    dy,
                    mouseData: data,
                    dwFlags: windows::Win32::UI::Input::KeyboardAndMouse::MOUSE_EVENT_FLAGS(flags),
                    time: 0,
                    dwExtraInfo: 0,
                },
            },
        }
    }

    fn key(vk: u16, scan: u16, flags: u32) -> INPUT {
        INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: INPUT_0 {
                ki: KEYBDINPUT {
                    wVk: VIRTUAL_KEY(vk),
                    wScan: scan,
                    dwFlags: windows::Win32::UI::Input::KeyboardAndMouse::KEYBD_EVENT_FLAGS(flags),
                    time: 0,
                    dwExtraInfo: 0,
                },
            },
        }
    }

    /// 把屏幕像素换算成绝对坐标事件
    fn move_to(x: i32, y: i32) -> Result<(), String> {
        let (w, h) = screen_size();
        if w <= 1 || h <= 1 {
            return Err("读不到屏幕尺寸，无法定位坐标".to_string());
        }
        let nx = (x.clamp(0, w - 1) as i64 * 65535 / (w - 1) as i64) as i32;
        let ny = (y.clamp(0, h - 1) as i64 * 65535 / (h - 1) as i64) as i32;
        send(&[mouse(
            nx,
            ny,
            0,
            MOUSEEVENTF_MOVE.0 | MOUSEEVENTF_ABSOLUTE.0,
        )])
    }

    /// 按键按下/抬起用哪组标志
    fn button_flags(button: &str) -> Result<(u32, u32), String> {
        let pair = match button {
            "left" => (MOUSEEVENTF_LEFTDOWN.0, MOUSEEVENTF_LEFTUP.0),
            "right" => (MOUSEEVENTF_RIGHTDOWN.0, MOUSEEVENTF_RIGHTUP.0),
            "middle" => (MOUSEEVENTF_MIDDLEDOWN.0, MOUSEEVENTF_MIDDLEUP.0),
            other => return Err(format!("不支持的鼠标键：{other}（可选 left/right/middle）")),
        };
        Ok(pair)
    }

    pub fn click_at(x: i32, y: i32, button: &str, count: u32) -> Result<(), String> {
        let (down, up) = button_flags(button)?;
        move_to(x, y)?;
        let mut inputs = Vec::with_capacity(count as usize * 2);
        for _ in 0..count {
            inputs.push(mouse(0, 0, 0, down));
            inputs.push(mouse(0, 0, 0, up));
        }
        send(&inputs)
    }

    pub fn mouse_down(button: &str) -> Result<(), String> {
        let (down, _) = button_flags(button)?;
        send(&[mouse(0, 0, 0, down)])
    }

    pub fn mouse_up(button: &str) -> Result<(), String> {
        let (_, up) = button_flags(button)?;
        send(&[mouse(0, 0, 0, up)])
    }

    pub fn move_cursor(x: i32, y: i32) -> Result<(), String> {
        move_to(x, y)
    }

    /// 读当前光标位置。生产代码里没有调用点，只给实机回环测试验证
    /// 「事件确实被系统接受并生效」用——SendInput 返回成功只代表事件入队，
    /// 不代表光标真的动了。
    #[cfg(test)]
    pub fn cursor_pos() -> Result<(i32, i32), String> {
        use windows::Win32::Foundation::POINT;
        use windows::Win32::UI::WindowsAndMessaging::GetCursorPos;
        let mut p = POINT::default();
        unsafe { GetCursorPos(&mut p) }.map_err(|e| format!("读光标位置失败：{e}"))?;
        Ok((p.x, p.y))
    }

    /// 滚轮：正值向上滚，负值向下滚，1 = 标准一格（WHEEL_DELTA=120）
    pub fn wheel(delta: i32) -> Result<(), String> {
        let raw = (delta * 120) as u32;
        send(&[mouse(0, 0, raw, MOUSEEVENTF_WHEEL.0)])
    }

    /// 逐码元输入。UTF-16 代理对会被 Windows 自动合成为完整字符，
    /// 因此中文/emoji 都能过——这是 UNICODE 标志相对 VK 方案的关键优势。
    pub fn type_text(text: &str) -> Result<(), String> {
        let mut batch: Vec<INPUT> = Vec::with_capacity(128);
        for unit in text.encode_utf16() {
            batch.push(key(0, unit, KEYEVENTF_UNICODE.0));
            batch.push(key(0, unit, KEYEVENTF_UNICODE.0 | KEYEVENTF_KEYUP.0));
            // 单批过大时中途冲刷，避免瞬时注入上千个事件把输入队列打满
            if batch.len() >= 128 {
                send(&batch)?;
                batch.clear();
            }
        }
        send(&batch)
    }

    pub fn key_down(vk: u16) -> Result<(), String> {
        send(&[key(vk, 0, 0)])
    }

    pub fn key_up(vk: u16) -> Result<(), String> {
        send(&[key(vk, 0, KEYEVENTF_KEYUP.0)])
    }

    /// 键名 → 虚拟键码。只覆盖实际会用到的键，不做全键盘映射。
    pub fn parse_key(name: &str) -> Result<u16, String> {
        let key = name.trim().to_lowercase();
        if key.is_empty() {
            return Err("按键名不能为空".to_string());
        }

        if key.chars().count() == 1 {
            let c = key.chars().next().unwrap();
            if c.is_ascii_alphabetic() {
                return Ok(c.to_ascii_uppercase() as u16);
            }
            if c.is_ascii_digit() {
                return Ok(c as u16);
            }
            // 符号键走 OEM 虚拟码
            let vk = match c {
                ' ' => 0x20,
                '+' | '=' => 0xBB,
                '-' => 0xBD,
                ',' => 0xBC,
                '.' => 0xBE,
                '/' => 0xBF,
                ';' => 0xBA,
                '\'' => 0xDE,
                '[' => 0xDB,
                ']' => 0xDD,
                '\\' => 0xDC,
                '`' => 0xC0,
                other => return Err(format!("不认识的按键：{other}")),
            };
            return Ok(vk);
        }

        let vk = match key.as_str() {
            "enter" | "return" => 0x0D,
            "tab" => 0x09,
            "esc" | "escape" => 0x1B,
            "space" => 0x20,
            "backspace" | "back" => 0x08,
            "delete" | "del" => 0x2E,
            "insert" | "ins" => 0x2D,
            "home" => 0x24,
            "end" => 0x23,
            "pageup" | "pgup" => 0x21,
            "pagedown" | "pgdn" => 0x22,
            "up" | "arrowup" => 0x26,
            "down" | "arrowdown" => 0x28,
            "left" | "arrowleft" => 0x25,
            "right" | "arrowright" => 0x27,
            "ctrl" | "control" => 0x11,
            "alt" => 0x12,
            "shift" => 0x10,
            "win" | "meta" | "super" => 0x5B,
            "capslock" => 0x14,
            "printscreen" | "prtsc" => 0x2C,
            "f1" => 0x70,
            "f2" => 0x71,
            "f3" => 0x72,
            "f4" => 0x73,
            "f5" => 0x74,
            "f6" => 0x75,
            "f7" => 0x76,
            "f8" => 0x77,
            "f9" => 0x78,
            "f10" => 0x79,
            "f11" => 0x7A,
            "f12" => 0x7B,
            other => return Err(format!("不认识的按键：{other}")),
        };
        Ok(vk)
    }
}

/// 把 "ctrl+shift+s" 这样的组合键拆成 [ctrl, shift, s]。
///
/// 允许末尾带 `+`（例如 `ctrl++` 表示 ctrl 加加号），所以不能简单按 `+` 全切。
fn split_combo(spec: &str) -> Vec<String> {
    let trimmed = spec.trim();
    if let Some(head) = trimmed.strip_suffix("++") {
        let mut parts: Vec<String> = head
            .split('+')
            .filter(|s| !s.trim().is_empty())
            .map(|s| s.trim().to_string())
            .collect();
        parts.push("+".to_string());
        return parts;
    }
    trimmed
        .split('+')
        .filter(|s| !s.trim().is_empty())
        .map(|s| s.trim().to_string())
        .collect()
}

fn mouse_button(args: &serde_json::Value) -> String {
    args.get("button")
        .and_then(|v| v.as_str())
        .unwrap_or("left")
        .to_lowercase()
}

fn require_i32(args: &serde_json::Value, key: &str) -> AppResult<i32> {
    args.get(key)
        .and_then(|v| v.as_i64())
        .map(|v| v as i32)
        .ok_or_else(|| AppError::InvalidArgument(format!("缺少参数 {key}（整数坐标）")))
}

// ============================================================
// 鼠标移动
// ============================================================

pub struct MouseMoveTool;

#[async_trait]
impl AgentTool for MouseMoveTool {
    fn name(&self) -> &'static str {
        "mouse_move"
    }

    fn description(&self) -> &'static str {
        "把鼠标移到屏幕绝对坐标 (x, y)。只移动不点击。坐标以主屏左上角为原点，\
         单位像素。要操作别的程序前，先用 focus_window 把它切到前台，否则鼠标会落在\
         被遮挡的位置上。"
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "x": {"type": "integer", "description": "横坐标（像素，主屏左上角为 0）"},
                "y": {"type": "integer", "description": "纵坐标（像素，主屏左上角为 0）"}
            },
            "required": ["x", "y"]
        })
    }

    fn risk_level(&self) -> RiskLevel {
        // 不标 Safe：契约里它登记为 desktop_control（实质副作用），
        // 而 Safe 只允许挂非实质副作用。移动光标本身无害，但把它当"无副作用"
        // 会在策略层放行后续同类操作，边界容易越走越松。
        RiskLevel::Low
    }

    async fn execute(&self, args: serde_json::Value) -> AppResult<ToolResult> {
        let x = require_i32(&args, "x")?;
        let y = require_i32(&args, "y")?;

        #[cfg(windows)]
        {
            match win::move_cursor(x, y) {
                Ok(()) => Ok(ToolResult::ok(json!({"x": x, "y": y}))),
                Err(e) => Ok(ToolResult::err("INPUT_REJECTED", &e)),
            }
        }
        #[cfg(not(windows))]
        {
            let _ = (x, y);
            Ok(ToolResult::err("UNSUPPORTED_PLATFORM", "仅 Windows 可用"))
        }
    }
}

// ============================================================
// 鼠标点击
// ============================================================

pub struct MouseClickTool;

#[async_trait]
impl AgentTool for MouseClickTool {
    fn name(&self) -> &'static str {
        "mouse_click"
    }

    fn description(&self) -> &'static str {
        "在屏幕绝对坐标处点击鼠标。可指定左/右/中键和连击次数（双击传 count=2）。\
         不给坐标时在当前鼠标位置点击。"
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "x": {"type": "integer", "description": "横坐标（像素）"},
                "y": {"type": "integer", "description": "纵坐标（像素）"},
                "button": {
                    "type": "string",
                    "enum": ["left", "right", "middle"],
                    "description": "鼠标键，默认 left"
                },
                "count": {"type": "integer", "description": "点击次数，默认 1；双击传 2"}
            },
            "required": []
        })
    }

    fn risk_level(&self) -> RiskLevel {
        RiskLevel::Low
    }

    async fn execute(&self, args: serde_json::Value) -> AppResult<ToolResult> {
        let button = mouse_button(&args);
        let count = args
            .get("count")
            .and_then(|v| v.as_i64())
            .unwrap_or(1)
            .clamp(1, 5) as u32;

        // 坐标可以不给：`None` 表示沿用当前光标位置
        let coords = match (
            args.get("x").and_then(|v| v.as_i64()),
            args.get("y").and_then(|v| v.as_i64()),
        ) {
            (Some(x), Some(y)) => Some((x as i32, y as i32)),
            _ => None,
        };

        #[cfg(windows)]
        {
            let result = match coords {
                Some((x, y)) => win::click_at(x, y, &button, count),
                None => {
                    // 没有坐标时在原地点击，重复 count 次
                    let mut outcome = Ok(());
                    for _ in 0..count {
                        outcome = win::mouse_down(&button).and_then(|_| win::mouse_up(&button));
                        if outcome.is_err() {
                            break;
                        }
                    }
                    outcome
                }
            };
            match result {
                Ok(()) => Ok(ToolResult::ok(json!({
                    "button": button,
                    "count": count,
                    "x": coords.map(|c| c.0),
                    "y": coords.map(|c| c.1),
                }))),
                Err(e) => Ok(ToolResult::err("INPUT_REJECTED", &e)),
            }
        }
        #[cfg(not(windows))]
        {
            let _ = (button, count, coords);
            Ok(ToolResult::err("UNSUPPORTED_PLATFORM", "仅 Windows 可用"))
        }
    }
}

// ============================================================
// 鼠标拖拽
// ============================================================

pub struct MouseDragTool;

#[async_trait]
impl AgentTool for MouseDragTool {
    fn name(&self) -> &'static str {
        "mouse_drag"
    }

    fn description(&self) -> &'static str {
        "按住鼠标从起点拖到终点后松开。用于拖拽文件、选中文字、拖动滚动条或窗口。\
         内部按 12 步插值移动——一次性跳过去很多程序识别不到拖拽。"
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "from_x": {"type": "integer", "description": "起点横坐标"},
                "from_y": {"type": "integer", "description": "起点纵坐标"},
                "to_x": {"type": "integer", "description": "终点横坐标"},
                "to_y": {"type": "integer", "description": "终点纵坐标"},
                "button": {
                    "type": "string",
                    "enum": ["left", "right", "middle"],
                    "description": "按住哪个键拖，默认 left"
                }
            },
            "required": ["from_x", "from_y", "to_x", "to_y"]
        })
    }

    fn risk_level(&self) -> RiskLevel {
        RiskLevel::Low
    }

    async fn execute(&self, args: serde_json::Value) -> AppResult<ToolResult> {
        let fx = require_i32(&args, "from_x")?;
        let fy = require_i32(&args, "from_y")?;
        let tx = require_i32(&args, "to_x")?;
        let ty = require_i32(&args, "to_y")?;
        let button = mouse_button(&args);

        #[cfg(windows)]
        {
            if let Err(e) = win::move_cursor(fx, fy) {
                return Ok(ToolResult::err("INPUT_REJECTED", &e));
            }
            if let Err(e) = win::mouse_down(&button) {
                return Ok(ToolResult::err("INPUT_REJECTED", &e));
            }

            const STEPS: i32 = 12;
            let mut failure: Option<String> = None;
            for i in 1..=STEPS {
                let x = fx + (tx - fx) * i / STEPS;
                let y = fy + (ty - fy) * i / STEPS;
                if let Err(e) = win::move_cursor(x, y) {
                    failure = Some(e);
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(12)).await;
            }

            // 无论中途是否失败，必须抬起——否则鼠标会一直处于按下状态，
            // 用户接下来任何点击都会变成拖拽。
            let release = win::mouse_up(&button);

            return match (failure, release) {
                (Some(e), _) => Ok(ToolResult::err("INPUT_REJECTED", &e)),
                (None, Err(e)) => Ok(ToolResult::err("INPUT_REJECTED", &e)),
                (None, Ok(())) => Ok(ToolResult::ok(json!({
                    "from": [fx, fy],
                    "to": [tx, ty],
                    "button": button,
                }))),
            };
        }
        #[cfg(not(windows))]
        {
            let _ = (fx, fy, tx, ty, button);
            Ok(ToolResult::err("UNSUPPORTED_PLATFORM", "仅 Windows 可用"))
        }
    }
}

// ============================================================
// 滚轮
// ============================================================

pub struct MouseScrollTool;

#[async_trait]
impl AgentTool for MouseScrollTool {
    fn name(&self) -> &'static str {
        "mouse_scroll"
    }

    fn description(&self) -> &'static str {
        "滚动鼠标滚轮。delta 为正向上滚、为负向下滚，1 表示标准一格 \
         （对应 Windows 的 WHEEL_DELTA=120）。滚之前一般要先 mouse_move 到目标区域。"
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "delta": {"type": "integer", "description": "滚动格数，正数向上、负数向下"}
            },
            "required": ["delta"]
        })
    }

    fn risk_level(&self) -> RiskLevel {
        // 滚动可能触发"加载更多"、改变滑块值，属于实质副作用
        RiskLevel::Low
    }

    async fn execute(&self, args: serde_json::Value) -> AppResult<ToolResult> {
        let delta = args
            .get("delta")
            .and_then(|v| v.as_i64())
            .ok_or_else(|| AppError::InvalidArgument("缺少参数 delta（整数格数）".to_string()))?
            .clamp(-50, 50) as i32;

        #[cfg(windows)]
        {
            match win::wheel(delta) {
                Ok(()) => Ok(ToolResult::ok(json!({"delta": delta}))),
                Err(e) => Ok(ToolResult::err("INPUT_REJECTED", &e)),
            }
        }
        #[cfg(not(windows))]
        {
            let _ = delta;
            Ok(ToolResult::err("UNSUPPORTED_PLATFORM", "仅 Windows 可用"))
        }
    }
}

// ============================================================
// 键盘输入文本
// ============================================================

pub struct KeyboardTypeTool;

#[async_trait]
impl AgentTool for KeyboardTypeTool {
    fn name(&self) -> &'static str {
        "keyboard_type"
    }

    fn description(&self) -> &'static str {
        "向当前焦点窗口输入一段文本（支持中文和 emoji，不受输入法影响）。\
         输入前必须先让目标输入框获得焦点——通常先 mouse_click 到输入框。\
         含换行符时按回车处理。"
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "text": {"type": "string", "description": "要输入的文本"}
            },
            "required": ["text"]
        })
    }

    fn risk_level(&self) -> RiskLevel {
        RiskLevel::Low
    }

    async fn execute(&self, args: serde_json::Value) -> AppResult<ToolResult> {
        let text = args
            .get("text")
            .and_then(|v| v.as_str())
            .ok_or_else(|| AppError::InvalidArgument("缺少参数 text".to_string()))?;

        if text.is_empty() {
            return Ok(ToolResult::err("INVALID_ARGUMENT", "text 不能为空"));
        }
        if text.chars().count() > 20000 {
            return Ok(ToolResult::err(
                "INVALID_ARGUMENT",
                "单次输入超过 2 万字符，请分段",
            ));
        }

        #[cfg(windows)]
        {
            // 换行要拆开走回车键：UNICODE 方式发 U+000A 很多控件不认
            let mut typed = 0usize;
            for (idx, segment) in text.split('\n').enumerate() {
                if idx > 0 {
                    if let Err(e) = win::key_down(0x0D).and_then(|_| win::key_up(0x0D)) {
                        return Ok(ToolResult::err("INPUT_REJECTED", &e));
                    }
                }
                if segment.is_empty() {
                    continue;
                }
                if let Err(e) = win::type_text(segment) {
                    return Ok(ToolResult::err("INPUT_REJECTED", &e));
                }
                typed += segment.chars().count();
            }
            Ok(ToolResult::ok(json!({"typed_chars": typed})))
        }
        #[cfg(not(windows))]
        {
            let _ = text;
            Ok(ToolResult::err("UNSUPPORTED_PLATFORM", "仅 Windows 可用"))
        }
    }
}

// ============================================================
// 按键 / 组合键
// ============================================================

pub struct KeyboardPressTool;

#[async_trait]
impl AgentTool for KeyboardPressTool {
    fn name(&self) -> &'static str {
        "keyboard_press"
    }

    fn description(&self) -> &'static str {
        "按下一个键或组合键。单键写 \"enter\" / \"esc\" / \"tab\" / \"f5\"，\
         组合键用加号连接写 \"ctrl+s\" / \"ctrl+shift+esc\" / \"alt+tab\"。\
         按下顺序从左到右，抬起顺序反向——与真人操作一致。"
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "key": {"type": "string", "description": "键名或组合键，例如 \"enter\"、\"ctrl+s\""}
            },
            "required": ["key"]
        })
    }

    fn risk_level(&self) -> RiskLevel {
        RiskLevel::Low
    }

    async fn execute(&self, args: serde_json::Value) -> AppResult<ToolResult> {
        let spec = args
            .get("key")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| AppError::InvalidArgument("缺少参数 key".to_string()))?;

        let parts = split_combo(spec);
        if parts.is_empty() {
            return Ok(ToolResult::err("INVALID_ARGUMENT", "组合键解析为空"));
        }

        #[cfg(windows)]
        {
            let mut codes: Vec<u16> = Vec::with_capacity(parts.len());
            for part in &parts {
                match win::parse_key(part) {
                    Ok(vk) => codes.push(vk),
                    Err(e) => return Ok(ToolResult::err("INVALID_ARGUMENT", &e)),
                }
            }

            // 依次按下
            let mut pressed = 0usize;
            for vk in &codes {
                if let Err(e) = win::key_down(*vk) {
                    // 已经按下的必须先松开，否则会卡住修饰键
                    for done in codes[..pressed].iter().rev() {
                        let _ = win::key_up(*done);
                    }
                    return Ok(ToolResult::err("INPUT_REJECTED", &e));
                }
                pressed += 1;
            }
            // 反向抬起
            for vk in codes.iter().rev() {
                if let Err(e) = win::key_up(*vk) {
                    return Ok(ToolResult::err("INPUT_REJECTED", &e));
                }
            }

            Ok(ToolResult::ok(json!({
                "key": spec,
                "parts": parts,
            })))
        }
        #[cfg(not(windows))]
        {
            let _ = (spec, parts);
            Ok(ToolResult::err("UNSUPPORTED_PLATFORM", "仅 Windows 可用"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_combo_handles_plain_and_multi() {
        assert_eq!(split_combo("enter"), vec!["enter"]);
        assert_eq!(split_combo("ctrl+s"), vec!["ctrl", "s"]);
        assert_eq!(split_combo("ctrl+shift+esc"), vec!["ctrl", "shift", "esc"]);
        // 多余空格不该变成空键名
        assert_eq!(split_combo(" ctrl + s "), vec!["ctrl", "s"]);
    }

    #[test]
    fn split_combo_handles_trailing_plus_key() {
        // "ctrl++" 是 ctrl 加加号，不能切成 ["ctrl"]
        assert_eq!(split_combo("ctrl++"), vec!["ctrl", "+"]);
    }

    #[test]
    fn split_combo_ignores_empty_input() {
        assert!(split_combo("").is_empty());
        // "+++" 末尾的 "++" 被当成转义加号，剩下的 "+" 单独成键也无意义，
        // 最终整体等价于「按加号键」——这是有意为之，不是漏判。
        assert_eq!(split_combo("+++"), vec!["+"]);
    }

    #[cfg(windows)]
    #[test]
    fn parse_key_covers_letters_digits_and_named_keys() {
        assert_eq!(win::parse_key("a").unwrap(), 0x41);
        assert_eq!(win::parse_key("Z").unwrap(), 0x5A);
        assert_eq!(win::parse_key("5").unwrap(), 0x35);
        assert_eq!(win::parse_key("enter").unwrap(), 0x0D);
        assert_eq!(win::parse_key("ESC").unwrap(), 0x1B);
        assert_eq!(win::parse_key("f12").unwrap(), 0x7B);
        // 未知键必须报错，不能悄悄退化成 0（那会发出一个空击键）
        assert!(win::parse_key("不存在的键").is_err());
        assert!(win::parse_key("hyper").is_err());
    }

    /// 实机回环：真的把光标移到屏幕中心，再读回来核对，最后还原。
    ///
    /// 跑法：`cargo test --lib real_cursor_round_trip -- --ignored`
    ///
    /// 之所以必须跑：`SendInput` 返回非零只说明事件进了系统队列，
    /// 被 UIPI 拦住、被"移动时显示窗口内容"之类的设置吞掉，它同样会返回成功。
    /// 只有读回光标坐标才能证明链路真的通。
    #[cfg(windows)]
    #[test]
    #[ignore]
    fn real_cursor_round_trip() {
        let (w, h) = win::screen_size();
        assert!(w > 100 && h > 100, "读不到合理的屏幕尺寸：{w}x{h}");

        let origin = win::cursor_pos().expect("读初始光标位置失败");
        let target = (w / 2, h / 2);
        win::move_cursor(target.0, target.1).expect("移动光标失败");

        // 系统有时会在下一次消息循环才更新位置，给一点余量
        std::thread::sleep(std::time::Duration::from_millis(80));
        let landed = win::cursor_pos().expect("读回光标位置失败");

        // 还原，别把用户的鼠标扔在屏幕中间
        let _ = win::move_cursor(origin.0, origin.1);

        assert_eq!(
            landed, target,
            "光标没落到目标点：期望 {target:?}，实际 {landed:?}"
        );
    }

    /// 实机验证键盘事件确实被系统接收。
    ///
    /// 刻意只按一下 Shift：它不产生任何字符、不改变前台应用的状态，
    /// 是唯一能验证 `SendInput` 键盘通路又不往用户窗口里打字的方式。
    /// 想验证"真的打出了字"，必须有个受控的接收端，那属于端到端测试范畴，
    /// 不放在单元测试里。
    #[cfg(windows)]
    #[test]
    #[ignore]
    fn real_key_event_accepted() {
        let vk = win::parse_key("shift").expect("解析 shift 失败");
        win::key_down(vk).expect("Shift 按下被系统拒绝");
        win::key_up(vk).expect("Shift 抬起被系统拒绝");
    }
}
