//! 对话历史的分级组装与压缩。
//!
//! ## 为什么需要这个模块
//!
//! 原先 `jarvis.rs` 用一句 `take(10)` 决定喂给模型的历史：只取最近 10 条，
//! 第 11 轮之前的对话**从数据库角度就没被读进来**。这不是窗口不够，是取法太粗。
//!
//! 结果是「最久的」被当成「最不重要的」丢掉 —— 用户第一轮交代的背景
//! （"我在改 v7 分支"）会在第 11 轮时消失，问"那个分支改完了吗"模型就无从回答。
//!
//! ## 分级策略
//!
//! ```text
//! ┌─────────────────────────────────────────┐
//! │ 冻结层  system prompt + 长期记忆   永不丢 │
//! ├─────────────────────────────────────────┤
//! │ 近期层  最近 N 轮原文             保原文 │
//! ├─────────────────────────────────────────┤
//! │ 远期层  更早的对话                 压摘要 │
//! └─────────────────────────────────────────┘
//! ```
//!
//! 预算用 token 而非条数来算，因为一条长消息和一条"好的"占的空间差几个数量级。

use crate::db::conversation::ConversationMessage;
use crate::llm::OllamaMessage;

/// 冻结层与近期层之外还要留给输出的余量（token）。
///
/// 组装历史时不能把窗口占满 —— 模型还要写回答。这个值比 `num_predict`
/// 略大一些，给工具调用的参数留出空间。
pub const OUTPUT_RESERVE_TOKENS: usize = 1400;

/// 远期层压缩后允许占用的最大 token。
///
/// 摘要的价值是「记住聊过什么」而不是「复现原文」，所以压得比原文狠得多。
pub const SUMMARY_BUDGET_TOKENS: usize = 500;

/// 粗略估算一段文本的 token 数。
///
/// 中文约 1 字符 ≈ 0.6 token，英文约 4 字符 ≈ 1 token。
/// 这里按「ASCII 字符数 / 4 + 非 ASCII 字符数 × 0.6」折算。
///
/// 注意：`ContextEngine::estimate_tokens` 里有个命名与实现不符的写法
/// （把 ASCII 数叫成 chinese_chars，且用 byte len 参与计算），
/// 那是既有代码，本模块不依赖它。
pub fn estimate_tokens(text: &str) -> usize {
    let ascii = text.chars().filter(|c| c.is_ascii()).count();
    let non_ascii = text.chars().count() - ascii;
    (ascii / 4) + ((non_ascii as f64) * 0.6) as usize
}

/// 估算一组消息的总 token。
pub fn estimate_messages_tokens(messages: &[OllamaMessage]) -> usize {
    messages
        .iter()
        .map(|m| estimate_tokens(&m.content) + 8) // 8 是每条消息的角色/格式开销
        .sum()
}

/// 分级组装的结果。
pub struct AssembledHistory {
    /// 最终喂给模型的消息列表（已按预算裁剪）。
    pub messages: Vec<OllamaMessage>,
    /// 被判定为「远期」、需要压缩的历史条数（0 表示无需压缩）。
    pub pending_summary_count: usize,
}

/// 按预算把数据库里的历史裁成「近期原文 + 远期待压缩」两部分。
///
/// `available_tokens` 是留给历史的预算（调用方已扣掉工具定义、system、输出余量）。
/// 从最新往回装，装满为止；装不下的部分标记为待压缩，而不是直接丢弃。
///
/// 注意 `messages` 必须按时间正序（旧 → 新）。
pub fn assemble_history(messages: &[ConversationMessage], available_tokens: usize) -> AssembledHistory {
    if messages.is_empty() {
        return AssembledHistory {
            messages: Vec::new(),
            pending_summary_count: 0,
        };
    }

    // 从最新一条往回累加，直到超出预算
    let mut used = 0usize;
    let mut split_at = messages.len(); // split_at 之前的都是「远期」
    for (idx, msg) in messages.iter().enumerate().rev() {
        let cost = estimate_tokens(&msg.content) + 8;
        if used + cost > available_tokens {
            // 这一条装不下：它和它之前的所有条都归入远期。
            // 之所以不从中间切开，是因为切一半会留下孤立的 tool 消息
            // （role=tool 的条目如果丢了对应的 assistant tool_call，会被模型当成无效输入）。
            split_at = idx + 1;
            break;
        }
        used += cost;
        split_at = idx;
    }

    let recent = &messages[split_at..];
    let pending_summary_count = split_at;

    AssembledHistory {
        messages: convert(recent),
        pending_summary_count,
    }
}

/// 把远期消息压成一段纯文本，供摘要使用。
///
/// 只取 role=user 与 role=assistant 的内容 —— tool 消息是中间过程，
/// 对「聊过什么」没有价值，且体积大。
pub fn flatten_for_summary(messages: &[ConversationMessage]) -> String {
    let mut lines = Vec::new();
    for msg in messages {
        match msg.role.as_str() {
            "user" => {
                let text = truncate_chars(&msg.content, 300);
                lines.push(format!("用户：{}", text));
            }
            "assistant" if !msg.content.trim().is_empty() => {
                let text = truncate_chars(&msg.content, 300);
                lines.push(format!("助手：{}", text));
            }
            _ => {}
        }
    }
    lines.join("\n")
}

