//! 网页版本快照。
//!
//! 解决的问题：改稿是"整页重生成"，一次不满意的修改会把上一版直接顶掉，用户回不去。
//! 实测最难受的场景是 —— 页面本来能用，只想改个主色调，改完却连按钮都点不动了；
//! 这时候既没有旧版可退，也只能凭记忆重新描述一遍需求。
//!
//! 设计取舍：
//! - **纯内存 + 进程内单例**，不落库。理由：快照里是完整的 HTML 源码（单份可达数十 KB），
//!   落进 SQLite 会让 `settings` 那类小表的读写路径变重；而这个功能的生命周期与"当前这次
//!   编辑会话"绑定，进程退出后没有保留价值。
//! - **按会话分组**：同一份需求的多次修改共享一条版本链，新需求开新链。
//! - **保留全量 HTML**，不做 diff 存储。单份几十 KB、一条链最多十几版，内存占用在几 MB 量级，
//!   换来的是"回退"这个操作退化成一次赋值 —— 不需要重放补丁，也不会因补丁错位而回退失败。
//!
//! 与 `lib.rs` 的集成点只有三个：生成完成后 `push`、前端 `list`、前端点回退时 `get`。

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

/// 一条链最多保留的版本数。
///
/// 超过就丢最早的：用户关心的是"最近几步能不能退回去"，而不是完整历史。
/// 每份 HTML 按 30KB 估，20 版约 600KB，可以放心常驻内存。
pub const MAX_VERSIONS_PER_CHAIN: usize = 20;

/// 单个版本。
#[derive(Debug, Clone)]
pub struct Snapshot {
    /// 版本号，从 1 开始递增。
    pub version: u32,
    /// 生成/修改完成的时间（本地可读格式）。
    pub created_at: String,
    /// 完整 HTML 源码。
    pub html: String,
    /// 触发这一版的操作说明：首版是需求摘要，后续是用户的修改意见。
    pub note: String,
    /// 自检加权致命度（越小越好）。用于在列表里直观对比"哪一版更健康"。
    pub health: usize,
    /// 自检问题条数。
    pub issues: usize,
    /// 其中影响功能的问题条数。
    pub functional_issues: usize,
}

/// 一条版本链。
#[derive(Debug, Clone, Default)]
pub struct Chain {
    /// 链标识（用需求指纹，同一需求复用同一条链）。
    pub id: String,
    /// 按时间顺序排列的版本。
    pub versions: Vec<Snapshot>,
}

impl Chain {
    /// 最新一版。
    pub fn latest(&self) -> Option<&Snapshot> {
        self.versions.last()
    }

    /// 取指定版本。
    pub fn get(&self, version: u32) -> Option<&Snapshot> {
        self.versions.iter().find(|item| item.version == version)
    }

    /// 追加一版，返回版本号。
    fn push(
        &mut self,
        html: String,
        note: String,
        health: usize,
        issues: usize,
        functional: usize,
    ) -> u32 {
        let version = self.versions.last().map_or(1, |last| last.version + 1);
        self.versions.push(Snapshot {
            version,
            created_at: now(),
            html,
            note,
            health,
            issues,
            functional_issues: functional,
        });
        // 超限就从头丢。注意版本号不回退 —— 回退时前端拿的是"某次记录的版本号"，
        // 若号码被复用，可能出现"点第 3 版却拿到后来那一版"的错配。
        if self.versions.len() > MAX_VERSIONS_PER_CHAIN {
            let drop_count = self.versions.len() - MAX_VERSIONS_PER_CHAIN;
            self.versions.drain(..drop_count);
        }
        version
    }
}

/// 内存里的全部版本链。键是链标识。
fn store() -> &'static Mutex<HashMap<String, Chain>> {
    static STORE: OnceLock<Mutex<HashMap<String, Chain>>> = OnceLock::new();
    STORE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// 当前会话的活跃链标识。
///
/// 前端每次生成都会带上 `chain_id`；没带时用需求指纹兜底。这样"同一份需求反复改"
/// 自然落进同一条链，"换了需求"自动开新链，前端不必自己维护这个状态。
fn active_chain() -> &'static Mutex<Option<String>> {
    static ACTIVE: OnceLock<Mutex<Option<String>>> = OnceLock::new();
    ACTIVE.get_or_init(|| Mutex::new(None))
}
/// 从需求文本算一个稳定的链标识。
///
/// 只取前 32 个字符的哈希特征即可 —— 不需要密码学强度，只要求"同一份需求稳定、
/// 不同需求大概率不同"。加长度做二次区分，避免"前缀相同、后面不同"的两份需求撞车。
pub fn chain_id_for(requirements: &str) -> String {
    let trimmed = requirements.trim();
    if trimmed.is_empty() {
        return "empty".to_string();
    }
    let head: String = trimmed.chars().take(32).collect();
    // FNV-1a：够散、无依赖、几行写完。
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in trimmed.as_bytes() {
        hash ^= *byte as u64;
        hash = hash.wrapping_mul(0x100_0000_01b3);
    }
    format!("{head}#{hash:016x}#{}", trimmed.chars().count())
}

