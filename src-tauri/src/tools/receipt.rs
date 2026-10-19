//! 工具回执 —— 每次工具调用的"事实记录"，供审计与后续验证使用。
//!
//! 与 `audit_logs` 的区别：
//! - `audit_logs` 回答"谁在什么时候调了什么、被允许还是拒绝"，是安全审计。
//! - `tool_receipts` 回答"这次调用实际改变了什么、用什么证据判定它完成了"，
//!   是事实账本。模型说"已经做完了"没有意义，回执里有没有 `VERIFIED` 才有意义。
//!
//! 这里不做任何"替模型圆场"的处理：字段缺失就是 `UNVERIFIED`，
//! 失败就是 `FAILED`，没有验证策略就是 `NOT_REQUIRED`。

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::contract::{CapabilityContract, SideEffect, VerificationStrategy};
use super::registry::ToolResult;

/// 验证结论
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum VerificationStatus {
    /// 证据成立：独立探查通过，或必填回执字段齐全且工具自报成功
    Verified,
    /// 工具自报成功，但回执字段缺失——不能算完成
    Unverified,
    /// 工具执行失败或超时
    Failed,
    /// 该能力没有声明机器可判据
    NotRequired,
    /// 工具自报成功，但独立探查发现事实相反。
    ///
    /// 这一档存在的意义：工具（以及替它说话的模型）说"做完了"，
    /// 而现实世界没变。此时以前会把字段齐全当成通过，等于让自述
    /// 覆盖事实。探查说"没成"时，任何自述都不能翻案。
    Contradicted,
}

impl VerificationStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            VerificationStatus::Verified => "VERIFIED",
            VerificationStatus::Unverified => "UNVERIFIED",
            VerificationStatus::Failed => "FAILED",
            VerificationStatus::NotRequired => "NOT_REQUIRED",
            VerificationStatus::Contradicted => "CONTRADICTED",
        }
    }
}

/// 独立探查的结论。
///
/// 与"回执字段齐全"是两件事：字段齐全只证明工具把话说全了，
/// 探查证明的是世界真的变了（进程在、文件在、窗口在）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProbeVerdict {
    /// 探查确认事实成立，附带判据摘要
    Pass(String),
    /// 探查发现事实不成立，附带原因
    Fail(String),
    /// 该能力没有可用的探查手段（不是"通过"，是"没查"）
    Unavailable,
}

/// 一次验证的完整结论
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VerificationOutcome {
    pub status: VerificationStatus,
    pub strategy: String,
    pub required_fields: Vec<String>,
    pub missing_fields: Vec<String>,
    /// 独立探查给出的判据（探查不可用时为 `None`）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub evidence: Option<String>,
}

impl VerificationOutcome {
    /// 是否达到了"可以对外声称完成"的门槛
    pub fn is_proven(&self) -> bool {
        matches!(
            self.status,
            VerificationStatus::Verified | VerificationStatus::NotRequired
        )
    }
}

/// 判定一次调用是否达成契约要求。
///
/// 优先级是刻意的：**独立探查 > 工具自述**。
/// 1. 探查说"事实不成立" → `CONTRADICTED`，字段再齐全也不算完成；
/// 2. 探查说"事实成立" → `VERIFIED`，这比字段齐全更强的证据；
/// 3. 探查不可用 → 退回字段校验（`VERIFIED` / `UNVERIFIED`）。
///
/// `required_fields` 支持点号路径（如 `result.path`），当前内置能力都只用
/// 顶层字段；保留点号是为了后续嵌套返回不必改签名。
pub fn verify(
    contract: &CapabilityContract,
    success: bool,
    result: &ToolResult,
    probe: Option<&ProbeVerdict>,
) -> VerificationOutcome {
    let required: Vec<String> = contract.required_fields.iter().map(|s| s.to_string()).collect();
    let strategy = contract.verification.as_str().to_string();

    if contract.verification == VerificationStrategy::None {
        return VerificationOutcome {
            status: VerificationStatus::NotRequired,
            strategy,
            required_fields: required,
            missing_fields: Vec::new(),
            evidence: None,
        };
    }

    if !success {
        return VerificationOutcome {
            status: VerificationStatus::Failed,
            strategy,
            required_fields: required,
            missing_fields: Vec::new(),
            evidence: None,
        };
    }

    // 探查结论优先：工具说成功，不代表世界变了。
    match probe {
        Some(ProbeVerdict::Fail(reason)) => {
            return VerificationOutcome {
                status: VerificationStatus::Contradicted,
                strategy,
                required_fields: required,
                missing_fields: Vec::new(),
                evidence: Some(reason.clone()),
            };
        }
        Some(ProbeVerdict::Pass(summary)) => {
            return VerificationOutcome {
                status: VerificationStatus::Verified,
                strategy,
                required_fields: required,
                missing_fields: Vec::new(),
                evidence: Some(summary.clone()),
            };
        }
        Some(ProbeVerdict::Unavailable) | None => {}
    }

    let data = result.data.as_ref();
    let missing: Vec<String> = required
        .iter()
        .filter(|field| !field_present(data, field))
        .cloned()
        .collect();

    VerificationOutcome {
        status: if missing.is_empty() {
            VerificationStatus::Verified
        } else {
            VerificationStatus::Unverified
        },
        strategy,
        required_fields: required,
        missing_fields: missing,
        evidence: None,
    }
}

