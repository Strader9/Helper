import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";

/**
 * 自动化面板：常驻定时任务 + 多端设备配对。
 *
 * 为什么这两块放在同一个 Tab：它们都是"应用在我不看着的时候替我做事"的
 * 授权入口——定时任务授权它到点自己跑，设备配对授权另一台机器替我下指令。
 * 放在一起，用户一眼能看到"我给了谁多大的权"。
 */

interface ScheduledJob {
  id: string;
  name: string;
  kind: string;
  schedule: string;
  goal: string;
  enabled: boolean;
  next_run_at: string | null;
  last_run_at: string | null;
  run_count: number;
  max_runs: number | null;
  last_status: string | null;
  last_summary: string | null;
  last_error: string | null;
  created_at: string;
  updated_at: string;
}

interface JobRun {
  id: string;
  job_id: string;
  session_id: string | null;
  started_at: string;
  finished_at: string | null;
  status: string;
  summary: string | null;
  error: string | null;
  steps: number;
  tools_used: string[];
}

interface Device {
  id: string;
  name: string;
  platform: string | null;
  token_hint: string | null;
  paired_at: string;
  last_seen_at: string | null;
  revoked: boolean;
}

interface GatewayStatus {
  running: boolean;
  bound: string | null;
  last_error: string | null;
  requests: number;
}

interface PairingInfo {
  code: string;
  expires_at: string;
  gateway_running: boolean;
  endpoint: string | null;
}

const KIND_LABEL: Record<string, string> = {
  interval: "固定间隔",
  cron: "按日程",
  once: "一次性",
};

const SCHEDULE_HINT: Record<string, string> = {
  interval: "30m / 2h / 90（纯数字按秒）",
  cron: "0 9 * * 1-5 或「每天 09:00」「每周一 08:30」",
  once: "2026-11-01 09:00",
};

const STATUS_COLOR: Record<string, string> = {
  ok: "var(--success-color)",
  running: "var(--neon-blue)",
  failed: "var(--danger-color)",
};

const cardStyle: React.CSSProperties = {
  border: "1px solid var(--border-color)",
  borderRadius: 10,
  padding: 16,
  marginBottom: 16,
  background: "var(--bg-panel)",
};

const labelStyle: React.CSSProperties = {
  display: "block",
  fontSize: 12,
  color: "var(--text-muted)",
  marginBottom: 4,
};

const inputStyle: React.CSSProperties = {
  width: "100%",
  padding: "6px 10px",
  borderRadius: 6,
  border: "1px solid var(--border-color)",
  background: "var(--bg-secondary)",
  color: "var(--text-primary)",
  fontSize: 13,
};

const btnStyle: React.CSSProperties = {
  padding: "5px 12px",
  borderRadius: 6,
  border: "1px solid var(--border-color)",
  background: "var(--bg-tertiary)",
  color: "var(--text-primary)",
  fontSize: 12,
  cursor: "pointer",
};

const primaryBtnStyle: React.CSSProperties = {
  ...btnStyle,
  background: "var(--accent-color)",
  borderColor: "var(--accent-color)",
  color: "#fff",
};

