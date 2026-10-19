//! 任务真值快照 —— 一轮任务结束时的"判决书"。
//!
//! 判据优先级只有一条：**独立探查 > 回执字段齐全 > 工具自述成功**。
//! 单条回执已经按这个顺序判过了，这一层只做聚合，不再重新解释单条结果。
//! 聚合唯一新增的语义是：一件事拆成好几次调用时，少数派的下场会拖垮整体——
//! 五次调用里有一次被否决，这一轮就不能声称完成。

use serde::Serialize;

use crate::tools::contract::SideEffect;
use crate::tools::receipt::{ToolReceipt, VerificationStatus};

/// 一轮任务的整体裁决。
///
/// 顺序即严重程度，`verdict_of` 里按"最坏优先"短路。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum TaskVerdict {
    /// 这一轮没有产生任何回执（纯对话，没动过手）
    Empty,
    /// 有实质副作用的调用全部拿到了证据
    Verified,
    /// 一部分有证据、一部分没有——做到了，但不是全部都能证明
    Partial,
    /// 有实质副作用，但没有任何一条能证明
    Unverified,
    /// 有调用失败
    Failed,
    /// 有调用被独立探查否决：工具说成了，世界没变
    Contradicted,
    /// 只有无可判据的能力被调用（如只读），不构成"完成"也不构成"失败"
    NotRequired,
}

impl TaskVerdict {
    pub fn as_str(&self) -> &'static str {
        match self {
            TaskVerdict::Empty => "EMPTY",
            TaskVerdict::Verified => "VERIFIED",
            TaskVerdict::Partial => "PARTIAL",
            TaskVerdict::Unverified => "UNVERIFIED",
            TaskVerdict::Failed => "FAILED",
            TaskVerdict::Contradicted => "CONTRADICTED",
            TaskVerdict::NotRequired => "NOT_REQUIRED",
        }
    }

    /// 是否达到了"可以对外说这件事做完了"的门槛。
    ///
    /// `Empty` 也算够门槛：一个字都没动过，就没有"没做成"可言，
    /// 硬判成失败会把普通问答也标成红的。
    pub fn is_proven(&self) -> bool {
        matches!(
            self,
            TaskVerdict::Verified | TaskVerdict::NotRequired | TaskVerdict::Empty
        )
    }
}

/// 需要用户看一眼的原因。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum AttentionReason {
    /// 有调用被探查否决
    Contradicted,
    /// 有调用失败
    Failed,
    /// 有实质副作用但没有证据
    Unverified,
    /// 有调用被权限拒绝
    Denied,
}

impl AttentionReason {
    pub fn as_str(&self) -> &'static str {
        match self {
            AttentionReason::Contradicted => "CONTRADICTED",
            AttentionReason::Failed => "FAILED",
            AttentionReason::Unverified => "UNVERIFIED",
            AttentionReason::Denied => "DENIED",
        }
    }
}

/// 计数面板：界面上的那排数字直接取自这里，避免前端二次统计出偏差。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct TaskTruthCounts {
    pub total: i64,
    pub material: i64,
    pub read_only: i64,
    pub verified: i64,
    pub unverified: i64,
    pub failed: i64,
    pub contradicted: i64,
    pub not_required: i64,
    pub denied: i64,
}

/// 一条可以在界面上直接显示的判据。
#[derive(Debug, Clone, Serialize)]
pub struct EvidenceLine {
    pub receipt_id: String,
    pub tool_name: String,
    pub capability_id: String,
    pub side_effect: String,
    pub risk_level: String,
    /// `VERIFIED` / `UNVERIFIED` / `FAILED` / `NOT_REQUIRED` / `CONTRADICTED`
    pub verification_status: String,
    pub strategy: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub evidence: Option<String>,
    pub missing_fields: Vec<String>,
    pub success: bool,
    pub denied: bool,
    pub material: bool,
    pub duration_ms: i64,
    /// 给人看的一句话结论
    pub summary: String,
    /// 该回执的落库时间（本地时间字符串）。空串表示读不到时间——
    /// 旧数据或非库来源，前端据此不画时间轴刻度。
    pub at: String,
}