/// 记录一版新的产物。返回版本号。
///
/// `chain_id` 由调用方决定（首版用 `chain_id_for(requirements)`，后续沿用同一串）；
/// 同时会把它记为活跃链，方便后续不传时兜底。
#[allow(clippy::too_many_arguments)]
pub fn push(
    chain_id: &str,
    html: String,
    note: String,
    health: usize,
    issues: usize,
    functional: usize,
) -> u32 {
    let mut guard = store()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let chain = guard.entry(chain_id.to_string()).or_insert_with(|| Chain {
        id: chain_id.to_string(),
        versions: Vec::new(),
    });
    let version = chain.push(html, note, health, issues, functional);
    if let Ok(mut active) = active_chain().lock() {
        *active = Some(chain_id.to_string());
    }
    version
}

/// 列出某条链的版本摘要（不含 HTML 正文 —— 列表用不着，几十 KB 一份没必要传）。
///
/// `chain_id` 为空时用活跃链。
pub fn list(chain_id: Option<&str>) -> Vec<SnapshotSummary> {
    let id = match resolve(chain_id) {
        Some(id) => id,
        None => return Vec::new(),
    };
    let guard = store()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    guard
        .get(&id)
        .map(|chain| {
            chain
                .versions
                .iter()
                .map(|item| SnapshotSummary {
                    version: item.version,
                    created_at: item.created_at.clone(),
                    note: item.note.clone(),
                    health: item.health,
                    issues: item.issues,
                    functional_issues: item.functional_issues,
                    chars: item.html.chars().count(),
                })
                .collect()
        })
        .unwrap_or_default()
}

/// 取回某一版的完整 HTML，用于回退。
pub fn get(chain_id: Option<&str>, version: u32) -> Option<String> {
    let id = resolve(chain_id)?;
    let guard = store()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    guard
        .get(&id)
        .and_then(|chain| chain.get(version))
        .map(|item| item.html.clone())
}

/// 丢弃某条链（用户换了新需求时调用，避免旧链一直占着内存）。
pub fn drop_chain(chain_id: &str) {
    let mut guard = store()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    guard.remove(chain_id);
    if let Ok(mut active) = active_chain().lock() {
        if active.as_deref() == Some(chain_id) {
            *active = None;
        }
    }
}

/// 清空全部链。
///
/// 给"重置工作区"用 —— 用户换了完全无关的新需求、或想从干净状态开始时调用。
/// 进程重启后它本来就是空的。
#[allow(dead_code)]
pub fn clear() {
    let mut guard = store()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    guard.clear();
    if let Ok(mut active) = active_chain().lock() {
        *active = None;
    }
}

fn resolve(chain_id: Option<&str>) -> Option<String> {
    if let Some(id) = chain_id.filter(|id| !id.trim().is_empty()) {
        return Some(id.to_string());
    }
    active_chain().lock().ok().and_then(|active| active.clone())
}

/// 列表里的一项（不含正文，所以很小）。
#[derive(Debug, Clone, serde::Serialize)]
pub struct SnapshotSummary {
    pub version: u32,
    pub created_at: String,
    pub note: String,
    pub health: usize,
    pub issues: usize,
    pub functional_issues: usize,
    pub chars: usize,
}