/// 字段是否真的存在于回执里。
///
/// 注意"存在"的定义：`0`、`false`、空数组都算存在——它们是有效事实，
/// 不是缺失。把 `exit_code: 0` 当成缺失会让所有成功的命令都判为未验证。
fn field_present(data: Option<&serde_json::Value>, path: &str) -> bool {
    let Some(mut cursor) = data else {
        return false;
    };
    for segment in path.split('.') {
        match cursor.get(segment) {
            Some(next) => cursor = next,
            None => return false,
        }
    }
    true
}

/// 一条工具回执
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolReceipt {
    pub receipt_id: String,
    pub session_id: String,
    pub run_id: Option<String>,
    pub tool_name: String,
    pub capability_id: String,
    pub side_effect: String,
    pub scope: String,
    pub reversible: bool,
    pub risk_level: String,
    pub verification_strategy: String,
    pub verification_status: String,
    /// 独立探查给出的判据（探查不可用时为空）
    pub verification_evidence: Option<String>,
    /// 缺失的必填字段（JSON 数组字符串），便于直接落库
    pub missing_fields: String,
    /// 幂等键：仅实质副作用能力有值
    pub idempotency_key: Option<String>,
    /// 请求参数的规范化摘要
    pub request_digest: String,
    /// 返回结果的规范化摘要
    pub result_digest: Option<String>,
    pub permission_result: String,
    pub success: bool,
    pub error_code: Option<String>,
    pub duration_ms: i64,
}

impl ToolReceipt {
    /// 组装回执。`server_now` 由调用方注入，便于测试固定时间。
    #[allow(clippy::too_many_arguments)]
    pub fn build(
        contract: &CapabilityContract,
        session_id: &str,
        run_id: Option<&str>,
        arguments: &serde_json::Value,
        result: &ToolResult,
        permission_result: &str,
        duration_ms: i64,
        risk_level: &str,
        outcome: &VerificationOutcome,
    ) -> Self {
        let canonical_args = canonical_json(arguments);

        Self {
            receipt_id: uuid::Uuid::new_v4().to_string(),
            session_id: session_id.to_string(),
            run_id: run_id.map(str::to_owned),
            tool_name: contract.tool.to_string(),
            capability_id: contract.tool.to_string(),
            side_effect: contract.side_effect.as_str().to_string(),
            scope: contract.scope.to_string(),
            reversible: contract.reversible,
            risk_level: risk_level.to_string(),
            verification_strategy: outcome.strategy.clone(),
            verification_status: outcome.status.as_str().to_string(),
            verification_evidence: outcome.evidence.clone(),
            missing_fields: serde_json::to_string(&outcome.missing_fields)
                .unwrap_or_else(|_| "[]".to_string()),
            idempotency_key: idempotency_key(contract.side_effect, session_id, contract.tool, arguments),
            request_digest: sha256_hex(&canonical_args),
            result_digest: result
                .data
                .as_ref()
                .map(|d| sha256_hex(&canonical_json(d))),
            permission_result: permission_result.to_string(),
            success: result.success,
            error_code: result.error_code.clone(),
            duration_ms,
        }
    }
}

