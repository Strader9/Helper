export type ChatRunPhase =
  | "idle"
  | "routing"
  | "executing"
  | "waiting_permission"
  | "responding"
  | "cancelling"
  | "completed"
  | "failed";

export interface ChatRunStatus {
  phase: ChatRunPhase;
  detail?: string;
  startedAt?: number;
}

export interface ChatEventEnvelope<T = unknown> {
  type?: string;
  data?: T;
  session_id?: string;
  run_id?: string;
  request_id?: string;
  [key: string]: unknown;
}

export const IDLE_CHAT_RUN: ChatRunStatus = { phase: "idle" };

export function createChatRunId(): string {
  if (typeof globalThis.crypto?.randomUUID === "function") {
    return globalThis.crypto.randomUUID();
  }
  return `run-${Date.now()}-${Math.random().toString(36).slice(2, 10)}`;
}

const ACTIVE_PHASES = new Set<ChatRunPhase>([
  "routing",
  "executing",
  "waiting_permission",
  "responding",
  "cancelling",
]);

export function isActiveChatPhase(phase: ChatRunPhase): boolean {
  return ACTIVE_PHASES.has(phase);
}

export function parseEventEnvelope(payload: unknown): ChatEventEnvelope {
  if (typeof payload !== "string") {
    return payload && typeof payload === "object"
      ? payload as ChatEventEnvelope
      : { data: payload };
  }

  try {
    const parsed: unknown = JSON.parse(payload);
    return parsed && typeof parsed === "object"
      ? parsed as ChatEventEnvelope
      : { data: parsed };
  } catch {
    return { data: payload };
  }
}

function findErrorDetails(value: unknown, depth = 0): { message: string; code?: string } | null {
  if (depth > 4 || value == null) return null;

  if (value instanceof Error) {
    return findErrorDetails(value.message, depth + 1) ?? { message: value.message };
  }

  if (typeof value === "string") {
    const text = value.trim();
    if (!text) return null;
    if ((text.startsWith("{") && text.endsWith("}")) || (text.startsWith("[") && text.endsWith("]"))) {
      try {
        return findErrorDetails(JSON.parse(text), depth + 1) ?? { message: text };
      } catch {
        // 普通文本错误，直接交给下方处理。
      }
    }
    return { message: text };
  }

  if (typeof value === "object") {
    const record = value as Record<string, unknown>;
    const code = typeof record.code === "string" ? record.code : undefined;
    for (const key of ["error", "message", "reason", "detail", "data"]) {
      const nested = findErrorDetails(record[key], depth + 1);
      if (nested) return { message: nested.message, code: nested.code ?? code };
    }
    if (code) return { message: code, code };
  }

  return null;
}

const ERROR_CODE_MESSAGES: Record<string, string> = {
  PATH_PROTECTED: "系统安全策略阻止了访问这个路径。请直接使用应用名称启动，或在设置中为该应用授权。",
  PATH_NOT_ALLOWED: "当前路径不在允许范围内，请先授权后重试。",
  PERMISSION_DENIED: "操作已取消：没有获得所需权限。",
  PERMISSION_TIMEOUT: "等待授权超时，操作已安全取消。",
  COMMAND_BLOCKED: "该命令被安全策略拦截，请换一种更明确的操作方式。",
  TOOL_NOT_FOUND: "暂时没有可完成此操作的工具。",
  PROGRAM_NOT_FOUND: "没有找到这个应用，请确认名称或安装路径。",
  OLLAMA_UNAVAILABLE: "本地 AI 服务当前不可用，请确认 Ollama 已启动。",
  CANCELLED: "操作已取消。",
};

export function humanizeError(value: unknown, fallback = "操作没有完成，请重试"): string {
  const details = findErrorDetails(value);
  if (!details) return fallback;

  const code = details.code?.toUpperCase();
  if (code && ERROR_CODE_MESSAGES[code]) return ERROR_CODE_MESSAGES[code];

  const upperMessage = details.message.toUpperCase();
  const matchedCode = Object.keys(ERROR_CODE_MESSAGES).find((key) => upperMessage.includes(key));
  if (matchedCode) return ERROR_CODE_MESSAGES[matchedCode];

  if (/TIMED?\s*OUT|TIMEOUT|超时/i.test(details.message)) {
    return "操作等待超时，请检查本地 AI 服务或目标应用后重试。";
  }
  if (/CONNECTION REFUSED|ECONNREFUSED|无法连接|连接失败/i.test(details.message)) {
    return "无法连接本地 AI 服务，请确认 Ollama 已启动。";
  }

  return details.message.length > 240
    ? `${details.message.slice(0, 237)}...`
    : details.message;
}

export function formatToolResult(value: string): string {
  try {
    const parsed: unknown = JSON.parse(value);
    if (parsed && typeof parsed === "object") {
      const record = parsed as Record<string, unknown>;
      if (record.error || record.code) return humanizeError(record, "工具执行失败");
      for (const key of ["message", "summary"]) {
        if (typeof record[key] === "string" && record[key]) return record[key] as string;
      }
      return JSON.stringify(parsed, null, 2);
    }
  } catch {
    // 非 JSON 工具结果按原文显示。
  }
  return value;
}

export function phasePresentation(status: ChatRunStatus, elapsedSeconds = 0): {
  label: string;
  detail: string;
} {
  const slowHint = elapsedSeconds >= 15 ? ` · 已用 ${elapsedSeconds} 秒，仍在处理中` : "";
  switch (status.phase) {
    case "routing":
      return { label: "理解指令", detail: `${status.detail ?? "正在选择最快的执行路径"}${slowHint}` };
    case "executing":
      return { label: "执行中", detail: `${status.detail ?? "正在执行操作"}${slowHint}` };
    case "waiting_permission":
      return { label: "等待授权", detail: status.detail ?? "请确认权限后继续" };
    case "responding":
      return { label: "生成回复", detail: `${status.detail ?? "正在整理执行结果"}${slowHint}` };
    case "cancelling":
      return { label: "正在取消", detail: status.detail ?? "正在安全停止当前任务" };
    case "completed":
      return { label: "已完成", detail: status.detail ?? "指令已处理完成" };
    case "failed":
      return { label: "未完成", detail: status.detail ?? "操作执行失败" };
    default:
      return { label: "待命", detail: "可以开始新的指令" };
  }
}
