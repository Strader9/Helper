import { useState, useEffect, useCallback } from "react";
import { invoke } from "@tauri-apps/api/core";
import CyberIcon from "../components/CyberIcon";

// ============================================================
// 类型定义
// ============================================================

interface DiskInfo {
  drive: string;
  total_gb: number;
  used_gb: number;
  usage_percent: number;
}

interface SystemMetrics {
  cpu_usage: number;
  memory_total_gb: number;
  memory_used_gb: number;
  memory_usage_percent: number;
  disks: DiskInfo[];
  network_ok: boolean;
  gpu_name: string | null;
  gpu_usage: number | null;
}

interface AlertItem {
  type: "warning" | "critical";
  title: string;
  message: string;
}

// ============================================================
// 辅助组件：环形进度条
// ============================================================

function RingChart({
  value,
  size = 120,
  strokeWidth = 10,
  label,
  subLabel,
  color,
}: {
  value: number;
  size?: number;
  strokeWidth?: number;
  label: string;
  subLabel?: string;
  color?: string;
}) {
  const radius = (size - strokeWidth) / 2;
  const circumference = 2 * Math.PI * radius;
  const offset = circumference - (Math.min(value, 100) / 100) * circumference;

  const getColor = () => {
    if (color) return color;
    if (value >= 90) return "#ff3355";
    if (value >= 75) return "#ffaa00";
    return "#ff8c00";
  };

  return (
    <div className="ring-chart">
      <svg width={size} height={size}>
        <circle
          cx={size / 2}
          cy={size / 2}
          r={radius}
          fill="none"
          stroke="var(--border-color)"
          strokeWidth={strokeWidth}
        />
        <circle
          cx={size / 2}
          cy={size / 2}
          r={radius}
          fill="none"
          stroke={getColor()}
          strokeWidth={strokeWidth}
          strokeLinecap="round"
          strokeDasharray={circumference}
          strokeDashoffset={offset}
          transform={`rotate(-90 ${size / 2} ${size / 2})`}
          style={{ transition: "stroke-dashoffset 0.5s ease" }}
        />
        <text
          x="50%"
          y="45%"
          textAnchor="middle"
          dominantBaseline="middle"
          fill="var(--text-primary)"
          fontSize="18"
          fontWeight="600"
        >
          {value.toFixed(1)}%
        </text>
        <text
          x="50%"
          y="65%"
          textAnchor="middle"
          dominantBaseline="middle"
          fill="var(--text-secondary)"
          fontSize="10"
        >
          {label}
        </text>
      </svg>
      {subLabel && <div className="ring-sub-label">{subLabel}</div>}
    </div>
  );
}

// ============================================================
// 主组件
// ============================================================