/// 把 `ConversationMessage` 转成模型消息。
///
/// 与 `agent::convert_messages` 同源，但这里显式跳过 role=tool —— 远期裁剪
/// 可能让 tool 消息失去配对的 assistant，喂给模型会引起格式错误。
fn convert(messages: &[ConversationMessage]) -> Vec<OllamaMessage> {
    messages
        .iter()
        .filter_map(|msg| match msg.role.as_str() {
            "user" => Some(OllamaMessage::user(&msg.content)),
            "assistant" => Some(OllamaMessage::assistant(&msg.content)),
            // tool 结果只在当轮有用，历史里不需要复现
            _ => None,
        })
        .collect()
}

/// 按字符数截断（不是字节数），避免把多字节字符切坏。
fn truncate_chars(text: &str, max_chars: usize) -> String {
    let count = text.chars().count();
    if count <= max_chars {
        return text.to_string();
    }
    let head: String = text.chars().take(max_chars).collect();
    format!("{}…（共 {} 字）", head, count)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn msg(role: &str, content: &str) -> ConversationMessage {
        ConversationMessage {
            id: 0,
            session_id: "s".to_string(),
            role: role.to_string(),
            content: content.to_string(),
            tool_calls: None,
            tool_call_id: None,
            timestamp: "2026-10-13 00:00:00".to_string(),
            tokens_used: None,
        }
    }

    #[test]
    fn keeps_everything_when_it_fits_the_budget() {
        // 预算充足时不该丢任何一条 —— 这是「别忘事」的前提。
        let history = vec![
            msg("user", "你好"),
            msg("assistant", "你好，有什么可以帮你的？"),
            msg("user", "帮我打开记事本"),
        ];
        let result = assemble_history(&history, 4000);
        assert_eq!(result.pending_summary_count, 0, "不应有需要压缩的条目");
        assert_eq!(result.messages.len(), 3, "全部条目都应保留");
    }

    #[test]
    fn keeps_the_most_recent_messages_when_over_budget() {
        // 超预算时必须保住最新的 —— 用户刚说的话比十轮前的寒暄重要得多。
        let mut history = Vec::new();
        for i in 0..20 {
            history.push(msg("user", &format!("第{}条消息，内容稍长一些用于占位", i)));
        }
        let result = assemble_history(&history, 60);
        assert_eq!(result.pending_summary_count, 20 - result.messages.len());
        assert!(result.pending_summary_count > 0, "应当有远期条目");
        // 最后一条必须还在
        let last = result.messages.last().expect("至少要留一条");
        assert!(last.content.contains("第19条"), "最新一条必须保留，实际: {}", last.content);
    }

    #[test]
    fn never_emits_a_tool_message_into_history() {
        // 远期裁剪会让 tool 消息失去配对的 assistant tool_call，
        // 直接喂给模型会引起格式错误，所以转换阶段就过滤掉。
        let history = vec![
            msg("user", "看下系统状态"),
            msg("assistant", ""),
            msg("tool", "CPU:10% 内存:40%"),
            msg("user", "谢谢"),
        ];
        let result = assemble_history(&history, 4000);
        assert!(
            result.messages.iter().all(|m| m.role != "tool"),
            "历史中不应出现 role=tool 的消息"
        );
    }

    #[test]
    fn flattens_only_user_and_assistant_for_summary() {
        // tool 是中间过程，对「聊过什么」没价值，且体积大，压缩时排除。
        let history = vec![
            msg("user", "帮我看看磁盘"),
            msg("tool", "C: 90% D: 30%"),
            msg("assistant", "C 盘快满了"),
        ];
        let flat = flatten_for_summary(&history);
        assert!(flat.contains("用户：帮我看看磁盘"));
        assert!(flat.contains("助手：C 盘快满了"));
        assert!(!flat.contains("C: 90% D: 30%"), "tool 内容不应进入摘要素材");
    }

    #[test]
    fn estimates_chinese_as_more_expensive_than_ascii() {
        // 中文按 0.6 token/字，英文按 0.25 token/字符 —— 断言中文更贵，
        // 防止有人把它改成按字节数算（中文一字 3 字节会严重高估）。
        let chinese = estimate_tokens("这是一句中文");
        let ascii = estimate_tokens("abcdef");
        assert!(
            chinese > ascii,
            "6 个汉字({})应比 6 个英文字符({})占更多 token",
            chinese,
            ascii
        );
    }

    #[test]
    fn does_not_inflate_chinese_by_counting_bytes() {
        // 锁定对既有 bug 的修复：原先 `ContextEngine::estimate_tokens` 用
        // `text.len()`（字节数）参与计算，中文一字 3 字节，「这是一句中文」
        // 被算成 9 token（正确约 3.6），高估 2.5 倍。
        //
        // 这里断言上界：6 个汉字必须算出明显小于 8 的值。
        // 如果有人改回按字节算，这条会立刻失败。
        let tokens = estimate_tokens("这是一句中文");
        assert!(
            tokens <= 5,
            "6 个汉字不应超过 5 token（0.6/字 ≈ 3.6），实际 {} —— 是否又按字节数算了？",
            tokens
        );
    }

    #[test]
    fn empty_history_is_handled() {
        let result = assemble_history(&[], 1000);
        assert!(result.messages.is_empty());
        assert_eq!(result.pending_summary_count, 0);
    }
}