/// 一整轮的判决。
#[derive(Debug, Clone, Serialize)]
pub struct TaskTruthSnapshot {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    pub session_id: String,
    pub verdict: TaskVerdict,
    pub proven: bool,
    pub counts: TaskTruthCounts,
    pub attention: Vec<AttentionReason>,
    pub lines: Vec<EvidenceLine>,
}

fn status_of(receipt: &ToolReceipt) -> VerificationStatus {
    match receipt.verification_status.as_str() {
        "VERIFIED" => VerificationStatus::Verified,
        "UNVERIFIED" => VerificationStatus::Unverified,
        "FAILED" => VerificationStatus::Failed,
        "CONTRADICTED" => VerificationStatus::Contradicted,
        _ => VerificationStatus::NotRequired,
    }
}

fn is_denied(receipt: &ToolReceipt) -> bool {
    receipt.permission_result.starts_with("DENIED")
}

fn is_material(receipt: &ToolReceipt) -> bool {
    SideEffect::from_label(&receipt.side_effect)
        .map(|effect| effect.is_material())
        // 未知标签宁可当作有副作用：漏判会让一次真实改动溜出账本，
        // 误判最多只是让界面多问一句。
        .unwrap_or(true)
}

fn parse_missing_fields(raw: &str) -> Vec<String> {
    serde_json::from_str::<Vec<String>>(raw).unwrap_or_default()
}

fn summarize(receipt: &ToolReceipt, status: VerificationStatus, missing: &[String]) -> String {
    if is_denied(receipt) {
        return format!("{}：没有获得授权，未执行", receipt.tool_name);
    }
    match status {
        VerificationStatus::Verified => match &receipt.verification_evidence {
            Some(evidence) => format!("{}：已验证（{}）", receipt.tool_name, evidence),
            None => format!("{}：已验证", receipt.tool_name),
        },
        VerificationStatus::Contradicted => match &receipt.verification_evidence {
            Some(evidence) => format!(
                "{}：工具说成功，但核对结果是「{}」，不算完成",
                receipt.tool_name, evidence
            ),
            None => format!("{}：工具说成功，但核对结果相反，不算完成", receipt.tool_name),
        },
        VerificationStatus::Failed => {
            let code = receipt.error_code.as_deref().unwrap_or("未知错误");
            format!("{}：执行失败（{}）", receipt.tool_name, code)
        }
        VerificationStatus::Unverified => {
            if missing.is_empty() {
                format!("{}：工具说成功，但没有可核对的证据", receipt.tool_name)
            } else {
                format!(
                    "{}：工具说成功，但缺少可核对字段（{}）",
                    receipt.tool_name,
                    missing.join("、")
                )
            }
        }
        VerificationStatus::NotRequired => {
            format!("{}：该能力没有机器可判据", receipt.tool_name)
        }
    }
}

pub fn evidence_line(receipt: &ToolReceipt) -> EvidenceLine {
    let status = status_of(receipt);
    let missing = parse_missing_fields(&receipt.missing_fields);
    EvidenceLine {
        receipt_id: receipt.receipt_id.clone(),
        tool_name: receipt.tool_name.clone(),
        capability_id: receipt.capability_id.clone(),
        side_effect: receipt.side_effect.clone(),
        risk_level: receipt.risk_level.clone(),
        verification_status: status.as_str().to_string(),
        strategy: receipt.verification_strategy.clone(),
        evidence: receipt.verification_evidence.clone(),
        missing_fields: missing.clone(),
        success: receipt.success,
        denied: is_denied(receipt),
        material: is_material(receipt),
        duration_ms: receipt.duration_ms,
        summary: summarize(receipt, status, &missing),
        at: receipt.created_at.clone(),
    }
}

/// 聚合裁决。只看实质副作用：只读调用没通过验证不说明任务没完成。
fn verdict_of(counts: &TaskTruthCounts) -> TaskVerdict {
    if counts.total == 0 {
        return TaskVerdict::Empty;
    }
    if counts.contradicted > 0 {
        return TaskVerdict::Contradicted;
    }
    if counts.failed > 0 {
        return TaskVerdict::Failed;
    }
    if counts.verified > 0 && counts.unverified > 0 {
        return TaskVerdict::Partial;
    }
    if counts.unverified > 0 {
        return TaskVerdict::Unverified;
    }
    if counts.verified > 0 {
        return TaskVerdict::Verified;
    }
    TaskVerdict::NotRequired
}