/// 计算幂等键。
///
/// 只有实质副作用能力才有幂等键：同一个会话里、同一份参数重复请求，
/// 得到同一个键。后续用它可以识别"这个动作是不是重放了"，
/// 而只读调用不需要这个语义。
pub fn idempotency_key(
    side_effect: SideEffect,
    session_id: &str,
    tool: &str,
    arguments: &serde_json::Value,
) -> Option<String> {
    if !side_effect.is_material() {
        return None;
    }
    let material = format!("{}|{}|{}", session_id, tool, canonical_json(arguments));
    Some(sha256_hex(&material)[..32].to_string())
}

/// 规范化 JSON：对象键排序后重新序列化。
///
/// 不做这一步的话，`{"a":1,"b":2}` 与 `{"b":2,"a":1}` 会得到不同的摘要与
/// 不同的幂等键，重放识别就失效了。
pub fn canonical_json(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            let mut out = String::from("{");
            for (idx, key) in keys.iter().enumerate() {
                if idx > 0 {
                    out.push(',');
                }
                out.push_str(&serde_json::to_string(key).unwrap_or_else(|_| "\"\"".to_string()));
                out.push(':');
                out.push_str(&canonical_json(&map[*key]));
            }
            out.push('}');
            out
        }
        serde_json::Value::Array(items) => {
            let parts: Vec<String> = items.iter().map(canonical_json).collect();
            format!("[{}]", parts.join(","))
        }
        other => serde_json::to_string(other).unwrap_or_else(|_| "null".to_string()),
    }
}

fn sha256_hex(input: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(input.as_bytes());
    hasher
        .finalize()
        .iter()
        .map(|b| format!("{:02x}", b))
        .collect()
}

