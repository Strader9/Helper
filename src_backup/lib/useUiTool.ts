import { useCallback, useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

/** 界面直呼工具时使用的会话标识，与后端 run_ui_tool 保持一致 */
const UI_SESSION = "ui-console";

export interface PermissionRequest {
  request_id: string;
  session_id: string;
  tool_name: string;
  description: string;
  risk_level: string;
  path?: string;
}

export interface UiToolResult {
  tool: string;
  success: boolean;
  data: unknown;
  error?: string | null;
  error_code?: string | null;
  duration_ms: number;
  receipt_id: string;
  permission: string;
}

export type PermissionDecision = "allow_once" | "always_allow" | "deny";

/**
 * 界面直呼工具的粘合层。
 *
 * 为什么需要它：`run_ui_tool` 走的是与对话完全相同的三层闸门，
 * 遇到 MEDIUM 及以上的操作会发 `permission-request` 事件并**阻塞等待**
 * 前端回应（超时 30 秒自动拒绝）。页面自己 invoke 却不监听这个事件，
 * 就会表现为"点了按钮转半分钟然后失败"。
 *
 * 这里统一监听、统一渲染确认框，页面只关心 run() 的返回值。
 */
export function useUiTool() {
  const [pending, setPending] = useState<PermissionRequest | null>(null);
  const [running, setRunning] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const respondingRef = useRef(false);

  useEffect(() => {
    let unlisten: (() => void) | null = null;
    listen<PermissionRequest>("permission-request", (event) => {
      const payload = event.payload;
      // 只接管界面发起的调用；对话页有它自己的一套确认框
      if (payload.session_id !== UI_SESSION) return;
      setPending(payload);
    }).then((fn) => {
      unlisten = fn;
    });
    return () => {
      if (unlisten) unlisten();
    };
  }, []);

  const respond = useCallback(async (decision: PermissionDecision) => {
    if (!pending || respondingRef.current) return;
    respondingRef.current = true;
    try {
      await invoke("respond_permission", {
        response: { request_id: pending.request_id, decision },
      });
    } catch (e) {
      // 请求超时或已被处理：把错误抛给调用方看到，而不是静默吞掉
      setError(typeof e === "string" ? e : "权限确认失败");
    } finally {
      setPending(null);
      respondingRef.current = false;
    }
  }, [pending]);

  const run = useCallback(
    async (tool: string, args: Record<string, unknown> = {}): Promise<UiToolResult | null> => {
      setRunning(tool);
      setError(null);
      try {
        const res = await invoke<UiToolResult>("run_ui_tool", {
          toolName: tool,
          arguments: args,
        });
        if (!res.success) {
          setError(res.error ?? `${tool} 执行失败`);
        }
        return res;
      } catch (e) {
        setError(typeof e === "string" ? e : `${tool} 调用失败`);
        return null;
      } finally {
        setRunning(null);
        setPending(null);
      }
    },
    []
  );

  return { pending, running, error, run, respond, clearError: () => setError(null) };
}
