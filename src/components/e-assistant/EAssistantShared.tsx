// EAssistantCore 风格共享组件 —— 提取自 EAssistantCore SharedUI.tsx，适配 E-Assisant (Tauri)
import React from 'react';
import { motion } from 'motion/react';
import { Loader2 } from 'lucide-react';
import { cn } from '../../lib/cn';

/* ──────────────────────────────────────────────
   GlassCard —— 毛玻璃卡片（EAssistantCore 核心视觉元素）
   ────────────────────────────────────────────── */
interface GlassCardProps extends React.HTMLAttributes<HTMLDivElement> {
  children: React.ReactNode;
  hoverEffect?: boolean;
  glowColor?: string; // e.g. 'rgba(255,204,0,0.05)'
}

export function GlassCard({ children, className, hoverEffect = true, glowColor, ...props }: GlassCardProps) {
  return (
    <div
      className={cn(
        'glass-panel relative overflow-hidden rounded-2xl p-6 transition-all duration-500',
        hoverEffect && 'hover:border-celestial-saturn/30',
        className
      )}
      style={glowColor ? { boxShadow: `0 0 40px ${glowColor}` } : undefined}
      {...props}
    >
      {hoverEffect && (
        <div className="pointer-events-none absolute inset-0 bg-gradient-to-br from-celestial-saturn/5 via-transparent to-transparent opacity-0 transition-opacity duration-500 group-hover:opacity-100" />
      )}
      <div className="relative z-10">{children}</div>
    </div>
  );
}

/* ──────────────────────────────────────────────
   IconBox —— 图标容器
   ────────────────────────────────────────────── */
interface IconBoxProps {
  icon: React.ReactNode;
  className?: string;
  size?: 'sm' | 'md' | 'lg';
  color?: string; // background color
}

export function IconBox({ icon, className, size = 'md', color }: IconBoxProps) {
  const sizes = {
    sm: 'h-10 w-10 rounded-xl',
    md: 'h-14 w-14 rounded-2xl',
    lg: 'h-20 w-20 rounded-3xl',
  };
  return (
    <div
      className={cn('flex items-center justify-center bg-white/5 transition-transform group-hover:scale-110', sizes[size], className)}
      style={color ? { background: color } : undefined}
    >
      {icon}
    </div>
  );
}

/* ──────────────────────────────────────────────
   PulseCounter —— 脉冲数字统计
   ────────────────────────────────────────────── */
export function PulseCounter({ label, value, colorClass = 'text-celestial-saturn' }: { label: string; value: string; colorClass?: string }) {
  return (
    <div className="flex flex-col items-center gap-1">
      <div className={cn('text-2xl font-black tracking-tighter tabular-nums md:text-3xl', colorClass)}>{value}</div>
      <div className="text-center text-[10px] font-bold uppercase tracking-[0.2em] text-white/45 md:text-[12px] md:tracking-[0.3em]">{label}</div>
      <motion.div
        animate={{ scaleX: [0, 1, 0], opacity: [0, 1, 0] }}
        transition={{ duration: 2, repeat: Infinity, ease: 'easeInOut' }}
        className={cn('h-px w-full', colorClass.replace('text-', 'bg-'))}
      />
    </div>
  );
}

/* ──────────────────────────────────────────────
   LoadingSpinner
   ────────────────────────────────────────────── */
export function LoadingSpinner({ className, label }: { className?: string; label?: string }) {
  return (
    <div className={cn('flex min-h-[300px] flex-col items-center justify-center gap-3', className)}>
      <Loader2 className="animate-spin text-celestial-saturn" size={40} />
      {label && <span className="text-xs text-white/50">{label}</span>}
    </div>
  );
}

/* ──────────────────────────────────────────────
   PageHeader —— 页面顶部标题栏（EAssistantCore 风格）
   ────────────────────────────────────────────── */
interface PageHeaderProps {
  icon: React.ReactNode;
  iconBg?: string;
  title: string;
  subtitle?: string;
  actions?: React.ReactNode;
  onBack?: () => void;
}