fn now() -> String {
    chrono::Local::now().format("%H:%M:%S").to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 每个测试用一条**独有**的链，不碰共享状态。
    ///
    /// 原本这里用全局 `clear()` 做隔离，但 `#[test]` 默认并行执行，一个测试的 clear
    /// 会把另一个测试刚 push 的版本一起抹掉 —— 表现为随机失败。改用互不重叠的链标识
    /// 之后，测试之间不再有任何共享写入。
    ///
    /// 链标识里带上测试名，同一个测试的多次调用仍落在同一条链上。
    fn lane(name: &str) -> String {
        format!("__test__/{name}")
    }

    #[test]
    fn assigns_increasing_versions_within_a_chain() {
        let chain = lane("increasing");
        assert_eq!(
            push(&chain, "<html>v1</html>".into(), "首版".into(), 10, 2, 1),
            1
        );
        assert_eq!(
            push(&chain, "<html>v2</html>".into(), "改主色".into(), 4, 1, 0),
            2
        );
        assert_eq!(
            push(&chain, "<html>v3</html>".into(), "加收藏".into(), 0, 0, 0),
            3
        );

        let list = list(Some(&chain));
        assert_eq!(list.len(), 3);
        assert_eq!(list[0].version, 1);
        assert_eq!(list[2].version, 3);
        // 摘要要反映自检质量，供前端显示"哪一版更健康"。
        assert_eq!(list[2].health, 0);
        assert_eq!(list[1].functional_issues, 0);
    }

    #[test]
    fn returns_the_requested_version_for_rollback() {
        let chain = lane("rollback");
        push(&chain, "<html>第一版</html>".into(), "首版".into(), 9, 1, 1);
        push(
            &chain,
            "<html>第二版</html>".into(),
            "改一下".into(),
            2,
            1,
            0,
        );

        let back = get(Some(&chain), 1).expect("应当能取回第 1 版");
        assert_eq!(back, "<html>第一版</html>");
        assert_eq!(get(Some(&chain), 2).unwrap(), "<html>第二版</html>");
        assert!(get(Some(&chain), 99).is_none(), "不存在的版本要返回 None");
    }

    #[test]
    fn keeps_chains_separate_for_different_requirements() {
        let a = chain_id_for("# 取名器");
        let b = chain_id_for("# 记账本");
        assert_ne!(a, b);
        push(&a, "<html>A</html>".into(), "首版".into(), 0, 0, 0);
        push(&b, "<html>B</html>".into(), "首版".into(), 0, 0, 0);

        assert_eq!(list(Some(&a)).len(), 1);
        assert_eq!(list(Some(&b)).len(), 1);
        assert_eq!(get(Some(&a), 1).unwrap(), "<html>A</html>");
        assert_eq!(get(Some(&b), 1).unwrap(), "<html>B</html>");
    }

    #[test]
    fn falls_back_to_the_active_chain_when_none_given() {
        let chain = lane("active");
        push(&chain, "<html>C</html>".into(), "首版".into(), 0, 0, 0);
        // 不传 chain_id 时用最近一次 push 的链。
        //
        // 这条断言依赖"活跃指针"这个全局状态，所以在其它测试并行 push 时可能被改掉。
        // 只断言"能取到刚写的那一版"，不去断言链上只有 1 条 —— 后者会被邻居影响。
        assert_eq!(
            get(None, 1).unwrap_or_default(),
            "<html>C</html>".to_string()
        );
    }

    #[test]
    fn drops_the_oldest_versions_beyond_the_cap_but_keeps_numbering() {
        let chain = lane("cap");
        let total = MAX_VERSIONS_PER_CHAIN + 5;
        for index in 1..=total {
            push(
                &chain,
                format!("<html>{index}</html>"),
                format!("第 {index} 次"),
                0,
                0,
                0,
            );
        }
        let list = list(Some(&chain));
        assert_eq!(list.len(), MAX_VERSIONS_PER_CHAIN);
        // 最早 5 版被丢掉，但号码不重排 —— 前端仍能按号码准确取回。
        assert_eq!(list[0].version, 6, "第 1–5 版应被丢弃");
        assert_eq!(list.last().unwrap().version, total as u32);
        assert!(get(Some(&chain), 5).is_none());
        assert_eq!(
            get(Some(&chain), 6).unwrap(),
            "<html>6</html>",
            "号码与内容必须仍然对应"
        );
    }

    #[test]
    fn chain_id_is_stable_and_distinguishes_similar_prefixes() {
        let a = chain_id_for("# 取名器\n第一段");
        let a2 = chain_id_for("  # 取名器\n第一段  ");
        assert_eq!(a, a2, "前后空白不该影响链标识");

        // 前缀相同、正文不同要算不同链。
        let long_a = format!("# {}\nX", "很长的需求前缀".repeat(5));
        let long_b = format!("# {}\nY", "很长的需求前缀".repeat(5));
        assert_ne!(chain_id_for(&long_a), chain_id_for(&long_b));
    }

    #[test]
    fn dropping_a_chain_clears_its_versions() {
        let chain = lane("drop");
        push(&chain, "<html>E</html>".into(), "首版".into(), 0, 0, 0);
        assert_eq!(list(Some(&chain)).len(), 1);

        drop_chain(&chain);
        assert!(list(Some(&chain)).is_empty());
    }

    #[test]
    fn empty_requirement_still_gets_a_usable_chain() {
        let id = chain_id_for("   ");
        assert_eq!(id, "empty");
        push(&id, "<html></html>".into(), "首版".into(), 0, 0, 0);
        assert_eq!(get(Some(&id), 1).unwrap(), "<html></html>");
    }
}
