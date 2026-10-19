import { invoke } from "@tauri-apps/api/core";

/**
 * 任务真值 —— 界面上"这件事到底成没成"的唯一来源。
 *
 * 这里刻意不从工具返回值里自己推断结论：后端已经把"回执字段齐全"、
 * "独立探查"、"权限拒绝"三件事合成一个裁决了。前端再猜一次，
 * 就又多了一处会说谎的地方。
 */

export type TaskVerdict =
  | "EMPTY"
  | "VERIFIED"
  | "PARTIAL"
  | "UNVERIFIED"
  | "FAILED"
  | "CONTRADICTED"
  | "NOT_REQUIRED";

export type AttentionReason = "CONTRADICTED" | "FAILED" | "UNVERIFIED" | "DENIED";

export type ReceiptStatus =
  | "VERIFIED"
  | "UNVERIFIED"
  | "FAILED"
  | "NOT_REQUIRED"
  | "CONTRADICTED";

export interface EvidenceLine {
  receipt_id: string;
  tool_name: string;
  capability_id: string;
  side_effect: string;
  risk_level: string;
  verification_status: ReceiptStatus;
  strategy: string;
  evidence?: string;
  missing_fields: string[];
  success: boolean;
  denied: boolean;
  material: boolean;
  duration_ms: number;
  summary: string;
}

export interface TaskTruthCounts {
  total: number;
  material: number;
  read_only: number;
  verified: number;
  unverified: number;
  failed: number;
  contradicted: number;
  not_required: number;
  denied: number;
}

export interface TaskTruthSnapshot {
  run_id?: string;
  session_id: string;
  verdict: TaskVerdict;
  proven: boolean;
  counts: TaskTruthCounts;
  attention: AttentionReason[];
  lines: EvidenceLine[];
}

export interface ClaimAssessment {
  claims_success: boolean;
  backed: boolean;
  correction?: string;
}

export interface TaskTruthResponse {
  snapshot: TaskTruthSnapshot;
  claim?: ClaimAssessment;
}

/** 结论 -> 语气。语气只决定颜色，不决定文案。 */
export type EvidenceTone = "ok" | "warn" | "bad" | "idle";

export interface VerdictPresentation {
  label: string;
  detail: string;
  tone: EvidenceTone;
}

export function verdictPresentation(snapshot: TaskTruthSnapshot): VerdictPresentation {
  const counts = snapshot.counts;
  switch (snapshot.verdict) {
    case "VERIFIED":
      return {
        label: "已验证",
        detail: `${counts.verified} 项操作的证据都核对上了`,
        tone: "ok",
      };
    case "PARTIAL":
      return {
        label: "部分完成",
        detail: `${counts.verified} 项有证据，${counts.unverified} 项没有`,
        tone: "warn",
      };
    case "UNVERIFIED":
      return {
        label: "未能确认",
        detail: "工具说做完了，但拿不到可以核对的证据",
        tone: "warn",
      };
    case "FAILED":
      return {
        label: "执行失败",
        detail: `${counts.failed} 项操作报错`,
        tone: "bad",
      };
    case "CONTRADICTED":
      return {
        label: "与事实不符",
        detail: "工具说成功了，核对却发现现实没变",
        tone: "bad",
      };
    case "NOT_REQUIRED":
      return {
        label: "只读操作",
        detail: "这一轮没有改动系统，无需验证",
        tone: "idle",
      };
    default:
      return {
        label: "无操作",
        detail: "这一轮没有调用任何工具",
        tone: "idle",
      };
  }
}

const REASON_TEXT: Record<AttentionReason, string> = {
  CONTRADICTED: "有操作被核对否决：工具说完成了，实际没有",
  FAILED: "有操作执行失败",
  UNVERIFIED: "有操作缺少可以核对的证据",
  DENIED: "有操作因为没有获得授权而没有执行",
};

export function attentionText(reason: AttentionReason): string {
  return REASON_TEXT[reason] ?? reason;
}

export function receiptTone(status: ReceiptStatus): EvidenceTone {
  switch (status) {
    case "VERIFIED":
      return "ok";
    case "CONTRADICTED":
    case "FAILED":
      return "bad";
    case "UNVERIFIED":
      return "warn";
    default:
      return "idle";
  }
}

export function receiptLabel(status: ReceiptStatus): string {  switch (status) {
    case "VERIFIED":
      return "已验证";
    case "CONTRADICTED":
      return "与事实不符";
    case "FAILED":
      return "失败";
    case "UNVERIFIED":
      return "未确认";
    default:
      return "无可判据";
  }
}

const SIDE_EFFECT_LABEL: Record<string, string> = {
  local_read: "读取",
  local_write: "写入文件",
  local_state_change: "改系统设置",
  desktop_control: "操作桌面",
  network_read: "联网读取",
  external_state_change: "改外部状态",
  external_communication: "对外通信",
  process_execution: "启动进程",
  installation: "安装软件",
  none: "无副作用",
};

const STRATEGY_LABEL: Record<string, string> = {
  terminal_receipt: "终态回执",
  state_diff: "状态对比",
  artifact: "产物核对",
  provider_ack: "调用方回执",
  none: "无判据",
  undeclared: "未登记契约",
};

export function sideEffectLabel(value: string): string {
  return SIDE_EFFECT_LABEL[value] ?? value;
}

export function strategyLabel(value: string): string {
  return STRATEGY_LABEL[value] ?? value;
}

/**
 * 拉取一轮任务的真值。失败返回 null —— 界面不该因为取证失败
 * 就编一个"成功"出来，宁可不显示这块。
 */
export async function fetchTaskTruth(
  runId: string,
  claimText?: string,
): Promise<TaskTruthResponse | null> {
  if (!runId) return null;
  try {
    return await invoke<TaskTruthResponse>("get_task_truth", {
      runId,
      claimText: claimText && claimText.trim() ? claimText : null,
    });
  } catch (error) {
    console.warn("[taskTruth] 读取任务真值失败", error);
    return null;
  }
}