/// 用一批回执构造快照。`run_id` 只做标记，不参与判断。
pub fn build(run_id: Option<&str>, receipts: &[ToolReceipt]) -> TaskTruthSnapshot {
    let mut counts = TaskTruthCounts {
        total: receipts.len() as i64,
        ..TaskTruthCounts::default()
    };

    for receipt in receipts {
        let status = status_of(receipt);
        let material = is_material(receipt);
        if material {
            counts.material += 1;
        } else {
            counts.read_only += 1;
        }
        if is_denied(receipt) {
            counts.denied += 1;
        }
        match status {
            VerificationStatus::Verified => counts.verified += 1,
            VerificationStatus::Unverified => counts.unverified += 1,
            VerificationStatus::Failed => counts.failed += 1,
            VerificationStatus::Contradicted => counts.contradicted += 1,
            VerificationStatus::NotRequired => counts.not_required += 1,
        }
    }

    let verdict = verdict_of(&counts);

    let mut attention = Vec::new();
    if counts.contradicted > 0 {
        attention.push(AttentionReason::Contradicted);
    }
    if counts.failed > 0 {
        attention.push(AttentionReason::Failed);
    }
    if counts.unverified > 0 {
        attention.push(AttentionReason::Unverified);
    }
    if counts.denied > 0 {
        attention.push(AttentionReason::Denied);
    }

    TaskTruthSnapshot {
        run_id: run_id.map(str::to_owned),
        session_id: receipts
            .first()
            .map(|r| r.session_id.clone())
            .unwrap_or_default(),
        verdict,
        proven: verdict.is_proven(),
        counts,
        attention,
        lines: receipts.iter().map(evidence_line).collect(),
    }
}

// ============================================================
// 宣称与回执的分歧
// ============================================================

/// 动作词。宣称"做完了"在中文里几乎总是"已/已经 + 动词"或"动词 + 好了"，
/// 所以这里只列动作本身，完成语义交给前后的标记判断——
/// 直接列"已打开"这类整词的话，"已经打开了"就会漏掉。
///
/// 不用正则库：要的只是"这句话在声称事情办完了吗"，
/// 词表加窗口够用，也不必为它把整条依赖引进二进制。
const ACTION_VERBS: &[&str] = &[
    "打开", "启动", "创建", "新建", "写入", "保存", "发送", "生成", "导出",
    "上传", "下载", "安装", "卸载", "关闭", "删除", "修改", "绘制", "执行",
    "完成", "处理", "搞定", "弄好", "办好", "重启", "切换", "设置", "整理",
];

/// 出现在动作词**之前**的完成标记（五个字以内，"已经帮你打开了"也要能认出来）
const PREFIX_MARKERS: &[&str] = &["已", "成功", "刚刚"];

/// 出现在动作词**之后**的完成标记（三个字以内）
const SUFFIX_MARKERS: &[&str] = &["成功", "好了", "完成", "完毕", "完了"];

/// 否定词：出现在动作词前三个字以内，就说明这不是"做完了"的宣称。
const NEGATION_MARKERS: &[&str] = &[
    "未", "没", "无法", "不能", "尚未", "并未", "还没", "拒绝", "失败", "取消",
];

/// 英文的完成语，只保留最典型的一批。
const CLAIM_MARKERS_EN: &[&str] = &[
    "successfully ", "all done", "task complete", "operation complete",
    "has been created", "has been opened", "has been saved", "is now open",
];

/// 取 `index` 前最多 `n` 个字（按字符边界切，不会切坏 UTF-8）。
fn before(text: &str, index: usize, n: usize) -> &str {
    let start = text[..index]
        .char_indices()
        .rev()
        .take(n)
        .last()
        .map(|(i, _)| i)
        .unwrap_or(0);
    &text[start..index]
}

