//! 记忆衰减：按层级（tier）调整时间衰减率
//!
//! 借鉴 LumiCore 的 MemoryTier 分层（episodic → internalized → growth → core_identity），
//! 用 Rust 重写。核心思想：不同层级的记忆以不同速率淡出检索结果——
//! 原始对话记忆（episodic）快速淡出，核心身份（core_identity）永不衰减。

use crate::memory::MemoryTier;

/// 各 tier 的**日衰减率**：检索得分按 `rate ^ 距今天数` 缩放。
///
/// 越接近 1.0 衰减越慢；1.0 表示永不衰减。
pub fn tier_decay_rate(tier: MemoryTier) -> f64 {
    match tier {
        // 原始对话/任务记忆：最快淡出（约半个月后权重减半）
        MemoryTier::Episodic => 0.90,
        // 内化偏好：较慢衰减（用户偏好应当稳定）
        MemoryTier::Internalized => 0.97,
        // LLM 固化叙事：中等衰减
        MemoryTier::Growth => 0.95,
        // 核心身份：永不衰减
        MemoryTier::CoreIdentity => 1.0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn core_identity_never_decays() {
        assert_eq!(tier_decay_rate(MemoryTier::CoreIdentity), 1.0);
    }

    #[test]
    fn episodic_decays_fastest_among_decaying_tiers() {
        let rates = [
            tier_decay_rate(MemoryTier::Episodic),
            tier_decay_rate(MemoryTier::Internalized),
            tier_decay_rate(MemoryTier::Growth),
        ];
        let min = rates.iter().cloned().fold(f64::INFINITY, f64::min);
        assert_eq!(min, tier_decay_rate(MemoryTier::Episodic));
    }
}
