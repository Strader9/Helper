import { useState } from "react";
import { invoke } from "@tauri-apps/api/core";

/**
 * GitHub 趋势页面
 *
 * 调用后端 `fetch_github_trending` 拉取近期活跃热门仓库，
 * 并将结果保存为趋势快照（github_snapshots 表）。
 */

interface Repo {
  name: string | null;
  full_name: string | null;
  description: string | null;
  html_url: string | null;
  stars: number | null;
  forks: number | null;
  language: string | null;
  owner: string | null;
  updated_at: string | null;
}

interface TrendingResponse {
  repos: Repo[];
  count: number;
  range: string;
}

const RANGES = [
  { key: "daily", label: "每日" },
  { key: "weekly", label: "每周" },
  { key: "monthly", label: "每月" },
];

function GitHub() {
  const [range, setRange] = useState("daily");
  const [language, setLanguage] = useState("");
  const [limit] = useState(20);
  const [repos, setRepos] = useState<Repo[]>([]);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [lastRange, setLastRange] = useState<string | null>(null);

  async function handleFetch() {
    setLoading(true);
    setError(null);
    try {
      const resp = await invoke<TrendingResponse>("fetch_github_trending", {
        range,
        language: language.trim() || null,
        limit,
      });
      setRepos(resp.repos);
      setLastRange(resp.range);
    } catch (e) {
      setError(String(e));
    } finally {
      setLoading(false);
    }
  }

  return (
    <div className="page-container">
      <header className="page-header">
        <h1>GitHub 趋势</h1>
        <p className="page-subtitle">近期活跃的热门开源仓库</p>
      </header>

      <div className="section-card">
        <div className="logs-toolbar">
          <div className="filter-group">
            <span>范围：</span>
            {RANGES.map((r) => (
              <button
                key={r.key}
                className={`filter-btn ${range === r.key ? "active" : ""}`}
                onClick={() => setRange(r.key)}
              >
                {r.label}
              </button>
            ))}
            <span style={{ marginLeft: "1rem" }}>语言：</span>
            <input
              type="text"
              value={language}
              placeholder="如 rust / python（可选）"
              onChange={(e) => setLanguage(e.target.value)}
              style={{
                padding: "0.3rem 0.5rem",
                borderRadius: 6,
                border: "1px solid #444",
                background: "#1e1e1e",
                color: "#ddd",
                width: "10rem",
              }}
            />
            <button
              className="filter-btn active"
              onClick={handleFetch}
              disabled={loading}
              style={{ marginLeft: "0.5rem" }}
            >
              {loading ? "拉取中..." : "拉取趋势"}
            </button>
          </div>
        </div>

        {error && (
          <p className="empty-state" style={{ color: "#e06c75" }}>
            拉取失败：{error}
          </p>
        )}

        {!error && repos.length === 0 && !loading && (
          <p className="empty-state">
            暂无数据
            <br />
            <small>点击「拉取趋势」获取 GitHub 热门仓库</small>
          </p>
        )}

        {repos.length > 0 && (
          <div className="logs-body">
            {repos.map((r, i) => (
              <div
                key={i}
                style={{
                  padding: "0.75rem 0",
                  borderBottom: "1px solid #2a2a2a",
                }}
              >
                <div
                  style={{
                    display: "flex",
                    justifyContent: "space-between",
                    alignItems: "baseline",
                    gap: "0.5rem",
                  }}
                >
                  <span style={{ color: "#61afef", fontWeight: 600 }}>
                    {r.full_name || r.name}
                  </span>
                  <span style={{ color: "#98c379", whiteSpace: "nowrap" }}>
                    ★ {r.stars ?? 0}
                  </span>
                </div>
                {r.description && (
                  <div
                    style={{
                      color: "#aaa",
                      fontSize: "0.85rem",
                      marginTop: "0.2rem",
                    }}
                  >
                    {r.description}
                  </div>
                )}
                <div
                  style={{
                    color: "#888",
                    fontSize: "0.78rem",
                    marginTop: "0.25rem",
                  }}
                >
                  {r.language && (
                    <span style={{ marginRight: "0.75rem" }}>{r.language}</span>
                  )}
                  {r.html_url && <span>{r.html_url}</span>}
                </div>
              </div>
            ))}
          </div>
        )}

        {lastRange && repos.length > 0 && (
          <p style={{ color: "#666", fontSize: "0.78rem", marginTop: "0.5rem" }}>
            范围：{lastRange} · 共 {repos.length} 个仓库（已保存趋势快照）
          </p>
        )}
      </div>
    </div>
  );
}

export default GitHub;