function Dashboard() {
  const [metrics, setMetrics] = useState<SystemMetrics | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [lastUpdate, setLastUpdate] = useState<Date | null>(null);

  const fetchMetrics = useCallback(async () => {
    try {
      const data = await invoke<SystemMetrics>("get_system_metrics");
      setMetrics(data);
      setError(null);
      setLastUpdate(new Date());
    } catch (e) {
      console.error("Failed to fetch system metrics:", e);
      setError(typeof e === "string" ? e : "获取系统指标失败");
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    fetchMetrics();
    const interval = setInterval(fetchMetrics, 3000);
    return () => clearInterval(interval);
  }, [fetchMetrics]);

  // 生成告警
  const alerts: AlertItem[] = [];
  if (metrics) {
    if (metrics.cpu_usage >= 90) {
      alerts.push({
        type: "critical",
        title: "CPU 使用率过高",
        message: `当前 CPU 使用率为 ${metrics.cpu_usage.toFixed(1)}%，建议检查高负载进程`,
      });
    } else if (metrics.cpu_usage >= 75) {
      alerts.push({
        type: "warning",
        title: "CPU 使用率偏高",
        message: `当前 CPU 使用率为 ${metrics.cpu_usage.toFixed(1)}%`,
      });
    }

    if (metrics.memory_usage_percent >= 90) {
      alerts.push({
        type: "critical",
        title: "内存使用率过高",
        message: `已使用 ${metrics.memory_used_gb.toFixed(1)} GB / ${metrics.memory_total_gb.toFixed(1)} GB (${metrics.memory_usage_percent.toFixed(1)}%)`,
      });
    } else if (metrics.memory_usage_percent >= 80) {
      alerts.push({
        type: "warning",
        title: "内存使用率偏高",
        message: `已使用 ${metrics.memory_used_gb.toFixed(1)} GB / ${metrics.memory_total_gb.toFixed(1)} GB`,
      });
    }

    if (!metrics.network_ok) {
      alerts.push({
        type: "critical",
        title: "网络连接异常",
        message: "无法连接到外部网络，请检查网络设置",
      });
    }

    const fullDisk = metrics.disks.find((d) => d.usage_percent >= 95);
    if (fullDisk) {
      alerts.push({
        type: "critical",
        title: `磁盘 ${fullDisk.drive} 即将满`,
        message: `已使用 ${fullDisk.usage_percent.toFixed(1)}%，剩余空间不足`,
      });
    }
  }

  if (loading) {
    return (
      <div className="page-container dashboard-loading">
        <div className="loading-spinner" />
        <p>正在加载系统状态...</p>
      </div>
    );
  }

  if (error) {
    return (
      <div className="page-container dashboard-error">
        <div className="error-icon"><CyberIcon name="alertTriangle" size={14} /></div>
        <h3>加载失败</h3>
        <p>{error}</p>
        <button className="retry-btn" onClick={fetchMetrics}>
          重试
        </button>
      </div>
    );
  }

  if (!metrics) {
    return (
      <div className="page-container dashboard-error">
        <p>无法获取系统指标</p>
      </div>
    );
  }

  return (
    <div className="page-container">
      <header className="page-header">
        <h1>仪表盘</h1>
        <p className="page-subtitle">
          实时监控系统状态
          {lastUpdate && (
            <span className="refresh-hint">
              · 上次更新 {lastUpdate.toLocaleTimeString()}
            </span>
          )}
        </p>
      </header>

      {/* 核心指标环形图 */}
      <div className="dashboard-rings">
        <div className="metric-card ring-card">
          <RingChart
            value={metrics.cpu_usage}
            label="CPU"
            subLabel={metrics.cpu_usage >= 90 ? "过高" : metrics.cpu_usage >= 75 ? "偏高" : "正常"}
          />
        </div>
        <div className="metric-card ring-card">
          <RingChart
            value={metrics.memory_usage_percent}
            label="内存"
            subLabel={`${metrics.memory_used_gb.toFixed(1)} / ${metrics.memory_total_gb.toFixed(1)} GB`}
          />
        </div>
        {metrics.gpu_name && metrics.gpu_usage !== null && (
          <div className="metric-card ring-card gpu-ring">
            <RingChart
              value={metrics.gpu_usage}
              label="GPU"
              subLabel={metrics.gpu_name}
              color="#2a7abf"
            />
          </div>
        )}
        <div className="metric-card ring-card network-ring">
          <div className={`network-status ${metrics.network_ok ? "ok" : "error"}`}>
            <div className="network-icon">{metrics.network_ok ? <CyberIcon name="wifi" size={14} /> : <CyberIcon name="xCircle" size={14} />}</div>
            <div className="network-label">
              {metrics.network_ok ? "网络正常" : "网络异常"}
            </div>
            <div className="network-detail">
              {metrics.network_ok ? "已连接到互联网" : "无法连接外部网络"}
            </div>
          </div>
        </div>
      </div>

      {/* 磁盘使用情况 */}
      <div className="section-card">
        <h3>磁盘使用情况</h3>
        {metrics.disks.length === 0 ? (
          <p className="empty-state">无法获取磁盘信息</p>
        ) : (
          <div className="disk-list">
            {metrics.disks.map((disk) => (
              <div key={disk.drive} className="disk-item">
                <div className="disk-info">
                  <span className="disk-drive">{disk.drive}</span>
                  <span className="disk-size">
                    {disk.used_gb.toFixed(1)} GB / {disk.total_gb.toFixed(1)} GB
                  </span>
                </div>
                <div className="disk-bar-bg">
                  <div
                    className={`disk-bar-fill ${
                      disk.usage_percent >= 90
                        ? "critical"
                        : disk.usage_percent >= 75
                        ? "warning"
                        : ""
                    }`}
                    style={{ width: `${Math.min(disk.usage_percent, 100)}%` }}
                  />
                </div>
                <span className="disk-percent">{disk.usage_percent.toFixed(1)}%</span>
              </div>
            ))}
          </div>
        )}
      </div>

      {/* 系统告警 */}
      <div className="section-card">
        <h3>系统告警</h3>
        {alerts.length === 0 ? (
          <p className="empty-state">系统运行正常，暂无告警</p>
        ) : (
          <div className="alert-list">
            {alerts.map((alert, i) => (
              <div key={i} className={`alert-item ${alert.type}`}>
                <span className="alert-icon">
                  {alert.type === "critical" ? "🔴" : "🟡"}
                </span>
                <div className="alert-content">
                  <div className="alert-title">{alert.title}</div>
                  <div className="alert-message">{alert.message}</div>
                </div>
              </div>
            ))}
          </div>
        )}
      </div>
    </div>
  );
}

export default Dashboard;
