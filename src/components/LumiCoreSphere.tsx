import type { CSSProperties } from 'react';
import { motion, useReducedMotion } from 'motion/react';
import { CheckCircle2, CircleAlert, Loader2, Pause } from 'lucide-react';
import { LumiCoreOrb } from './LumiCoreOrb';
import { MissionCoreMaterial, type MissionTheme } from './MissionCoreMaterial';

export type LumiCoreTaskState = 'queued' | 'working' | 'paused' | 'attention' | 'completed';

export interface LumiCoreOrbitTask {
  id: string;
  title: string;
  state: LumiCoreTaskState;
  active: boolean;
}

const TASK_COLORS: Record<LumiCoreTaskState, string> = {
  queued: '#94a3b8',
  working: '#67e8f9',
  paused: '#fbbf24',
  attention: '#fb7185',
  completed: '#6ee7b7',
};

function TaskIcon({ state }: { state: LumiCoreTaskState }) {
  if (state === 'working') return <Loader2 size={11} className="animate-spin" />;
  if (state === 'paused') return <Pause size={11} />;
  if (state === 'attention') return <CircleAlert size={11} />;
  if (state === 'completed') return <CheckCircle2 size={11} />;
  return <span className="h-1.5 w-1.5 rounded-full bg-current" />;
}

export function LumiCoreSphere({
  tasks,
  state,
  loading,
  showTaskStatus = true,
  labels,
  theme = 'nebula',
}: {
  tasks: LumiCoreOrbitTask[];
  state: 'ready' | 'working' | 'attention';
  loading?: boolean;
  /** 首页的 Jarvis 视图不展示围绕核心的后台任务浮签。 */
  showTaskStatus?: boolean;
  theme?: MissionTheme;
  labels: {
    aria: string;
    lumiCore: string;
    ready: string;
    working: string;
    attention: string;
    noTasks: string;
  };
}) {
  const reducedMotion = useReducedMotion();
  const activeTasks = tasks.filter(task => task.active).slice(0, 8);
  const stateLabel = state === 'working' ? labels.working : state === 'attention' ? labels.attention : labels.ready;
  const coreColor = state === 'attention' ? '#fb7185' : state === 'working' ? '#67e8f9' : '#a78bfa';
  const motionEnabled = reducedMotion !== true;

  return (
    <section
      aria-label={labels.aria}
      aria-busy={loading}
      data-lumi-core-command-field
      data-lumi-core-field-state={state}
      data-lumi-core-loading={loading ? 'true' : 'false'}
      data-lumi-core-active-tasks={activeTasks.length}
      className="lumi-core-command-field relative h-full min-h-[420px]"
    >
      <div className="absolute left-1/2 top-1/2 h-[min(68vw,560px)] w-[min(68vw,560px)] -translate-x-1/2 -translate-y-1/2">
        <motion.div
          className={`lumi-core-command-field__core absolute left-1/2 top-1/2 z-10 -translate-x-1/2 -translate-y-1/2 ${theme === 'nebula' ? 'h-[66%] w-[66%]' : 'h-[48%] w-[48%]'}`}
          animate={motionEnabled ? { x: [-8, 8, -8], y: [5, -7, 5] } : { x: 0, y: 0 }}
          transition={motionEnabled ? { duration: 7.5, repeat: Infinity, ease: 'easeInOut' } : { duration: 0 }}
        >
          {theme === 'nebula' ? (
            <LumiCoreOrb
              sentiment={state === 'attention' ? 'excited' : state === 'working' ? 'focused' : 'default'}
              callState={state === 'working' ? 'thinking' : 'idle'}
              className="h-full w-full"
            />
          ) : (
            <MissionCoreMaterial theme={theme} />
          )}
        </motion.div>
        <div className="lumi-core-command-field__core-label absolute left-1/2 top-[72%] z-20 -translate-x-1/2 rounded-2xl border px-4 py-2 text-center shadow-xl">
          <div className="text-[9px] font-black uppercase tracking-[0.28em] text-white/48">{labels.lumiCore}</div>
          <div className="mt-1 flex items-center justify-center gap-1.5 text-[10px] font-bold text-white/72">
            <span className="h-1.5 w-1.5 rounded-full" style={{ backgroundColor: coreColor, boxShadow: `0 0 10px ${coreColor}` }} />
            {stateLabel}
          </div>
          <div className="mt-1 font-mono text-[8px] tracking-[0.18em] text-white/30">{String(activeTasks.length).padStart(2, '0')}</div>
        </div>

        {showTaskStatus && activeTasks.map((task, index) => {
          const angle = (Math.PI * 2 * index) / Math.max(activeTasks.length, 1) - Math.PI / 2;
          const radius = 42 + (index % 2) * 6;
          const x = 50 + Math.cos(angle) * radius;
          const y = 50 + Math.sin(angle) * radius;
          const color = TASK_COLORS[task.state];
          return (
            <motion.div
              key={task.id}
              className="lumi-core-command-field__beacon absolute z-20 flex max-w-[150px] items-center gap-1.5 rounded-xl border px-2.5 py-2 text-[9px] font-bold text-white/72 shadow-xl"
              data-task-state={task.state}
              style={{
                left: `${x}%`,
                top: `${y}%`,
                translateX: '-50%',
                translateY: '-50%',
                borderColor: `${color}55`,
                '--beacon-color': color,
              } as CSSProperties}
              initial={reducedMotion ? false : { opacity: 0, scale: 0.8 }}
              animate={motionEnabled ? { opacity: 1, scale: 1, y: [0, index % 2 ? 5 : -5, 0] } : { opacity: 1, scale: 1, y: 0 }}
              transition={motionEnabled
                ? { opacity: { duration: 0.25 }, scale: { duration: 0.25 }, y: { duration: 4 + index * 0.25, repeat: Infinity, ease: 'easeInOut' } }
                : { duration: 0 }}
              title={task.title}
            >
              <span className="lumi-core-command-field__beacon-dot shrink-0" style={{ color }}><TaskIcon state={task.state} /></span>
              <span className="truncate">{task.title}</span>
            </motion.div>
          );
        })}
      </div>

      {showTaskStatus && !loading && activeTasks.length === 0 && (
        <div className="lumi-core-command-field__empty absolute bottom-20 left-1/2 -translate-x-1/2 rounded-full border px-3 py-1.5 text-[9px]">
          {labels.noTasks}
        </div>
      )}
    </section>
  );
}
