import { useState, useEffect, useCallback } from 'react';
import { listen, type UnlistenFn } from '@tauri-apps/api/event';
import { invoke } from '@tauri-apps/api/core';

interface PermissionRequestPayload {
  request_id: string;
  tool_name: string;
  risk_level: string;
  path?: string;
  description?: string;
  arguments?: Record<string, unknown>;
}

type PermissionDecision = 'allow_once' | 'always_allow' | 'deny';

const RISK_STYLES: Record<string, { label: string; color: string; bg: string; border: string }> = {
  HIGH: { label: '高风险', color: '#f87171', bg: 'rgba(248,113,113,0.1)', border: 'rgba(248,113,113,0.3)' },
  MEDIUM: { label: '中风险', color: '#fbbf24', bg: 'rgba(251,191,36,0.1)', border: 'rgba(251,191,36,0.3)' },
  LOW: { label: '低风险', color: '#34d399', bg: 'rgba(52,211,153,0.1)', border: 'rgba(52,211,153,0.3)' },
  CRITICAL: { label: '极高风险', color: '#ef4444', bg: 'rgba(239,68,68,0.15)', border: 'rgba(239,68,68,0.4)' },
};

/**
 * 全局权限确认弹窗。
 * 监听后端 permission-request 事件，在任何页面都能弹出。
 * "始终允许"会被后端保存到 trusted_tools，下次同工具自动放行。
 */
export function EAssistantPermissionGate() {
  const [request, setRequest] = useState<PermissionRequestPayload | null>(null);
  const [responding, setResponding] = useState(false);
  const [unlisten, setUnlisten] = useState<UnlistenFn | null>(null);

  useEffect(() => {
    let cancelled = false;

    const setup = async () => {
      try {
        const fn = await listen<PermissionRequestPayload>('permission-request', (event) => {
          if (cancelled) return;
          console.log('[PermissionGate] 收到权限请求:', event.payload);
          setRequest(event.payload);
        });
        if (!cancelled) setUnlisten(() => fn);
      } catch (e) {
        console.error('[PermissionGate] 监听失败:', e);
      }
    };

    setup();

    return () => {
      cancelled = true;
      if (unlisten) {
        try { unlisten(); } catch { /* ignore */ }
      }
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  const respond = useCallback(async (decision: PermissionDecision) => {
    if (!request || responding) return;
    setResponding(true);
    try {
      await invoke('respond_permission', {
        response: { request_id: request.request_id, decision },
      });
      console.log('[PermissionGate] 已响应:', decision);
    } catch (e) {
      console.error('[PermissionGate] 响应失败:', e);
    } finally {
      setRequest(null);
      setResponding(false);
    }
  }, [request, responding]);

  if (!request) return null;

  const risk = (request.risk_level || 'MEDIUM').toUpperCase();
  const style = RISK_STYLES[risk] || RISK_STYLES.MEDIUM;

  return (
    <div className="fixed inset-0 z-[9999] flex items-center justify-center" style={{ background: 'rgba(0,0,0,0.6)', backdropFilter: 'blur(4px)' }}>
      <div
        className="w-[440px] max-w-[90vw] rounded-2xl p-6"
        style={{
          background: 'linear-gradient(135deg, rgba(20,25,50,0.95), rgba(10,14,30,0.98))',
          border: `1px solid ${style.border}`,
          boxShadow: `0 0 40px ${style.bg}, 0 20px 60px rgba(0,0,0,0.5)`,
        }}
      >
        {/* 头部 */}
        <div className="mb-4 flex items-center gap-3">
          <div
            className="flex h-11 w-11 items-center justify-center rounded-xl text-xl"
            style={{ background: style.bg, border: `1px solid ${style.border}` }}
          >
            ⚠️
          </div>
          <div>
            <div className="text-lg font-bold text-white">需要你确认</div>
            <div className="text-xs text-white/50">此操作会产生实际影响，请确认来源可信</div>
          </div>
        </div>

        {/* 风险标签 */}
        <div className="mb-4 flex items-center gap-2">
          <span
            className="rounded-full px-3 py-1 text-xs font-semibold"
            style={{ background: style.bg, color: style.color, border: `1px solid ${style.border}` }}
          >
            {style.label}
          </span>
          <span className="font-mono text-sm text-white/80">{request.tool_name}</span>
        </div>

        {/* 详情 */}
        <div className="mb-5 space-y-2 rounded-xl border border-white/10 bg-black/30 p-3">
          {request.path && (
            <div className="flex gap-2 text-xs">
              <span className="w-12 shrink-0 text-white/40">路径</span>
              <span className="break-all font-mono text-white/70">{request.path}</span>
            </div>
          )}
          {request.description && (
            <div className="flex gap-2 text-xs">
              <span className="w-12 shrink-0 text-white/40">说明</span>
              <span className="text-white/70">{request.description}</span>
            </div>
          )}
          {request.arguments && Object.keys(request.arguments).length > 0 && (
            <div className="flex gap-2 text-xs">
              <span className="w-12 shrink-0 text-white/40">参数</span>
              <span className="break-all font-mono text-white/50">
                {JSON.stringify(request.arguments).slice(0, 120)}
                {JSON.stringify(request.arguments).length > 120 ? '...' : ''}
              </span>
            </div>
          )}
        </div>

        {/* 按钮 */}
        <div className="flex gap-2">
          <button
            onClick={() => respond('deny')}
            disabled={responding}
            className="flex-1 rounded-xl border border-white/10 bg-white/5 py-2.5 text-sm font-medium text-white/60 transition hover:bg-white/10 hover:text-white disabled:opacity-40"
          >
            拒绝
          </button>
          <button
            onClick={() => respond('allow_once')}
            disabled={responding}
            className="flex-1 rounded-xl py-2.5 text-sm font-semibold text-white transition hover:opacity-90 disabled:opacity-40"
            style={{ background: 'linear-gradient(135deg, #0ea5e9, #0284c7)' }}
          >
            允许一次
          </button>
          <button
            onClick={() => respond('always_allow')}
            disabled={responding}
            className="flex-1 rounded-xl py-2.5 text-sm font-semibold text-white transition hover:opacity-90 disabled:opacity-40"
            style={{ background: 'linear-gradient(135deg, #22c55e, #16a34a)' }}
          >
            始终允许
          </button>
        </div>

        <div className="mt-3 text-center text-[10px] text-white/30">
          选择"始终允许"后，该工具下次将自动执行，不再询问
        </div>
      </div>
    </div>
  );
}
