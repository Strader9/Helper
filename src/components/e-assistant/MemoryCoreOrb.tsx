/**
 * MemoryCoreOrb —— 记忆核心动态可视化
 *
 * 基于 EAssistantCoreOrb 的 3D 粒子球架构，适配 E-Assisant 记忆页：
 *  - 呼吸光晕（breathing glow）
 *  - 自动旋转 + 鼠标拖拽
 *  - 记忆选中时脉冲反应（pulse ring）
 *  - 颜色随当前选中记忆类型切换
 *  - 环境漂浮粒子
 *
 * 纯 Canvas 2D，无外部依赖。
 */
import { useEffect, useRef, useState } from 'react';
import { AnimatePresence, motion } from 'motion/react';

/* ── 类型配色（与 EAssistantMemory TYPE_META 对应）────────── */
const TYPE_COLORS: Record<string, { core: string; glow: string; accent: string }> = {
  knowledge: { core: '#22d3ee', glow: 'rgba(34,211,238,', accent: '#67e8f9' },
  user_preference: { core: '#a78bfa', glow: 'rgba(167,139,250,', accent: '#c4b5fd' },
  task_history: { core: '#fbbf24', glow: 'rgba(251,191,36,', accent: '#fcd34d' },
  conversation_summary: { core: '#34d399', glow: 'rgba(52,211,153,', accent: '#6ee7b7' },
  app_usage: { core: '#38bdf8', glow: 'rgba(56,189,248,', accent: '#7dd3fc' },
};
const DEFAULT_COLOR = { core: '#ffcc00', glow: 'rgba(255,204,0,', accent: '#ffe066' };

function colorFor(type: string | null) {
  return (type && TYPE_COLORS[type]) || DEFAULT_COLOR;
}

/* ── 粒子系统 ────────────────────────────────────────── */
interface OrbParticle {
  x: number; y: number; z: number;
  baseX: number; baseY: number; baseZ: number;
  size: number;
  kind: 'core' | 'signal' | 'void';
}

const SPHERE_RADIUS = 160;

function createParticle(): OrbParticle {
  const theta = 2 * Math.PI * Math.random();
  const phi = Math.acos(2 * Math.random() - 1);
  const radius = Math.cbrt(Math.random()) * SPHERE_RADIUS;
  const baseX = radius * Math.sin(phi) * Math.cos(theta);
  const baseY = radius * Math.sin(phi) * Math.sin(theta);
  const baseZ = radius * Math.cos(phi);
  const roll = Math.random();
  return {
    x: baseX, y: baseY, z: baseZ,
    baseX, baseY, baseZ,
    size: Math.random() * 1.6 + 0.5,
    kind: roll < 0.45 ? 'core' : roll < 0.8 ? 'signal' : 'void',
  };
}

interface MemoryCoreOrbProps {
  /** 当前选中的记忆类型，驱动配色切换 */
  activeType?: string | null;
  /** 脉冲计数器：每次 +1 触发一圈脉冲 */
  pulseKey?: number;
  /** 粒子数量（性能档位） */
  particleCount?: number;
  className?: string;
}