export function PageHeader({ icon, iconBg = 'bg-celestial-saturn/15 border-celestial-saturn/20 text-celestial-saturn', title, subtitle, actions, onBack }: PageHeaderProps) {
  return (
    <div className="flex items-center gap-3 border-b border-white/[0.08] px-5 py-4">
      {onBack && (
        <button
          onClick={onBack}
          className="flex h-9 w-9 items-center justify-center rounded-xl border border-white/10 bg-white/5 text-white/60 transition-colors hover:bg-white/10 hover:text-white"
        >
          ←
        </button>
      )}
      <div className={cn('flex h-10 w-10 items-center justify-center rounded-xl border', iconBg)}>{icon}</div>
      <div className="flex-1 min-w-0">
        <h2 className="text-sm font-bold text-white/90">{title}</h2>
        {subtitle && <p className="text-xs text-white/55">{subtitle}</p>}
      </div>
      {actions && <div className="flex items-center gap-2">{actions}</div>}
    </div>
  );
}

/* ──────────────────────────────────────────────
   SectionTitle —— 页面内分组标题
   ────────────────────────────────────────────── */
export function SectionTitle({ label, count, color = 'bg-celestial-saturn' }: { label: string; count?: number; color?: string }) {
  return (
    <div className="mb-3 flex items-center gap-2">
      <div className={cn('h-2 w-2 rounded-full', color)} />
      <span className="text-xs font-black uppercase tracking-wider text-white/55">
        {label}
        {count !== undefined && ` (${count})`}
      </span>
    </div>
  );
}

/* ──────────────────────────────────────────────
   Badge —— 状态标签
   ────────────────────────────────────────────── */
const BADGE_VARIANTS = {
  safe: 'bg-emerald-500/10 border-emerald-500/30 text-emerald-400',
  warning: 'bg-amber-500/10 border-amber-500/30 text-amber-400',
  danger: 'bg-red-500/10 border-red-500/30 text-red-400',
  info: 'bg-cyan-500/10 border-cyan-500/30 text-cyan-400',
  neutral: 'bg-white/5 border-white/10 text-white/50',
  saturn: 'bg-celestial-saturn/10 border-celestial-saturn/30 text-celestial-saturn',
} as const;

export function Badge({ children, variant = 'neutral', className }: { children: React.ReactNode; variant?: keyof typeof BADGE_VARIANTS; className?: string }) {
  return (
    <span className={cn('inline-flex items-center rounded-md border px-1.5 py-0.5 text-[10px] font-black uppercase', BADGE_VARIANTS[variant], className)}>
      {children}
    </span>
  );
}

/* ──────────────────────────────────────────────
   ToggleSwitch —— 开关（EAssistantCore ToolPanel 风格）
   ────────────────────────────────────────────── */
export function ToggleSwitch({ enabled, onChange, disabled }: { enabled: boolean; onChange: () => void; disabled?: boolean }) {
  return (
    <button
      onClick={onChange}
      disabled={disabled}
      className={cn('flex h-5 w-9 shrink-0 items-center rounded-full p-0.5 transition-colors', enabled ? 'bg-emerald-500/60' : 'bg-white/10', disabled && 'opacity-40 cursor-not-allowed')}
    >
      <motion.div
        animate={{ x: enabled ? 16 : 0 }}
        className={cn('h-4 w-4 rounded-full', enabled ? 'bg-white' : 'bg-white/30')}
      />
    </button>
  );
}

/* ──────────────────────────────────────────────
   EmptyState —— 空状态
   ────────────────────────────────────────────── */
export function EmptyState({ icon, title, description }: { icon: React.ReactNode; title: string; description?: string }) {
  return (
    <div className="flex flex-col items-center justify-center gap-3 py-16 text-center">
      <div className="flex h-16 w-16 items-center justify-center rounded-2xl bg-white/5 text-white/30">{icon}</div>
      <h3 className="text-sm font-bold text-white/60">{title}</h3>
      {description && <p className="max-w-xs text-xs text-white/35">{description}</p>}
    </div>
  );
}

/* ──────────────────────────────────────────────
   StatCard —— 统计卡片
   ────────────────────────────────────────────── */
interface StatCardProps {
  label: string;
  value: string | number;
  icon: React.ReactNode;
  trend?: string;
  color?: string;
}

export function StatCard({ label, value, icon, trend, color = 'text-celestial-saturn' }: StatCardProps) {
  return (
    <div className="glass-panel rounded-2xl p-4">
      <div className="flex items-start justify-between">
        <div>
          <p className="text-[10px] font-bold uppercase tracking-wider text-white/40">{label}</p>
          <p className={cn('mt-1 text-2xl font-black tabular-nums', color)}>{value}</p>
          {trend && <p className="mt-0.5 text-[10px] text-emerald-400">{trend}</p>}
        </div>
        <div className="flex h-9 w-9 items-center justify-center rounded-xl bg-white/5 text-white/40">{icon}</div>
      </div>
    </div>
  );
}