function SettingsAutomation() {
  const [jobs, setJobs] = useState<ScheduledJob[]>([]);
  const [devices, setDevices] = useState<Device[]>([]);
  const [gateway, setGateway] = useState<GatewayStatus | null>(null);
  const [pairing, setPairing] = useState<PairingInfo | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  // 新建表单
  const [name, setName] = useState("");
  const [kind, setKind] = useState("cron");
  const [schedule, setSchedule] = useState("每天 09:00");
  const [goal, setGoal] = useState("");
  const [maxRuns, setMaxRuns] = useState("");

  // 选中的任务及其历史
  const [selectedJob, setSelectedJob] = useState<string | null>(null);
  const [runs, setRuns] = useState<JobRun[]>([]);
  const [runsLoading, setRunsLoading] = useState(false);

  async function refresh() {
    try {
      const [jobList, deviceList, status] = await Promise.all([
        invoke<ScheduledJob[]>("list_scheduled_jobs"),
        invoke<Device[]>("list_paired_devices"),
        invoke<GatewayStatus>("get_gateway_status"),
      ]);
      setJobs(jobList);
      setDevices(deviceList);
      setGateway(status);
      setError(null);
    } catch (e) {
      setError(String(e));
    }
  }

  useEffect(() => {
    refresh();
  }, []);

  async function createJob() {
    if (!name.trim() || !goal.trim() || !schedule.trim()) {
      setError("名称、时间表达式、目标都要填");
      return;
    }
    setBusy(true);
    try {
      await invoke("create_scheduled_job", {
        name: name.trim(),
        kind,
        schedule: schedule.trim(),
        goal: goal.trim(),
        maxRuns: maxRuns.trim() ? Number(maxRuns.trim()) : null,
      });
      setName("");
      setGoal("");
      setMaxRuns("");
      setError(null);
      await refresh();
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  }

  async function toggleJob(job: ScheduledJob) {
    try {
      await invoke("set_scheduled_job_enabled", { id: job.id, enabled: !job.enabled });
      await refresh();
    } catch (e) {
      setError(String(e));
    }
  }

  async function runJobNow(job: ScheduledJob) {
    try {
      await invoke("run_scheduled_job_now", { id: job.id });
      setError(null);
    } catch (e) {
      setError(String(e));
    }
  }

  async function removeJob(job: ScheduledJob) {
    if (!confirm(`删除定时任务「${job.name}」？运行历史会一起删除。`)) return;
    try {
      await invoke("delete_scheduled_job", { id: job.id });
      if (selectedJob === job.id) {
        setSelectedJob(null);
        setRuns([]);
      }
      await refresh();
    } catch (e) {
      setError(String(e));
    }
  }

  async function loadRuns(job: ScheduledJob) {
    setSelectedJob(job.id);
    setRunsLoading(true);
    try {
      const list = await invoke<JobRun[]>("get_scheduled_job_runs", { id: job.id, limit: 20 });
      setRuns(list);
      setError(null);
    } catch (e) {
      setError(String(e));
    } finally {
      setRunsLoading(false);
    }
  }

  async function makePairingCode() {
    try {
      const info = await invoke<PairingInfo>("create_pairing_code");
      setPairing(info);
      setError(null);
    } catch (e) {
      setError(String(e));
    }
  }

  async function revoke(device: Device) {
    if (!confirm(`撤销「${device.name}」的访问权？它的令牌会立刻失效。`)) return;
    try {
      await invoke("revoke_paired_device", { id: device.id });
      await refresh();
    } catch (e) {
      setError(String(e));
    }
  }

  return (
    <div>
      {error && (
        <div style={{ ...cardStyle, borderColor: "var(--danger-color)", color: "var(--danger-color)" }}>
          {error}
        </div>
      )}

      {/* ---------- 定时任务 ---------- */}
      <div style={cardStyle}>
        <div style={{ display: "flex", justifyContent: "space-between", alignItems: "center", marginBottom: 12 }}>
          <div>
            <div style={{ fontSize: 15, fontWeight: 600 }}>定时任务</div>
            <div style={{ fontSize: 12, color: "var(--text-muted)", marginTop: 2 }}>
              到点自动跑一次完整的助手循环。无人值守执行，需要确认的操作会被跳过。
            </div>
          </div>
          <button style={btnStyle} onClick={refresh}>刷新</button>
        </div>

        {jobs.length === 0 ? (
          <div style={{ fontSize: 13, color: "var(--text-muted)", padding: "8px 0" }}>
            还没有定时任务。也可以在对话里直接说「每天早上九点帮我汇总昨天的待办」。
          </div>
        ) : (
          <div style={{ overflowX: "auto" }}>
            <table style={{ width: "100%", borderCollapse: "collapse", fontSize: 13 }}>
              <thead>
                <tr style={{ color: "var(--text-muted)", textAlign: "left" }}>
                  <th style={{ padding: "6px 8px" }}>名称</th>
                  <th style={{ padding: "6px 8px" }}>节奏</th>
                  <th style={{ padding: "6px 8px" }}>下次</th>
                  <th style={{ padding: "6px 8px" }}>上次</th>
                  <th style={{ padding: "6px 8px" }}>次数</th>
                  <th style={{ padding: "6px 8px" }}>操作</th>
                </tr>
              </thead>
              <tbody>
                {jobs.map((job) => (
                  <tr key={job.id} style={{ borderTop: "1px solid var(--border-light)", opacity: job.enabled ? 1 : 0.55 }}>
                    <td style={{ padding: "8px" }}>
                      <div style={{ fontWeight: 500 }}>{job.name}</div>
                      <div style={{ fontSize: 11, color: "var(--text-muted)" }}>{job.goal}</div>
                    </td>
                    <td style={{ padding: "8px" }}>
                      <div>{KIND_LABEL[job.kind] ?? job.kind}</div>
                      <div style={{ fontSize: 11, color: "var(--text-muted)" }}>{job.schedule}</div>
                    </td>
                    <td style={{ padding: "8px" }}>{job.next_run_at ?? "—"}</td>
                    <td style={{ padding: "8px" }}>
                      <span style={{ color: STATUS_COLOR[job.last_status ?? ""] ?? "var(--text-secondary)" }}>
                        {job.last_status ?? "未运行"}
                      </span>
                    </td>
                    <td style={{ padding: "8px" }}>
                      {job.run_count}
                      {job.max_runs ? `/${job.max_runs}` : ""}
                    </td>
                    <td style={{ padding: "8px", whiteSpace: "nowrap" }}>
                      <button style={btnStyle} onClick={() => loadRuns(job)}>历史</button>{" "}
                      <button style={btnStyle} onClick={() => runJobNow(job)} disabled={!job.enabled}>立即</button>{" "}
                      <button style={btnStyle} onClick={() => toggleJob(job)}>
                        {job.enabled ? "暂停" : "启用"}
                      </button>{" "}
                      <button
                        style={{ ...btnStyle, color: "var(--danger-color)" }}
                        onClick={() => removeJob(job)}
                      >
                        删除
                      </button>
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        )}

        {/* 新建表单 */}
        <div style={{ marginTop: 16, borderTop: "1px solid var(--border-light)", paddingTop: 12 }}>
          <div style={{ fontSize: 13, fontWeight: 600, marginBottom: 8 }}>新建任务</div>
          <div style={{ display: "grid", gridTemplateColumns: "1fr 1fr", gap: 10 }}>
            <div>
              <label style={labelStyle}>名称</label>
              <input style={inputStyle} value={name} onChange={(e) => setName(e.target.value)} placeholder="晨间简报" />
            </div>
            <div>
              <label style={labelStyle}>类型</label>
              <select style={inputStyle} value={kind} onChange={(e) => setKind(e.target.value)}>
                <option value="cron">按日程（每天/每周）</option>
                <option value="interval">固定间隔</option>
                <option value="once">一次性</option>
              </select>
            </div>
            <div>
              <label style={labelStyle}>时间表达式</label>
              <input
                style={inputStyle}
                value={schedule}
                onChange={(e) => setSchedule(e.target.value)}
                placeholder={SCHEDULE_HINT[kind]}
              />
              <div style={{ fontSize: 11, color: "var(--text-muted)", marginTop: 4 }}>{SCHEDULE_HINT[kind]}</div>
            </div>
            <div>
              <label style={labelStyle}>最多执行次数（留空不限）</label>
              <input
                style={inputStyle}
                value={maxRuns}
                onChange={(e) => setMaxRuns(e.target.value)}
                placeholder="留空"
              />
            </div>
          </div>
          <div style={{ marginTop: 10 }}>
            <label style={labelStyle}>任务目标（交给助手去完成的事）</label>
            <textarea
              style={{ ...inputStyle, minHeight: 64, resize: "vertical" }}
              value={goal}
              onChange={(e) => setGoal(e.target.value)}
              placeholder="汇总昨天改过的文件，列出今天要跟进的三件事"
            />
          </div>
          <div style={{ marginTop: 10 }}>
            <button style={primaryBtnStyle} onClick={createJob} disabled={busy}>
              {busy ? "创建中…" : "创建任务"}
            </button>
          </div>
        </div>
      </div>

      {/* ---------- 运行历史 ---------- */}
      {selectedJob && (
        <div style={cardStyle}>
          <div style={{ display: "flex", justifyContent: "space-between", alignItems: "center", marginBottom: 8 }}>
            <div style={{ fontSize: 14, fontWeight: 600 }}>运行历史</div>
            <button style={btnStyle} onClick={() => { setSelectedJob(null); setRuns([]); }}>收起</button>
          </div>
          {runsLoading ? (
            <div style={{ fontSize: 13, color: "var(--text-muted)" }}>加载中…</div>
          ) : runs.length === 0 ? (
            <div style={{ fontSize: 13, color: "var(--text-muted)" }}>还没有运行记录。</div>
          ) : (
            <div style={{ display: "grid", gap: 8 }}>
              {runs.map((run) => (
                <div key={run.id} style={{ border: "1px solid var(--border-light)", borderRadius: 8, padding: 10 }}>
                  <div style={{ display: "flex", gap: 12, fontSize: 12, color: "var(--text-muted)" }}>
                    <span style={{ color: STATUS_COLOR[run.status] ?? "var(--text-secondary)", fontWeight: 600 }}>
                      {run.status}
                    </span>
                    <span>{run.started_at}</span>
                    <span>{run.steps} 步</span>
                    {run.tools_used.length > 0 && <span>{run.tools_used.join(" · ")}</span>}
                  </div>
                  {run.summary && (
                    <div style={{ fontSize: 13, marginTop: 6, whiteSpace: "pre-wrap" }}>{run.summary}</div>
                  )}
                  {run.error && (
                    <div style={{ fontSize: 12, marginTop: 6, color: "var(--danger-color)" }}>{run.error}</div>
                  )}
                </div>
              ))}
            </div>
          )}
        </div>
      )}

      {/* ---------- 多端设备 ---------- */}
      <div style={cardStyle}>
        <div style={{ display: "flex", justifyContent: "space-between", alignItems: "center", marginBottom: 8 }}>
          <div>
            <div style={{ fontSize: 15, fontWeight: 600 }}>多端设备</div>
            <div style={{ fontSize: 12, color: "var(--text-muted)", marginTop: 2 }}>
              {gateway?.running
                ? `网关运行中：${gateway.bound ?? "未知地址"}（已处理 ${gateway.requests} 个请求）`
                : gateway?.last_error
                  ? `网关未启动：${gateway.last_error}`
                  : "网关未启动"}
            </div>
          </div>
          <button style={primaryBtnStyle} onClick={makePairingCode}>生成配对码</button>
        </div>

        {pairing && (
          <div
            style={{
              border: "1px dashed var(--accent-color)",
              borderRadius: 8,
              padding: 12,
              marginBottom: 12,
              background: "var(--bg-secondary)",
            }}
          >
            <div style={{ fontSize: 12, color: "var(--text-muted)" }}>在另一台设备上输入这个配对码：</div>
            <div style={{ fontSize: 30, letterSpacing: 8, fontWeight: 700, fontFamily: "monospace", margin: "6px 0" }}>
              {pairing.code}
            </div>
            <div style={{ fontSize: 12, color: "var(--text-muted)" }}>
              {pairing.expires_at} 前有效 · 只能用一次 · 端点 {pairing.endpoint ?? "网关未监听（检查端口占用）"}
            </div>
            {!pairing.gateway_running && (
              <div style={{ fontSize: 12, color: "var(--danger-color)", marginTop: 6 }}>
                网关没在跑，配对码暂时用不了：检查「多端访问」配置里的端口是否被占用。
              </div>
            )}
          </div>
        )}

        {devices.length === 0 ? (
          <div style={{ fontSize: 13, color: "var(--text-muted)" }}>还没有配对设备。</div>
        ) : (
          <div style={{ display: "grid", gap: 8 }}>
            {devices.map((device) => (
              <div
                key={device.id}
                style={{
                  display: "flex",
                  justifyContent: "space-between",
                  alignItems: "center",
                  border: "1px solid var(--border-light)",
                  borderRadius: 8,
                  padding: "8px 12px",
                  opacity: device.revoked ? 0.5 : 1,
                }}
              >
                <div>
                  <div style={{ fontSize: 13, fontWeight: 500 }}>
                    {device.name}
                    {device.revoked && (
                      <span style={{ fontSize: 11, color: "var(--danger-color)", marginLeft: 8 }}>已撤销</span>
                    )}
                  </div>
                  <div style={{ fontSize: 11, color: "var(--text-muted)" }}>
                    {device.platform ?? "未知平台"} · 配对于 {device.paired_at}
                    {device.last_seen_at ? ` · 最近活跃 ${device.last_seen_at}` : " · 从未连接"}
                    {device.token_hint ? ` · 令牌尾号 ${device.token_hint}` : ""}
                  </div>
                </div>
                {!device.revoked && (
                  <button
                    style={{ ...btnStyle, color: "var(--danger-color)" }}
                    onClick={() => revoke(device)}
                  >
                    撤销
                  </button>
                )}
              </div>
            ))}
          </div>
        )}
      </div>
    </div>
  );
}

export default SettingsAutomation;
