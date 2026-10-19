import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";

/**
 * 模型网关面板：展示 sidecar 的多 provider 健康情况。
 *
 * 这一页是"只读诊断"，不是配置编辑器。provider 列表来自 sidecar 的启动配置
 * （SIDECAR_PROVIDERS 环境变量），运行时改不了——与其在这里做一个改完不生效的
 * 表单，不如把真实状态摊开：谁健康、谁在失败、延迟多少、切过几次。
 */

interface Provider {
  id: string;
  name: string;
  model: string;
  protocol: string;
  baseUrl: string;
  hasApiKey: boolean;
  healthy: boolean;
  consecutiveFailures: number;
  lastError: string | null;
  lastLatencyMs: number | null;
  totalCalls: number;
  usage: { promptTokens: number; completionTokens: number } | null;
}

interface GatewayProbe {
  reachable: boolean;
  base_url: string;
  http_status?: number;
  providers?: Provider[];
  error?: string | null;
}

const cardStyle: React.CSSProperties = {
  border: "1px solid var(--border-color)",
  borderRadius: 10,
  padding: 16,
  marginBottom: 16,
  background: "var(--bg-panel)",
};

const thStyle: React.CSSProperties = {
  padding: "6px 8px",
  textAlign: "left",
  color: "var(--text-muted)",
  fontWeight: 500,
  fontSize: 12,
};

const tdStyle: React.CSSProperties = {
  padding: "8px",
  fontSize: 13,
  borderTop: "1px solid var(--border-light)",
};

function SettingsModel() {
  const [probe, setProbe] = useState<GatewayProbe | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);

  async function refresh() {
    setLoading(true);
    try {
      const result = await invoke<GatewayProbe>("get_model_gateway_status");
      setProbe(result);
      setError(null);
    } catch (e) {
      setError(String(e));
    } finally {
      setLoading(false);
    }
  }

  useEffect(() => {
    refresh();
  }, []);

  const providers = probe?.providers ?? [];

  return (
    <div>
      {error && (
        <div style={{ ...cardStyle, borderColor: "var(--danger-color)", color: "var(--danger-color)" }}>{error}</div>
      )}

      <div style={cardStyle}>
        <div style={{ display: "flex", justifyContent: "space-between", alignItems: "flex-start" }}>
          <div>
            <div style={{ fontSize: 15, fontWeight: 600 }}>模型网关（sidecar）</div>
            <div style={{ fontSize: 12, color: "var(--text-muted)", marginTop: 2 }}>
              多 provider 优先级、失败自动切换、健康与 token 计量都在这一层。
              地址来自「通用 → 模型连接」旁的 sidecar.url 配置。
            </div>
          </div>
          <button
            onClick={refresh}
            style={{
              padding: "5px 12px",
              borderRadius: 6,
              border: "1px solid var(--border-color)",
              background: "var(--bg-tertiary)",
              color: "var(--text-primary)",
              fontSize: 12,
              cursor: "pointer",
            }}
          >
            {loading ? "检测中…" : "重新检测"}
          </button>
        </div>

        <div style={{ marginTop: 12, fontSize: 13 }}>
          <div>
            地址：<code>{probe?.base_url ?? "—"}</code>
          </div>
          <div style={{ marginTop: 4 }}>
            状态：
            {probe?.reachable ? (
              <span style={{ color: "var(--success-color)", fontWeight: 600 }}>可达</span>
            ) : (
              <span style={{ color: "var(--danger-color)", fontWeight: 600 }}>
                不可达{probe?.error ? ` · ${probe.error}` : ""}
              </span>
            )}
          </div>
        </div>

        {!probe?.reachable && (
          <div style={{ marginTop: 10, fontSize: 12, color: "var(--text-muted)", lineHeight: 1.7 }}>
            网关没起来时，助手会回落到直连 Ollama（即「通用 → 模型连接」里的地址）。
            需要多 provider 时，在 sidecar 目录执行 <code>npm start</code>，
            或设置 <code>SIDECAR_PROVIDERS</code> 后重启 sidecar。
          </div>
        )}
      </div>

      {probe?.reachable && (
        <div style={cardStyle}>
          <div style={{ fontSize: 14, fontWeight: 600, marginBottom: 8 }}>
            Provider 列表（按优先级从上到下）
          </div>
          {providers.length === 0 ? (
            <div style={{ fontSize: 13, color: "var(--text-muted)" }}>网关没有返回任何 provider。</div>
          ) : (
            <div style={{ overflowX: "auto" }}>
              <table style={{ width: "100%", borderCollapse: "collapse" }}>
                <thead>
                  <tr>
                    <th style={thStyle}>名称</th>
                    <th style={thStyle}>模型</th>
                    <th style={thStyle}>协议</th>
                    <th style={thStyle}>健康</th>
                    <th style={thStyle}>延迟</th>
                    <th style={thStyle}>调用</th>
                    <th style={thStyle}>token</th>
                  </tr>
                </thead>
                <tbody>
                  {providers.map((provider) => (
                    <tr key={provider.id}>
                      <td style={tdStyle}>
                        <div>{provider.name}</div>
                        <div style={{ fontSize: 11, color: "var(--text-muted)" }}>{provider.baseUrl}</div>
                      </td>
                      <td style={tdStyle}>{provider.model}</td>
                      <td style={tdStyle}>
                        {provider.protocol}
                        {provider.hasApiKey && (
                          <span style={{ fontSize: 11, color: "var(--text-muted)" }}> · 带密钥</span>
                        )}
                      </td>
                      <td style={tdStyle}>
                        <span
                          style={{
                            color: provider.healthy ? "var(--success-color)" : "var(--danger-color)",
                            fontWeight: 600,
                          }}
                        >
                          {provider.healthy ? "正常" : `连续失败 ${provider.consecutiveFailures}`}
                        </span>
                        {provider.lastError && (
                          <div style={{ fontSize: 11, color: "var(--text-muted)" }}>{provider.lastError}</div>
                        )}
                      </td>
                      <td style={tdStyle}>
                        {provider.lastLatencyMs != null ? `${provider.lastLatencyMs} ms` : "—"}
                      </td>
                      <td style={tdStyle}>{provider.totalCalls}</td>
                      <td style={tdStyle}>
                        {provider.usage
                          ? `${provider.usage.promptTokens} / ${provider.usage.completionTokens}`
                          : "—"}
                      </td>
                    </tr>
                  ))}
                </tbody>
              </table>
            </div>
          )}
          <div style={{ marginTop: 10, fontSize: 12, color: "var(--text-muted)" }}>
            排在前面的 provider 先被尝试；失败后自动切下一个，健康状态会在连续失败后翻转。
            provider 的增删改在 sidecar 的启动配置里，这里只读。
          </div>
        </div>
      )}
    </div>
  );
}

export default SettingsModel;