/// 取 `index` 后最多 `n` 个字。
fn after(text: &str, index: usize, n: usize) -> &str {
    let end = text[index..]
        .char_indices()
        .take(n + 1)
        .last()
        .map(|(i, _)| index + i)
        .unwrap_or(text.len());
    &text[index..end]
}

fn negated_at(text: &str, index: usize) -> bool {
    let window = before(text, index, 3);
    NEGATION_MARKERS.iter().any(|marker| window.contains(marker))
}

/// 这段话在声称"已经做完了"吗。
///
/// 只认肯定语义：`没有打开`、`尚未完成` 不算宣称。
pub fn claims_success(text: &str) -> bool {
    for verb in ACTION_VERBS {
        let mut from = 0;
        while let Some(offset) = text[from..].find(verb) {
            let index = from + offset;
            let end = index + verb.len();
            let claimed = !negated_at(text, index)
                && (PREFIX_MARKERS
                    .iter()
                    .any(|marker| before(text, index, 5).contains(marker))
                    || SUFFIX_MARKERS
                        .iter()
                        .any(|marker| after(text, end, 3).contains(marker)));
            if claimed {
                return true;
            }
            from = end;
            if from >= text.len() {
                break;
            }
        }
    }
    let lowered = text.to_lowercase();
    CLAIM_MARKERS_EN.iter().any(|marker| lowered.contains(marker))
}

/// 对模型最终回复的判定：它说的和账本对得上吗。
#[derive(Debug, Clone, Serialize)]
pub struct ClaimAssessment {
    /// 回复里有没有"已经做完了"这类宣称
    pub claims_success: bool,
    /// 宣称是否有回执支撑
    pub backed: bool,
    /// 需要替换给用户看的话（`None` 表示原文可用）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub correction: Option<String>,
}

fn correction_for(snapshot: &TaskTruthSnapshot) -> String {
    match snapshot.verdict {
        TaskVerdict::Contradicted => {
            let detail = snapshot
                .lines
                .iter()
                .find(|line| line.verification_status == "CONTRADICTED")
                .map(|line| line.summary.clone())
                .unwrap_or_else(|| "核对结果与工具的说法不一致".to_string());
            format!("这一步没有完成：{}。", detail)
        }
        TaskVerdict::Failed => "这一步没有完成：执行过程中出错了。".to_string(),
        TaskVerdict::Unverified => {
            "这一步我还不能确认完成：没有拿到可以核对的证据。你可以让我再试一次。".to_string()
        }
        TaskVerdict::Partial => {
            "这一步只完成了一部分：有些操作有证据，有些没有。".to_string()
        }
        _ => "这一步的结果我无法确认。".to_string(),
    }
}

