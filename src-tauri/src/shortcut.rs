//! 全局快捷键注册。
//!
//! 放在 lib 而不是 bin：启动注册（bin 侧）和设置热切换（lib 侧的命令）
//! 都要用它。留在 bin 里的话 lib 访问不到，只能复制一份，两份逻辑迟早漂移。

use tauri_plugin_global_shortcut::GlobalShortcutExt;

/// 用户填的键被占用时依次尝试的备用键。
pub const FALLBACK_SHORTCUTS: [&str; 3] = ["Ctrl+Alt+Space", "Ctrl+Shift+A", "Alt+Shift+Space"];

/// 兜底快捷键：用户填的键连解析都过不去时用它。
pub const DEFAULT_SHORTCUT: &str = "Ctrl+Shift+Space";

/// 注册全局快捷键，返回**真正生效**的那一个。
///
/// `previous` 是当前已占用的快捷键：热切换时要先注销，否则用户改完快捷键后
/// 旧键仍挂在系统里——表现为"按新键没反应、按旧键还能唤起"，看起来像设置
/// 没生效，实际是两个都还在。
///
/// 返回值可能是用户填的键，也可能是回退链里挑出来的，调用方据此决定是否
/// 提示"你填的键被占用了，已改用 X"。
pub fn register_global_shortcut_at(
    app_handle: &tauri::AppHandle,
    shortcut_str: &str,
    previous: Option<&str>,
) -> String {
    // 先释放旧键。注销失败不阻断流程——旧键可能本来就没注册上。
    if let Some(old) = previous {
        if old != shortcut_str {
            if let Ok(old_shortcut) = old.parse::<tauri_plugin_global_shortcut::Shortcut>() {
                let _ = app_handle.global_shortcut().unregister(old_shortcut);
            }
        }
    }

    let requested = shortcut_str.trim();
    let parsed = requested
        .parse::<tauri_plugin_global_shortcut::Shortcut>()
        .or_else(|e| {
            eprintln!(
                "[GlobalShortcut] 解析 '{}' 失败: {}，使用默认 {}",
                requested, e, DEFAULT_SHORTCUT
            );
            DEFAULT_SHORTCUT.parse::<tauri_plugin_global_shortcut::Shortcut>()
        });

    let Some(shortcut) = parsed.ok() else {
        // 连默认键都解析不了，说明 Tauri 插件层面出了问题，不是用户输入的问题。
        eprintln!("[GlobalShortcut] 默认快捷键也无法解析，放弃注册");
        return requested.to_string();
    };

    if app_handle.global_shortcut().register(shortcut).is_ok() {
        eprintln!("[GlobalShortcut] 已注册: {}", requested);
        // 解析失败走了默认键的情况，返回默认键而不是用户填的乱码
        return if requested
            .parse::<tauri_plugin_global_shortcut::Shortcut>()
            .is_ok()
        {
            requested.to_string()
        } else {
            DEFAULT_SHORTCUT.to_string()
        };
    }

    eprintln!(
        "[GlobalShortcut] 注册 '{}' 失败（多半已被其他软件占用），尝试备用快捷键",
        requested
    );

    for fb in FALLBACK_SHORTCUTS {
        if let Ok(fb_shortcut) = fb.parse::<tauri_plugin_global_shortcut::Shortcut>() {
            if app_handle.global_shortcut().register(fb_shortcut).is_ok() {
                eprintln!("[GlobalShortcut] 已回退到备用快捷键: {}", fb);
                return fb.to_string();
            }
        }
    }

    eprintln!("[GlobalShortcut] 所有快捷键均注册失败，全局快捷键功能不可用");
    requested.to_string()
}
