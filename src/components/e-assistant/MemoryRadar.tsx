/**
 * MemoryRadar —— 记忆证据构成雷达图
 *
 * 迁移自 EAssistantCore MemoryAvatarLab.MiniRadar，适配 E-Assisant：
 *  - 8 维真实记忆构成（纯 SVG，无外部依赖）
 *  - motion/react 入场动画（多边形从中心缩放展开）
 *  - 网格环 + 轴线 + 顶点 + 标签
 *  - 数值 0~1，由记忆数据映射而来
 */
import { motion } from 'motion/react';

export type RadarDimensions = Record<string, number>;

const DIM_ORDER = [
  'preferences',
  'habits',
  'facts',
  'episodes',
  'explicit',
  'important',
  'recent',
  'used',
] as const;

const DIM_LABELS: Record<string, string> = {
  preferences: '偏好',
  habits: '习惯',
  facts: '事实',
  episodes: '经历',
  explicit: '确认',
  important: '重要',
  recent: '近期',
  used: '常用',
};

const CX = 100;
const CY = 100;
const R = 72;

function vertexFor(angle: number, radius: number) {
  return {
    x: CX + radius * Math.cos(angle),
    y: CY + radius * Math.sin(angle),
  };
}

function angleFor(index: number) {
  return (Math.PI * 2 * index) / DIM_ORDER.length - Math.PI / 2;
}

interface MemoryRadarProps {
  values: RadarDimensions;
  size?: number;
  /** 入场动画延迟（ms） */
  delay?: number;
}

export function MemoryRadar({ values, size = 200, delay = 200 }: MemoryRadarProps) {
  // 数据多边形顶点
  const dataPoints = DIM_ORDER.map((dim, i) => {
    const angle = angleFor(i);
    const val = Math.max(0.06, Math.min(1, values[dim] ?? 0.1));
    return vertexFor(angle, R * val);
  });
  const dataPath = dataPoints.map((p) => `${p.x},${p.y}`).join(' ');

  // 网格环
  const rings = [0.25, 0.5, 0.75, 1];

  return (
    <svg
      width={size}
      height={size}
      viewBox="0 0 200 200"
      className="mx-auto overflow-visible"
    >
      {/* 网格环 */}
      {rings.map((scale) => (
        <polygon
          key={scale}
          points={DIM_ORDER.map((_, i) => {
            const v = vertexFor(angleFor(i), R * scale);
            return `${v.x},${v.y}`;
          }).join(' ')}
          fill="none"
          stroke="rgba(255,255,255,0.07)"
          strokeWidth={0.6}
        />
      ))}

      {/* 轴线 */}
      {DIM_ORDER.map((_, i) => {
        const outer = vertexFor(angleFor(i), R);
        return (
          <line
            key={i}
            x1={CX}
            y1={CY}
            x2={outer.x}
            y2={outer.y}
            stroke="rgba(255,255,255,0.05)"
            strokeWidth={0.5}
          />
        );
      })}

      {/* 标签 */}
      {DIM_ORDER.map((dim, i) => {
        const angle = angleFor(i);
        const lx = CX + (R + 16) * Math.cos(angle);
        const ly = CY + (R + 16) * Math.sin(angle);
        return (
          <text
            key={dim}
            x={lx}
            y={ly}
            textAnchor="middle"
            dominantBaseline="middle"
            className="fill-white/30"
            style={{ fontSize: '8px', fontFamily: 'system-ui, sans-serif', fontWeight: 600 }}
          >
            {DIM_LABELS[dim] || dim}
          </text>
        );
      })}

      {/* 数据多边形（入场动画） */}
      <motion.g
        initial={{ scale: 0, opacity: 0 }}
        animate={{ scale: 1, opacity: 1 }}
        transition={{ duration: 0.9, delay: delay / 1000, ease: [0.22, 1, 0.36, 1] }}
        style={{ transformOrigin: `${CX}px ${CY}px` }}
      >
        <polygon
          points={dataPath}
          fill="rgba(255,204,0,0.18)"
          stroke="rgba(255,204,0,0.7)"
          strokeWidth={1.2}
          strokeLinejoin="round"
        />
        {/* 顶点 */}
        {dataPoints.map((p, i) => (
          <circle
            key={i}
            cx={p.x}
            cy={p.y}
            r={2.8}
            fill="#ffcc00"
            style={{ filter: 'drop-shadow(0 0 3px rgba(255,204,0,0.6))' }}
          />
        ))}
      </motion.g>

      {/* 中心点 */}
      <circle cx={CX} cy={CY} r={1.5} fill="rgba(255,255,255,0.3)" />
    </svg>
  );
}

/* ── 从真实记忆元数据计算 8 维证据分布 ───────────────────
   这里不再把“记忆类型数量”伪装成人格、温暖度或创造力；每个维度都能由
   列表中的类型、来源、时间、重要性与访问次数直接验证。
────────────────────────────────────────────────────── */
export interface MemoryLike {
  memory_type: string;
  content: string;
  importance: number;
  source?: string;
  created_at?: string;
  access_count?: number;
}

export function computeRadarValues(memories: MemoryLike[]): RadarDimensions {
  const total = Math.max(1, memories.length);
  const count = (pred: (m: MemoryLike) => boolean) =>
    memories.filter(pred).length / total;

  const raw: RadarDimensions = {
    preferences: count((m) => m.memory_type === 'user_preference'),
    habits: count((m) => m.memory_type === 'app_usage'),
    facts: count((m) => m.memory_type === 'knowledge'),
    episodes: count((m) => ['task_history', 'conversation_summary'].includes(m.memory_type)),
    explicit: count((m) => m.source === 'manual'),
    important: count((m) => m.importance >= 60),
    recent: count((m) => {
      const timestamp = m.created_at ? Date.parse(m.created_at.replace(' ', 'T') + (m.created_at.includes('T') ? '' : 'Z')) : NaN;
      return Number.isFinite(timestamp) && Date.now() - timestamp <= 30 * 86400000;
    }),
    used: count((m) => (m.access_count ?? 0) > 0),
  };

  // 归一化：找到最大值，缩放到 0.95 上限，最低 0.15
  const max = Math.max(...Object.values(raw), 0.01);
  const result: RadarDimensions = {};
  for (const [k, v] of Object.entries(raw)) {
    result[k] = Math.max(0.15, Math.min(0.95, (v / max) * 0.95));
  }
  return result;
}