/// 计算验证结论并落库 —— 执行器与测试共用同一条路径。
///
/// 放在这里而不是 `ToolExecutor` 里面的理由：记账是回执模块的职责，
/// 执行器只负责"拿到结果后叫一声"。副作用之外，这样测试也不必构造
/// 整个执行器（那会把 Tauri 的运行期一起拉进来）。
///
/// 返回值的第一项无论如何都要有：调用方要靠它判断"这次到底算不算完成"，
/// 即使写库失败也不能把结论丢掉（写库失败只影响可追溯性，不影响事实）。
#[allow(clippy::too_many_arguments)]
pub fn record(
    db: &crate::db::Database,
    contract: Option<&CapabilityContract>,
    tool_name: &str,
    session_id: &str,
    run_id: Option<&str>,
    arguments: &serde_json::Value,
    result: &ToolResult,
    permission_result: &str,
    duration_ms: i64,
    risk_level: &str,
    probe: Option<&ProbeVerdict>,
) -> (VerificationOutcome, String) {
    let Some(contract) = contract else {
        // 没有契约：给一个能区分的结论，而不是伪装成"已验证"。
        return (
            VerificationOutcome {
                status: VerificationStatus::NotRequired,
                strategy: "undeclared".to_string(),
                required_fields: Vec::new(),
                missing_fields: Vec::new(),
                evidence: None,
            },
            String::new(),
        );
    };

    // 被拒绝的调用没有"事实"可言，直接用失败结论，不跑字段校验。
    let outcome = if permission_result.starts_with("DENIED") {
        VerificationOutcome {
            status: VerificationStatus::Failed,
            strategy: contract.verification.as_str().to_string(),
            required_fields: contract
                .required_fields
                .iter()
                .map(|s| s.to_string())
                .collect(),
            missing_fields: Vec::new(),
            evidence: None,
        }
    } else {
        verify(contract, result.success, result, probe)
    };

    let receipt = ToolReceipt::build(
        contract,
        session_id,
        run_id,
        arguments,
        result,
        permission_result,
        duration_ms,
        risk_level,
        &outcome,
    );
    let receipt_id = receipt.receipt_id.clone();

    if let Err(e) = db.insert_tool_receipt(&receipt) {
        eprintln!("[Receipt] 写入回执失败（tool={}）: {}", tool_name, e);
        return (outcome, String::new());
    }

    (outcome, receipt_id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::contract::contract_for;

    fn contract(name: &str) -> &'static CapabilityContract {
        contract_for(name).expect("契约必须存在")
    }

    #[test]
    fn canonical_json_is_key_order_independent() {
        let a: serde_json::Value = serde_json::from_str(r#"{"b":2,"a":{"d":4,"c":3}}"#).unwrap();
        let b: serde_json::Value = serde_json::from_str(r#"{"a":{"c":3,"d":4},"b":2}"#).unwrap();
        assert_eq!(canonical_json(&a), canonical_json(&b));
    }

    #[test]
    fn idempotency_key_is_stable_for_material_effects() {
        let args_a: serde_json::Value = serde_json::from_str(r#"{"path":"C:\\a.txt","x":1}"#).unwrap();
        let args_b: serde_json::Value = serde_json::from_str(r#"{"x":1,"path":"C:\\a.txt"}"#).unwrap();
        let k1 = idempotency_key(SideEffect::LocalWrite, "s1", "write_file", &args_a);
        let k2 = idempotency_key(SideEffect::LocalWrite, "s1", "write_file", &args_b);
        assert_eq!(k1, k2);
        assert!(k1.is_some());
    }

    #[test]
    fn read_only_calls_have_no_idempotency_key() {
        assert_eq!(
            idempotency_key(SideEffect::LocalRead, "s1", "read_file", &serde_json::json!({"path":"a"})),
            None
        );
    }

    #[test]
    fn zero_and_false_count_as_present_fields() {
        let contract = contract("execute_command");
        let result = ToolResult::ok(serde_json::json!({
            "stdout": "",
            "stderr": "",
            "exit_code": 0,
            "success": false
        }));
        let outcome = verify(contract, true, &result, None);
        assert_eq!(outcome.status, VerificationStatus::Verified);
        assert!(outcome.missing_fields.is_empty());
    }

    #[test]
    fn missing_receipt_field_downgrades_to_unverified() {
        let contract = contract("write_file");
        let result = ToolResult::ok(serde_json::json!({"path": "C:\\a.txt"}));
        let outcome = verify(contract, true, &result, None);
        assert_eq!(outcome.status, VerificationStatus::Unverified);
        assert_eq!(outcome.missing_fields, vec!["bytes_written".to_string()]);
        assert!(!outcome.is_proven());
    }

    #[test]
    fn failure_is_reported_as_failed_not_unverified() {
        let contract = contract("take_screenshot");
        let result = ToolResult::err("IO_ERROR", "磁盘写入失败");
        let outcome = verify(contract, false, &result, None);
        assert_eq!(outcome.status, VerificationStatus::Failed);
    }

    #[test]
    fn undeclared_verification_is_not_required_rather_than_verified() {
        let contract = contract("ping");
        let result = ToolResult::ok(serde_json::json!({"message": "pong"}));
        let outcome = verify(contract, true, &result, None);
        assert_eq!(outcome.status, VerificationStatus::NotRequired);
    }

    // ============================================================
    // 独立探查：自述不能覆盖事实
    // ============================================================

    #[test]
    fn probe_failure_overrides_complete_receipt_fields() {
        let contract = contract("write_file");
        // 字段齐全、工具自报成功——但探查说文件根本不在。
        let result = ToolResult::ok(serde_json::json!({
            "path": "C:\\qa\\ghost.txt",
            "bytes_written": 12
        }));
        let probe = ProbeVerdict::Fail("文件不存在: C:\\qa\\ghost.txt".to_string());
        let outcome = verify(contract, true, &result, Some(&probe));

        assert_eq!(outcome.status, VerificationStatus::Contradicted);
        assert!(!outcome.is_proven(), "被探查否证时不允许声称完成");
        assert_eq!(outcome.evidence.as_deref(), Some("文件不存在: C:\\qa\\ghost.txt"));
    }

    #[test]
    fn probe_success_is_stronger_than_field_presence() {
        let contract = contract("write_file");
        // 工具没报 bytes_written，但探查确认文件确实写进去了。
        let result = ToolResult::ok(serde_json::json!({"path": "C:\\qa\\a.txt"}));
        let probe = ProbeVerdict::Pass("文件存在且非空: C:\\qa\\a.txt".to_string());
        let outcome = verify(contract, true, &result, Some(&probe));

        assert_eq!(outcome.status, VerificationStatus::Verified);
        assert!(outcome.is_proven());
        assert!(outcome.evidence.is_some());
    }

    #[test]
    fn unavailable_probe_falls_back_to_field_check() {
        let contract = contract("write_file");
        let result = ToolResult::ok(serde_json::json!({"path": "C:\\qa\\a.txt"}));
        let outcome = verify(contract, true, &result, Some(&ProbeVerdict::Unavailable));

        assert_eq!(outcome.status, VerificationStatus::Unverified);
        assert!(outcome.evidence.is_none());
    }

    // ============================================================
    // 落库链路：record() 必须把同一条结论同时返回并持久化
    // ============================================================

    fn fresh_db(tag: &str) -> (crate::db::Database, std::path::PathBuf) {
        let mut dir = std::env::temp_dir();
        dir.push(format!("pcg_receipt_{}_{}", tag, uuid::Uuid::new_v4().simple()));
        std::fs::create_dir_all(&dir).expect("创建临时目录");
        let db = crate::db::Database::new(&dir.join("test.db")).expect("打开数据库");
        db.init_schema().expect("初始化 schema");
        (db, dir)
    }

    #[test]
    fn verified_material_call_is_persisted_with_an_idempotency_key() {
        let (db, dir) = fresh_db("verified");
        let contract = contract("write_file");
        let args = serde_json::json!({"path": "C:\\qa\\a.txt", "content": "x"});
        let result = ToolResult::ok(serde_json::json!({
            "path": "C:\\qa\\a.txt",
            "bytes_written": 1
        }));

        let (outcome, receipt_id) = record(
            &db, Some(contract), "write_file", "qa-session", Some("qa-run"),
            &args, &result, "ALLOWED", 12, "MEDIUM", None,
        );

        assert_eq!(outcome.status, VerificationStatus::Verified);
        assert!(!receipt_id.is_empty(), "落库成功时必须返回回执 ID");

        let rows = db.list_tool_receipts(10, None).expect("读取回执");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].verification_status, "VERIFIED");
        assert_eq!(rows[0].side_effect, "local_write");
        assert!(rows[0].result_digest.is_some());

        let key = rows[0].idempotency_key.clone().expect("实质副作用必须有幂等键");
        assert!(
            db.find_successful_receipt_by_idempotency(&key)
                .expect("按幂等键查询")
                .is_some()
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn denied_call_is_recorded_as_failed_not_verified() {
        let (db, dir) = fresh_db("denied");
        let contract = contract_for("write_file").expect("契约");
        let args = serde_json::json!({"path": "C:\\Windows\\system32\\x.dll", "content": "x"});
        let result = ToolResult::err("PATH_DENIED", "路径不在可信目录内");

        let (outcome, receipt_id) = record(
            &db, Some(contract), "write_file", "qa-session", None,
            &args, &result, "DENIED: PATH_DENIED", 0, "MEDIUM", None,
        );

        assert_eq!(outcome.status, VerificationStatus::Failed);
        assert!(!receipt_id.is_empty());

        let rows = db.list_tool_receipts(10, Some("write_file")).expect("读取回执");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].verification_status, "FAILED");
        assert!(!rows[0].success);
        assert_eq!(rows[0].permission_result, "DENIED: PATH_DENIED");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn undeclared_capability_records_nothing_and_says_so() {
        let (db, dir) = fresh_db("undeclared");
        let result = ToolResult::ok(serde_json::json!({"ok": true}));

        let (outcome, receipt_id) = record(
            &db, None, "unknown_tool", "qa-session", None,
            &serde_json::json!({}), &result, "ALLOWED", 1, "LOW", None,
        );

        assert_eq!(outcome.status, VerificationStatus::NotRequired);
        assert_eq!(outcome.strategy, "undeclared");
        assert!(receipt_id.is_empty());
        assert!(db.list_tool_receipts(10, None).expect("读取回执").is_empty());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn contradicted_call_is_persisted_with_its_evidence() {
        let (db, dir) = fresh_db("contradicted");
        let contract = contract("write_file");
        let args = serde_json::json!({"path": "C:\\qa\\ghost.txt", "content": "x"});
        let result = ToolResult::ok(serde_json::json!({
            "path": "C:\\qa\\ghost.txt",
            "bytes_written": 1
        }));
        let probe = ProbeVerdict::Fail("文件不存在".to_string());

        let (outcome, _) = record(
            &db, Some(contract), "write_file", "qa-session", Some("run-1"),
            &args, &result, "ALLOWED", 5, "MEDIUM", Some(&probe),
        );
        assert_eq!(outcome.status, VerificationStatus::Contradicted);

        let rows = db.list_tool_receipts(10, Some("write_file")).expect("读取回执");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].verification_status, "CONTRADICTED");
        assert_eq!(rows[0].verification_evidence.as_deref(), Some("文件不存在"));

        let _ = std::fs::remove_dir_all(&dir);
    }
}