export function MemoryCoreOrb({
  activeType = null,
  pulseKey = 0,
  particleCount = 420,
  className = '',
}: MemoryCoreOrbProps) {
  const canvasRef = useRef<HTMLCanvasElement>(null);
  const particlesRef = useRef<OrbParticle[]>([]);
  const rotationRef = useRef({ x: 0, y: 0 });
  const pointerRef = useRef({ x: 0, y: 0, dragging: false });
  const colorRef = useRef(colorFor(activeType));
  const pulseRef = useRef(0); // 0~1 脉冲强度衰减
  const [ringKey, setRingKey] = useState(0);

  // 同步配色
  useEffect(() => {
    colorRef.current = colorFor(activeType);
  }, [activeType]);

  // 脉冲触发
  useEffect(() => {
    if (pulseKey > 0) {
      pulseRef.current = 1;
      setRingKey((k) => k + 1);
    }
  }, [pulseKey]);

  // 初始化粒子
  useEffect(() => {
    particlesRef.current = Array.from({ length: particleCount }, createParticle);
  }, [particleCount]);

  // 主渲染循环
  useEffect(() => {
    const canvas = canvasRef.current;
    if (!canvas) return;
    const ctx = canvas.getContext('2d');
    if (!ctx) return;
    let raf = 0;

    const render = (time: number) => {
      const dpr = Math.min(window.devicePixelRatio || 1, 2);
      const w = canvas.clientWidth * dpr;
      const h = canvas.clientHeight * dpr;
      if (canvas.width !== w || canvas.height !== h) {
        canvas.width = w;
        canvas.height = h;
      }
      ctx.clearRect(0, 0, w, h);

      const cx = w / 2;
      const cy = h / 2;
      const col = colorRef.current;

      // 自动旋转
      if (!pointerRef.current.dragging) {
        rotationRef.current.y += 0.004;
        rotationRef.current.x += 0.0015;
      }

      // 呼吸因子
      const breath = 1 + Math.sin(time * 0.0018) * 0.04;
      // 脉冲衰减
      pulseRef.current *= 0.96;
      const pulse = pulseRef.current;

      // 外层呼吸光晕
      const glowR = (SPHERE_RADIUS * 1.6) * dpr * (1 + pulse * 0.3);
      const glow = ctx.createRadialGradient(cx, cy, 0, cx, cy, glowR);
      glow.addColorStop(0, `${col.glow}${0.12 + pulse * 0.15})`);
      glow.addColorStop(0.4, `${col.glow}${0.05 + pulse * 0.06})`);
      glow.addColorStop(1, `${col.glow}0)`);
      ctx.fillStyle = glow;
      ctx.fillRect(cx - glowR, cy - glowR, glowR * 2, glowR * 2);

      // 内核柔光
      const coreR = SPHERE_RADIUS * 0.5 * dpr * breath;
      const coreGrad = ctx.createRadialGradient(cx, cy, 0, cx, cy, coreR);
      coreGrad.addColorStop(0, `${col.glow}${0.25 + pulse * 0.2})`);
      coreGrad.addColorStop(0.6, `${col.glow}0.08)`);
      coreGrad.addColorStop(1, `${col.glow}0)`);
      ctx.fillStyle = coreGrad;
      ctx.fillRect(cx - coreR, cy - coreR, coreR * 2, coreR * 2);

      // 更新 & 绘制粒子
      const rotX = rotationRef.current.x;
      const rotY = rotationRef.current.y;
      const cosX = Math.cos(rotX), sinX = Math.sin(rotX);
      const cosY = Math.cos(rotY), sinY = Math.sin(rotY);

      const particles = particlesRef.current;
      for (let i = 0; i < particles.length; i++) {
        const p = particles[i];
        // 波动（呼吸 + 脉冲）
        const wave = Math.sin(time * 0.002 + (p.baseX + p.baseY + p.baseZ) * 0.012) * (8 + pulse * 40);
        const radialUnit = Math.sqrt(p.baseX ** 2 + p.baseY ** 2 + p.baseZ ** 2) + 0.001;
        const rf = (radialUnit * breath + wave) / radialUnit;
        let x = p.baseX * rf;
        let y = p.baseY * rf;
        let z = p.baseZ * rf;
        // 绕 X 旋转
        const ry = y * cosX - z * sinX;
        const rz = y * sinX + z * cosX;
        y = ry; z = rz;
        // 绕 Y 旋转
        p.x = x * cosY + z * sinY;
        p.z = -x * sinY + z * cosY;
        p.y = y;
      }
      // 按 z 排序（远→近）
      particles.sort((a, b) => a.z - b.z);

      for (let i = 0; i < particles.length; i++) {
        const p = particles[i];
        const boundedZ = Math.max(-280, Math.min(p.z, 500));
        const perspective = 520 / (520 - boundedZ);
        const px = p.x * perspective * dpr + cx;
        const py = p.y * perspective * dpr + cy;
        const sz = p.size * perspective * dpr;

        if (p.kind === 'void') {
          ctx.globalAlpha = Math.max(0.15, perspective - 0.35);
          ctx.strokeStyle = `${col.glow}0.35)`;
          ctx.lineWidth = 0.5 * dpr;
          ctx.beginPath();
          ctx.arc(px, py, sz, 0, Math.PI * 2);
          ctx.stroke();
        } else {
          const isSignal = p.kind === 'signal';
          ctx.globalAlpha = Math.max(0.12, perspective - 0.4);
          ctx.fillStyle = isSignal ? col.accent : col.core;
          if (isSignal) {
            ctx.shadowColor = col.core;
            ctx.shadowBlur = 4 * dpr;
          }
          ctx.beginPath();
          ctx.arc(px, py, sz, 0, Math.PI * 2);
          ctx.fill();
          ctx.shadowBlur = 0;
        }
      }
      ctx.globalAlpha = 1;

      raf = requestAnimationFrame(render);
    };

    raf = requestAnimationFrame(render);
    return () => cancelAnimationFrame(raf);
  }, []);

  /* ── 鼠标拖拽 ─────────────────────────────────────── */
  const onDown = (e: React.MouseEvent | React.TouchEvent) => {
    const pt = 'touches' in e ? e.touches[0] : e;
    pointerRef.current = { x: pt.clientX, y: pt.clientY, dragging: true };
  };
  const onMove = (e: React.MouseEvent | React.TouchEvent) => {
    if (!pointerRef.current.dragging) return;
    const pt = 'touches' in e ? e.touches[0] : e;
    rotationRef.current.y += (pt.clientX - pointerRef.current.x) * 0.01;
    rotationRef.current.x -= (pt.clientY - pointerRef.current.y) * 0.01;
    pointerRef.current.x = pt.clientX;
    pointerRef.current.y = pt.clientY;
  };
  const onUp = () => { pointerRef.current.dragging = false; };

  return (
    <div
      className={`relative flex aspect-square items-center justify-center cursor-grab active:cursor-grabbing select-none ${className}`}
      onMouseDown={onDown}
      onMouseMove={onMove}
      onMouseUp={onUp}
      onMouseLeave={onUp}
      onTouchStart={onDown}
      onTouchMove={onMove}
      onTouchEnd={onUp}
    >
      <canvas
        ref={canvasRef}
        className="pointer-events-none relative z-10 h-full w-full"
      />
      {/* 脉冲环 */}
      <AnimatePresence>
        {[0, 1].map((i) => (
          <motion.div
            key={`${ringKey}-${i}`}
            className="pointer-events-none absolute inset-0 rounded-full border"
            style={{ borderColor: colorFor(activeType).core }}
            initial={{ scale: 0.4, opacity: 0.7 }}
            animate={{ scale: 1.6, opacity: 0 }}
            transition={{ duration: 1.4, delay: i * 0.25, ease: 'easeOut' }}
          />
        ))}
      </AnimatePresence>
    </div>
  );
}
