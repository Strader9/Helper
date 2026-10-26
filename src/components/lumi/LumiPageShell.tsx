// LumiCore 风格页面外壳 —— 所有迁移页面的统一布局
import React from 'react';
import { useNavigate } from 'react-router-dom';
import { Home } from 'lucide-react';
import { PageHeader } from './LumiShared';
import { cn } from '../../lib/cn';

interface LumiPageShellProps {
  children: React.ReactNode;
  icon: React.ReactNode;
  iconBg?: string;
  title: string;
  subtitle?: string;
  actions?: React.ReactNode;
  className?: string;
  contentClassName?: string;
}

export function LumiPageShell({ children, icon, iconBg, title, subtitle, actions, className, contentClassName }: LumiPageShellProps) {
  const navigate = useNavigate();

  return (
    <div className={cn('lumi-surface flex h-screen w-full flex-col overflow-hidden text-white', className)}>
      <PageHeader
        icon={icon}
        iconBg={iconBg}
        title={title}
        subtitle={subtitle}
        actions={
          <>
            {actions}
            <button
              onClick={() => navigate('/jarvis')}
              className="flex h-9 w-9 items-center justify-center rounded-xl border border-white/10 bg-white/5 text-white/60 transition-colors hover:bg-white/10 hover:text-white"
              title="返回 Jarvis 工作台"
            >
              <Home size={16} />
            </button>
          </>
        }
        onBack={() => navigate('/jarvis')}
      />
      <div className={cn('flex-1 overflow-y-auto custom-scrollbar p-5 md:p-6', contentClassName)}>
        {children}
      </div>
    </div>
  );
}
