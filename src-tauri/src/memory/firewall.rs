//! 记忆防火墙：过滤敏感信息与噪声
//!
//! 借鉴早期原型的 Memory Firewall 设计（读逻辑、用 Rust 重写，不逐字复制）：
//! 每条记忆入库前先过防火墙，判定是否接受。核心价值是**拦住密钥/口令类内容**，
//! 不把 API key、token、密码写进长期记忆库——这与「不泄露密钥」的全局纪律一致。
//!
//! 单用户本地应用没有 org/shared 多租户场景，所以这里**不引入**原型中的
//! privacy_class / retention 多态推断，只保留真正有意义的「是否疑似密钥」判定，
//! 避免为用不上的概念加列、加枚举。

use serde::{Deserialize, Serialize};

/// 防火墙裁决结果
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FirewallDecision {
    /// 是否允许入库
    pub accepted: bool,
    /// 裁决原因（拒绝时给用户看的解释）
    pub reason: String,
    /// 是否判定为疑似敏感信息
    pub is_secret: bool,
}

/// 强信号密钥前缀：出现即判敏感，无需其他条件。
///
/// 这些前缀在正常自然语言里几乎不出现，且几乎总是密钥本体：
/// - `sk-`：OpenAI / 多数厂商的 API key 前缀
/// - `ghp_` / `gho_`：GitHub personal / OAuth token
/// - `xoxb-` / `xoxp-`：Slack token
/// - `AIza`：Google API key 前缀
/// - `AKIA`：AWS Access Key ID 前缀
const STRONG_KEY_PREFIXES: &[&str] = &[
    "sk-", "ghp_", "gho_", "xoxb-", "xoxp-", "aiza", "akia", "sk_live_", "sk_test_",
];

/// 密钥关键词：需要与「长得像值的长串」同时出现才判敏感，避免误拦
/// 「我忘了密码」这类正常内容。
const SECRET_KEYWORDS: &[&str] = &[
    "api_key",
    "apikey",
    "api key",
    "api-key",
    "access_token",
    "access token",
    "access-token",
    "secret",
    "password",
    "passwd",
    "passphrase",
    "private_key",
    "private key",
    "bearer",
    "authorization",
    "client_secret",
    "client-secret",
];

/// 评估防火墙
///
/// `user_approved`：保留参数，与早期原型对齐——将来若提供「显式批准存敏感项」的能力，
/// 可通过它放行。当前所有调用点传 `false`，即敏感内容一律拒绝。
pub fn evaluate_firewall(content: &str, user_approved: bool) -> FirewallDecision {
    let trimmed = content.trim();

    // 空内容或纯空白：拒绝
    if trimmed.is_empty() {
        return FirewallDecision {
            accepted: false,
            reason: "内容为空".to_string(),
            is_secret: false,
        };
    }

    // 过短的碎片（< 2 字）：噪声，拒绝
    if trimmed.chars().count() < 2 {
        return FirewallDecision {
            accepted: false,
            reason: "内容过短，视为噪声".to_string(),
            is_secret: false,
        };
    }

    // 敏感信息判定
    let is_secret = looks_like_secret(trimmed);

    if is_secret && !user_approved {
        return FirewallDecision {
            accepted: false,
            reason: "内容疑似包含密钥或口令，已拒绝写入长期记忆".to_string(),
            is_secret: true,
        };
    }

    FirewallDecision {
        accepted: true,
        reason: "通过防火墙".to_string(),
        is_secret,
    }
}

/// 判断文本是否疑似含密钥/口令。
///
/// 两条规则（命中其一即判敏感）：
/// 1. 出现强信号密钥前缀（`sk-`、`ghp_` 等）——几乎必是密钥本体；
/// 2. 同时出现密钥关键词（`password` 等）与「长得像值的长串」（连续 ≥20 个
///    字母数字/下划线/连字符）——关键词单独出现不拦，避免误杀正常记忆。
fn looks_like_secret(content: &str) -> bool {
    let lower = content.to_lowercase();

    // 规则 1：强前缀
    if STRONG_KEY_PREFIXES.iter().any(|p| lower.contains(p)) {
        return true;
    }

    // 规则 2：关键词 + 长 token 同时出现
    let has_keyword = SECRET_KEYWORDS.iter().any(|k| lower.contains(k));
    if !has_keyword {
        return false;
    }

    has_long_token(content)
}

/// 是否存在「长得像密钥值」的长串：连续 ≥20 个 `[A-Za-z0-9_\-]` 字符。
fn has_long_token(content: &str) -> bool {
    let mut run = 0usize;
    for c in content.chars() {
        if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
            run += 1;
            if run >= 20 {
                return true;
            }
        } else {
            run = 0;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_empty_and_tiny_content() {
        assert!(!evaluate_firewall("", false).accepted);
        assert!(!evaluate_firewall("   ", false).accepted);
        assert!(!evaluate_firewall("一", false).accepted);
    }

    #[test]
    fn accepts_normal_memory() {
        let d = evaluate_firewall("用户偏好深色主题", false);
        assert!(d.accepted);
        assert!(!d.is_secret);
    }

    #[test]
    fn rejects_api_key_with_sk_prefix() {
        let d = evaluate_firewall(
            "我的 openai key 是 sk-abcdefghijklmnopqrstuvwxyz123456",
            false,
        );
        assert!(!d.accepted, "应拒绝，实际原因: {}", d.reason);
        assert!(d.is_secret);
    }

    #[test]
    fn rejects_keyword_plus_long_token() {
        let d = evaluate_firewall("password: Abc12345678901234567890", false);
        assert!(!d.accepted, "应拒绝，实际原因: {}", d.reason);
        assert!(d.is_secret);
    }

    #[test]
    fn does_not_misclassify_forgot_password() {
        // 关键词单独出现、没有长 token，不应误拦正常记忆。
        let d = evaluate_firewall("用户忘了自己的密码，需要重置", false);
        assert!(d.accepted, "不应误拦，实际原因: {}", d.reason);
        assert!(!d.is_secret);
    }

    #[test]
    fn user_approval_can_bypass() {
        let d = evaluate_firewall("token: sk-abcdefghijklmnopqrstuvwxyz", true);
        assert!(d.accepted, "显式批准后应放行");
        assert!(d.is_secret);
    }
}