/// 把"模型说完成了"和"账本说完成了"对照一次。
///
/// 只有一种情况需要纠正：模型声称完成，而账本证明不了。
/// 反过来（账本成了、模型没说）不纠正——那只是话说得保守。
pub fn assess_claim(text: &str, snapshot: &TaskTruthSnapshot) -> ClaimAssessment {
    let claims = claims_success(text);
    if !claims {
        return ClaimAssessment {
            claims_success: false,
            backed: true,
            correction: None,
        };
    }
    if snapshot.proven {
        return ClaimAssessment {
            claims_success: true,
            backed: true,
            correction: None,
        };
    }
    ClaimAssessment {
        claims_success: true,
        backed: false,
        correction: Some(correction_for(snapshot)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn receipt(tool: &str, side_effect: &str, status: &str, evidence: Option<&str>) -> ToolReceipt {
        ToolReceipt {
            receipt_id: format!("r-{}", tool),
            session_id: "s1".to_string(),
            run_id: Some("run-1".to_string()),
            tool_name: tool.to_string(),
            capability_id: tool.to_string(),
            side_effect: side_effect.to_string(),
            scope: "test".to_string(),
            reversible: true,
            risk_level: "LOW".to_string(),
            verification_strategy: "state_diff".to_string(),
            verification_status: status.to_string(),
            verification_evidence: evidence.map(str::to_owned),
            missing_fields: "[]".to_string(),
            idempotency_key: None,
            request_digest: "d".to_string(),
            result_digest: None,
            permission_result: "ALLOWED".to_string(),
            success: status != "FAILED",
            error_code: None,
            duration_ms: 5,
            created_at: "2026-10-19 12:00:00".to_string(),
        }
    }

    #[test]
    fn empty_run_is_empty_not_failed() {
        let snapshot = build(Some("run-1"), &[]);
        assert_eq!(snapshot.verdict, TaskVerdict::Empty);
        assert!(snapshot.proven);
    }

    #[test]
    fn one_contradicted_call_vetoes_the_whole_run() {
        let receipts = vec![
            receipt("write_file", "local_write", "VERIFIED", Some("回读一致")),
            receipt("start_process", "process_execution", "CONTRADICTED", Some("进程未在运行")),
        ];
        let snapshot = build(Some("run-1"), &receipts);
        assert_eq!(snapshot.verdict, TaskVerdict::Contradicted);
        assert!(!snapshot.proven);
        assert!(snapshot.attention.contains(&AttentionReason::Contradicted));
    }

    #[test]
    fn verified_plus_unverified_is_partial() {
        let receipts = vec![
            receipt("write_file", "local_write", "VERIFIED", Some("回读一致")),
            receipt("set_volume", "local_state_change", "UNVERIFIED", None),
        ];
        assert_eq!(build(Some("run-1"), &receipts).verdict, TaskVerdict::Partial);
    }

    #[test]
    fn read_only_receipts_do_not_claim_completion() {
        let receipts = vec![receipt("read_file", "local_read", "NOT_REQUIRED", None)];
        let snapshot = build(Some("run-1"), &receipts);
        assert_eq!(snapshot.verdict, TaskVerdict::NotRequired);
        assert_eq!(snapshot.counts.read_only, 1);
        assert_eq!(snapshot.counts.material, 0);
    }

    #[test]
    fn read_only_gaps_do_not_drag_a_verified_write_down() {
        let receipts = vec![
            receipt("read_file", "local_read", "NOT_REQUIRED", None),
            receipt("write_file", "local_write", "VERIFIED", Some("回读一致")),
        ];
        assert_eq!(build(Some("run-1"), &receipts).verdict, TaskVerdict::Verified);
    }

    #[test]
    fn negation_disables_a_success_claim() {
        assert!(claims_success("已经打开了记事本"));
        assert!(!claims_success("没有打开记事本"));
        assert!(!claims_success("尚未完成"));
        assert!(!claims_success("我无法创建这个文件"));
    }

    #[test]
    fn unbacked_claim_gets_a_correction() {
        let receipts = vec![receipt("start_process", "process_execution", "UNVERIFIED", None)];
        let snapshot = build(Some("run-1"), &receipts);
        let assessment = assess_claim("已经帮你打开了记事本。", &snapshot);
        assert!(assessment.claims_success);
        assert!(!assessment.backed);
        assert!(assessment.correction.unwrap().contains("不能确认"));
    }

    #[test]
    fn backed_claim_is_left_alone() {
        let receipts = vec![receipt("write_file", "local_write", "VERIFIED", Some("回读一致"))];
        let snapshot = build(Some("run-1"), &receipts);
        let assessment = assess_claim("文件已写入。", &snapshot);
        assert!(assessment.claims_success);
        assert!(assessment.backed);
        assert!(assessment.correction.is_none());
    }

    #[test]
    fn evidence_line_carries_the_receipt_timestamp() {
        // 证据时间线靠这个字段画刻度：一旦它在映射时被丢掉，
        // 界面上就只剩一堆没有先后的行。
        let line = evidence_line(&receipt("write_file", "local_write", "VERIFIED", Some("回读一致")));
        assert_eq!(line.at, "2026-10-19 12:00:00");
    }

    #[test]
    fn contradicted_claim_cites_the_probe() {
        let receipts = vec![receipt(
            "start_process",
            "process_execution",
            "CONTRADICTED",
            Some("进程未在运行"),
        )];
        let snapshot = build(Some("run-1"), &receipts);
        let correction = assess_claim("已启动。", &snapshot).correction.unwrap();
        assert!(correction.contains("进程未在运行"));
    }
}
