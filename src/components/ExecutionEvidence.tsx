import { useState } from "react";
import CyberIcon from "./CyberIcon";
import "./ExecutionEvidence.css";
import {
  attentionText,
  receiptLabel,
  receiptTone,
  sideEffectLabel,
  strategyLabel,
  verdictPresentation,
  type EvidenceLine,
  type EvidenceTone,
  type ClaimAssessment,
  type TaskTruthSnapshot,
} from "../lib/taskTruth";

const TONE_ICON: Record<EvidenceTone, string> = {
  ok: "checkCircle",
  warn: "clock",
  bad: "xCircle",
  idle: "eye",
};

/**
 * 执行证据面板 —— 把回执账本里的判据摊开给用户看。
 *
 * 它存在的唯一理由：界面上"执行成功"这四个字以前只代表工具自己说了
 * 一声成功。现在这句话必须由判据支撑：有证据写"已验证"，没证据写
 * "未能确认"，被核对推翻写"与事实不符"。
 */
export default function ExecutionEvidence({
  snapshot,
  claim,
}: {
  snapshot: TaskTruthSnapshot;
  claim?: ClaimAssessment;
}) {
  const [expanded, setExpanded] = useState(false);
  const presentation = verdictPresentation(snapshot);
  const counts = snapshot.counts;
  const showLines = snapshot.lines.length > 0;

  const chips: Array<{ label: string; value: number; tone: EvidenceTone }> = [
    { label: "已验证", value: counts.verified, tone: "ok" },
    { label: "未确认", value: counts.unverified, tone: "warn" },
    { label: "失败", value: counts.failed, tone: "bad" },
    { label: "与事实不符", value: counts.contradicted, tone: "bad" },
  ];

  return (
    <section className={`evidence-panel tone-${presentation.tone}`}>
      <header className="evidence-head">
        <span className="evidence-head-icon">
          <CyberIcon name={TONE_ICON[presentation.tone]} size={15} />
        </span>
        <span className="evidence-title">执行证据</span>
        <span className="evidence-verdict">{presentation.label}</span>
        <span className="evidence-detail">{presentation.detail}</span>
        {showLines && (
          <button
            type="button"
            className="evidence-toggle"
            onClick={() => setExpanded((prev) => !prev)}
            aria-expanded={expanded}
          >
            <CyberIcon name={expanded ? "chevronDown" : "chevronRight"} size={13} />
            {snapshot.lines.length} 条回执
          </button>
        )}
      </header>

      <div className="evidence-counts">
        {chips.map((chip) => (
          <div
            key={chip.label}
            className={`evidence-chip ${chip.value > 0 ? `tone-${chip.tone}` : "tone-empty"}`}
          >
            <span className="evidence-chip-value">{chip.value}</span>
            <span className="evidence-chip-label">{chip.label}</span>
          </div>
        ))}
      </div>

      {snapshot.attention.length > 0 && (
        <ul className="evidence-attention">
          {snapshot.attention.map((reason) => (
            <li key={reason}>
              <CyberIcon name="alertTriangle" size={12} />
              {attentionText(reason)}
            </li>
          ))}
        </ul>
      )}

      {claim?.correction && (
        <p className="evidence-claim-warning">
          <CyberIcon name="shieldCheck" size={13} />
          {claim.correction}
        </p>
      )}

      {expanded && showLines && (
        <ul className="evidence-lines">
          {snapshot.lines.map((line) => (
            <EvidenceRow key={line.receipt_id} line={line} />
          ))}
        </ul>
      )}
    </section>
  );
}

function EvidenceRow({ line }: { line: EvidenceLine }) {
  const tone = receiptTone(line.verification_status);
  return (
    <li className={`evidence-line tone-${tone}`}>
      <span className="evidence-line-icon">
        <CyberIcon name={TONE_ICON[tone]} size={12} />
      </span>
      <div className="evidence-line-body">
        <div className="evidence-line-top">
          <span className="evidence-line-tool">{line.tool_name}</span>
          <span className="evidence-line-status">{receiptLabel(line.verification_status)}</span>
          <span className="evidence-line-meta">
            {sideEffectLabel(line.side_effect)} · {strategyLabel(line.strategy)} · {line.duration_ms}ms
          </span>
        </div>
        <p className="evidence-line-summary">{line.summary}</p>
        {line.missing_fields.length > 0 && (
          <p className="evidence-line-missing">
            缺少字段：{line.missing_fields.join("、")}
          </p>
        )}
      </div>
    </li>
  );
}
