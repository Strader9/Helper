import { Blocks, Puzzle, Wrench } from 'lucide-react';
import { useSearchParams } from 'react-router-dom';
import { LumiPageShell } from '../../components/lumi/LumiPageShell';
import { cn } from '../../lib/cn';
import LumiSkills from './LumiSkills';
import LumiTools from './LumiTools';

type CapabilityTab = 'tools' | 'skills';

const TABS: Array<{
  id: CapabilityTab;
  label: string;
  description: string;
  icon: typeof Wrench;
}> = [
  {
    id: 'tools',
    label: '工具',
    description: '查看工具契约、风险等级和授权状态',
    icon: Wrench,
  },
  {
    id: 'skills',
    label: 'Skill',
    description: '安装、启用和管理可复用技能',
    icon: Puzzle,
  },
];

function normalizeTab(value: string | null): CapabilityTab {
  return value === 'skills' ? 'skills' : 'tools';
}

export default function LumiCapabilities() {
  const [params, setParams] = useSearchParams();
  const activeTab = normalizeTab(params.get('tab'));

  const switchTab = (tab: CapabilityTab) => {
    const next = new URLSearchParams(params);
    next.set('tab', tab);
    setParams(next, { replace: true });
  };

  return (
    <LumiPageShell
      icon={<Blocks size={20} />}
      iconBg="bg-celestial-saturn/15 border-celestial-saturn/25 text-celestial-saturn"
      title="能力中心"
      subtitle="统一管理 AI 可以调用的工具与 Skill"
    >
      <div className="mb-6 grid gap-3 sm:grid-cols-2" role="tablist" aria-label="能力类型">
        {TABS.map((tab) => {
          const Icon = tab.icon;
          const active = tab.id === activeTab;
          return (
            <button
              key={tab.id}
              type="button"
              role="tab"
              aria-selected={active}
              onClick={() => switchTab(tab.id)}
              className={cn(
                'flex items-center gap-3 rounded-2xl border p-4 text-left transition-all',
                active
                  ? 'border-celestial-saturn/40 bg-celestial-saturn/10 shadow-[0_0_24px_rgba(255,204,0,0.08)]'
                  : 'border-white/[0.08] bg-white/[0.025] hover:border-white/15 hover:bg-white/[0.05]',
              )}
            >
              <span
                className={cn(
                  'flex h-10 w-10 shrink-0 items-center justify-center rounded-xl border',
                  active
                    ? 'border-celestial-saturn/25 bg-celestial-saturn/15 text-celestial-saturn'
                    : 'border-white/10 bg-white/5 text-white/45',
                )}
              >
                <Icon size={18} />
              </span>
              <span className="min-w-0">
                <span className={cn('block text-sm font-black', active ? 'text-white' : 'text-white/65')}>
                  {tab.label}
                </span>
                <span className="mt-0.5 block text-[11px] leading-relaxed text-white/35">
                  {tab.description}
                </span>
              </span>
            </button>
          );
        })}
      </div>

      <div role="tabpanel" aria-label={activeTab === 'tools' ? '工具' : 'Skill'}>
        {activeTab === 'tools' ? <LumiTools embedded /> : <LumiSkills embedded />}
      </div>
    </LumiPageShell>
  );
}
