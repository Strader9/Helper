import { useState, useEffect, useCallback } from 'react';
import { HashRouter, Routes, Route, useNavigate, useLocation } from 'react-router-dom';
import { invoke } from '@tauri-apps/api/core';
import {
  Brain,
  Ear,
  MessageSquare,
  Mic,
  MicOff,
  Monitor,
  Music,
  Orbit,
  Palette,
  Puzzle,
  ScrollText,
  Settings as SettingsIcon,
  UserRound,
  Users,
  Wrench,
} from 'lucide-react';
import { AppProvider } from './contexts/AppContext';
import { LumiCoreSphere } from './components/LumiCoreSphere';
import { LumiPermissionGate } from './components/lumi/LumiPermissionGate';
import { useVoice } from './hooks/useVoice';
import './index.css';

// 左侧功能入口（对齐 LumiCore 截图，点击跳转对应页面）
//
// 原先用 emoji + 纯色圆角块，在深色界面上会显得像儿童积木——emoji 本身
// 自带彩色描边和固定字重，无法与周围灰度层级协调。改成单色线性图标 +
// 玻璃底板后，颜色只作为"身份标识"出现在图标上，不再整块铺开。
const APP_ENTRIES = [
  { icon: ScrollText, label: '运行日志', color: '#7dd3fc', path: '/logs' },
  { icon: Wrench, label: '工具', color: '#fcd34d', path: '/tools' },
  { icon: Puzzle, label: '技能大厅', color: '#6ee7b7', path: '/skills' },
  { icon: Users, label: 'Team', color: '#93c5fd', path: '/devices' },
  { icon: Brain, label: '记忆化身', color: '#d8b4fe', path: '/memory' },
  { icon: Palette, label: '形象设计室', color: '#67e8f9', path: '/media' },
  // 「声音」原本和底栏的「设置」指向同一页，重复且没内容；
  // 换成贾维斯——语音输入与播报都在那一页，入口才有落点。
  { icon: Ear, label: 'Jarvis 设置', color: '#a5b4fc', path: '/jarvis' },
  { icon: Music, label: 'Music', color: '#fda4af', path: '/automation' },
];

// 底部导航
const BOTTOM_NAV = [
  { icon: Orbit, label: '指挥中心', path: '/' },
  { icon: Monitor, label: '系统', path: '/settings' },
  { icon: MessageSquare, label: '聊天', path: '/chat' },
  { icon: UserRound, label: '人格', path: '/memory' },
  { icon: ScrollText, label: '日志', path: '/logs' },
  { icon: SettingsIcon, label: '设置', path: '/settings' },
];

// 人格卡片数据类型
interface Persona {
  name: string;
  role: string;
  permission: string;
  color: string;
  level: number;
}

// 默认人格（后端无数据时使用）
const DEFAULT_PERSONAS: Persona[] = [
  { name: '苍穹', role: '聊天', permission: '请求批准', color: 'linear-gradient(135deg, #f59e0b, #d97706)', level: 30 },
  { name: '星云', role: '助手', permission: '替我审批', color: 'linear-gradient(135deg, #a855f7, #7c3aed)', level: 60 },
  { name: '赛博', role: '自主', permission: '完全访问', color: 'linear-gradient(135deg, #22c55e, #16a34a)', level: 100 },
];

function CommandCenter() {
  const navigate = useNavigate();
  const location = useLocation();
  const [activePersona, setActivePersona] = useState(1);
  const [personas, setPersonas] = useState<Persona[]>(DEFAULT_PERSONAS);
  const [learningStats, setLearningStats] = useState({ interactions: 128, optimizations: 3, status: '正在学习你的使用习惯...' });
  const { isListening, transcript, startListening, stopListening, supported } = useVoice();

  // 接入真实数据：人格配置
  useEffect(() => {
    invoke<{ key: string; value: string }[]>('get_settings')
      .then((items) => {
        const personaSetting = items.find((s) => s.key === 'guardian.personas');
        if (personaSetting) {
          try {
            const parsed = JSON.parse(personaSetting.value);
            if (Array.isArray(parsed) && parsed.length > 0) setPersonas(parsed);
          } catch { /* 保持默认 */ }
        }
      })
      .catch(() => { /* 保持默认 */ });
  }, []);

  // 接入真实数据：学习流统计
  useEffect(() => {
    invoke<any>('get_system_status')
      .then((status) => {
        if (status) {
          setLearningStats({
            interactions: status.total_interactions || 128,
            optimizations: status.optimizations_today || 3,
            status: status.learning_status || '正在学习你的使用习惯...',
          });
        }
      })
      .catch(() => { /* 保持默认 */ });
  }, []);

  const handleVoiceToggle = useCallback(() => {
    if (isListening) {
      stopListening();
    } else {
      startListening();
    }
  }, [isListening, startListening, stopListening]);

  // 首页话筒是 Jarvis 的语音入口：识别完成后转入 Jarvis 对话，
  // 不再绕到旧的通用聊天页。
  useEffect(() => {
    if (transcript && transcript.trim()) {
      navigate('/jarvis', { state: { voiceText: transcript } });
    }
  }, [transcript, navigate]);

  return (
    // 背景交给 body::before 的星云层，这里必须保持透明，否则会把景深整个盖掉
    <div className="relative h-screen w-full overflow-hidden bg-transparent">
      {/* 中央粒子球指挥场：监听时球体变红（attention），否则保持待命紫 */}
      <LumiCoreSphere
        tasks={[]}
        showTaskStatus={false}
        state={isListening ? 'attention' : 'ready'}
        labels={{
          aria: 'PC Guardian 指挥中心',
          lumiCore: 'PC GUARDIAN',
          ready: '待命',
          working: '执行中',
          attention: '需关注',
          noTasks: '暂无活跃任务',
        }}
      />

      {/* 左侧功能入口 */}
      <div className="absolute left-5 top-20 z-30 flex flex-col gap-3">
        {APP_ENTRIES.map((entry, i) => {
          const Icon = entry.icon;
          return (
            <div
              key={i}
              className="group flex cursor-pointer flex-col items-center gap-1.5"
              onClick={() => navigate(entry.path)}
            >
              <div
                className="relative flex h-11 w-11 items-center justify-center rounded-2xl border border-white/[0.08] bg-white/[0.03] backdrop-blur-xl transition-all duration-300 group-hover:-translate-y-0.5 group-hover:border-white/20 group-hover:bg-white/[0.07]"
                style={{ boxShadow: 'inset 0 1px 0 rgba(255,255,255,0.07), 0 4px 16px -8px rgba(0,0,0,0.8)' }}
              >
                <Icon size={18} strokeWidth={1.75} style={{ color: entry.color }} />
                {/* hover 时的同色微光，比整块色底克制得多 */}
                <span
                  className="pointer-events-none absolute inset-0 rounded-2xl opacity-0 transition-opacity duration-300 group-hover:opacity-100"
                  style={{ boxShadow: `0 0 18px -4px ${entry.color}55` }}
                />
              </div>
              <span className="text-[11px] font-medium text-white/40 transition-colors group-hover:text-white/75">
                {entry.label}
              </span>
            </div>
          );
        })}
      </div>

      {/* 右侧人格卡片 + 学习流 */}
      <div className="absolute right-5 top-20 z-50 flex w-64 flex-col gap-3">
        {personas.map((persona, i) => (
          <div
            key={i}
            className="cursor-pointer rounded-2xl p-3.5 text-white transition-all"
            style={{
              background: persona.color,
              opacity: activePersona === i ? 1 : 0.7,
              transform: activePersona === i ? 'scale(1.02)' : 'scale(1)',
              border: '1px solid rgba(255,255,255,0.1)',
            }}
            onClick={() => setActivePersona(i)}
          >
            <div className="mb-1.5 flex items-center gap-2">
              <span className="text-sm font-bold">{persona.name}</span>
              <span className="text-[10px] opacity-70">· {persona.role}</span>
            </div>
            <div className="text-[10px] opacity-60">{persona.permission}</div>
            <div className="mt-2 h-0.5 overflow-hidden rounded-full bg-white/20">
              <div className="h-full rounded-full bg-white/60" style={{ width: `${persona.level}%` }} />
            </div>
          </div>
        ))}

        {/* 学习流面板 */}
        <div className="rounded-2xl p-4 backdrop-blur-xl" style={{ background: 'rgba(10,14,30,0.8)', border: '1px solid rgba(255,255,255,0.08)' }}>
          <div className="mb-2.5 flex items-center justify-between">
            <span className="text-xs font-bold text-white/80">GUARDIAN 学习流</span>
            <span className="rounded-full bg-white/10 px-2 py-0.5 text-[10px] text-white/60">实时</span>
          </div>
          <div className="text-[11px] leading-relaxed text-white/50">
            {learningStats.status}
            <br />已记录 {learningStats.interactions} 条交互模式
            <br />今日优化 {learningStats.optimizations} 项工具调用
          </div>
        </div>
      </div>

      {/* 底部语音交互区 */}
      <div className="absolute bottom-24 left-1/2 z-25 flex -translate-x-1/2 flex-col items-center gap-3">
        <button
          className="flex h-14 w-14 items-center justify-center rounded-full backdrop-blur-md transition-all"
          style={{
            background: isListening ? 'rgba(239,68,68,0.2)' : 'rgba(255,255,255,0.08)',
            border: `1px solid ${isListening ? 'rgba(239,68,68,0.5)' : 'rgba(255,255,255,0.15)'}`,
            color: isListening ? '#ef4444' : 'rgba(255,255,255,0.7)',
          }}
          onClick={handleVoiceToggle}
          title={supported ? '点击开始语音会话' : '当前浏览器不支持语音识别'}
        >
          {isListening ? <MicOff size={22} strokeWidth={1.8} /> : <Mic size={22} strokeWidth={1.8} />}
        </button>
        <span className="text-[11px] text-white/40">
          {isListening ? `正在监听... ${transcript || ''}` : '语音交互 点击开始语音会话'}
        </span>
      </div>

      {/* 底部图标导航 */}
      <div className="absolute bottom-4 left-1/2 z-30 flex -translate-x-1/2 items-center gap-2 rounded-2xl p-2 backdrop-blur-xl" style={{ background: 'rgba(10,14,30,0.6)', border: '1px solid rgba(255,255,255,0.08)' }}>
        {BOTTOM_NAV.map((item, i) => {
          const Icon = item.icon;
          const isActive = location.pathname === item.path;
          return (
            <button
              key={i}
              className="relative flex h-10 w-10 items-center justify-center rounded-xl transition-all hover:bg-white/[0.07]"
              style={{ color: isActive ? '#a5f3fc' : 'rgba(255,255,255,0.4)' }}
              onClick={() => navigate(item.path)}
              title={item.label}
            >
              <Icon size={17} strokeWidth={1.75} />
              {isActive && (
                <span className="absolute inset-x-2.5 -bottom-0.5 h-px rounded-full bg-cyan-300/60" />
              )}
            </button>
          );
        })}
      </div>
    </div>
  );
}

// 懒加载页面组件（避免循环依赖，页面文件由子代理创建）
import { lazy, Suspense, Component, type ReactNode } from 'react';
import { LoadingSpinner } from './components/lumi/LumiShared';

// 错误边界：捕获页面渲染错误，避免空白页
class ErrorBoundary extends Component<{ children: ReactNode }, { error: string | null }> {
  state = { error: null };
  static getDerivedStateFromError(error: Error) {
    return { error: error.message || String(error) };
  }
  componentDidCatch(error: Error) {
    console.error('Page render error:', error);
  }
  render() {
    if (this.state.error) {
      return (
        <div className="lumi-surface flex h-screen w-full flex-col items-center justify-center gap-4 text-white">
          <div className="text-red-400 text-lg font-bold">页面渲染错误</div>
          <div className="text-white/60 text-sm max-w-md text-center font-mono">{this.state.error}</div>
          <button
            onClick={() => this.setState({ error: null })}
            className="mt-4 rounded-xl border border-celestial-saturn/30 bg-celestial-saturn/10 px-4 py-2 text-sm text-celestial-saturn hover:bg-celestial-saturn/20"
          >
            重试
          </button>
        </div>
      );
    }
    return this.props.children;
  }
}

const LumiChat = lazy(() => import('./pages/lumi/LumiChat'));
const LumiSettings = lazy(() => import('./pages/lumi/LumiSettings'));
const LumiLogs = lazy(() => import('./pages/lumi/LumiLogs'));
const LumiTools = lazy(() => import('./pages/lumi/LumiTools'));
const LumiSkills = lazy(() => import('./pages/lumi/LumiSkills'));
const LumiMemory = lazy(() => import('./pages/lumi/LumiMemory'));
const LumiTasks = lazy(() => import('./pages/lumi/LumiTasks'));
const LumiMedia = lazy(() => import('./pages/lumi/LumiMedia'));
const LumiMail = lazy(() => import('./pages/lumi/LumiMail'));
const LumiAutomation = lazy(() => import('./pages/lumi/LumiAutomation'));
const LumiDevices = lazy(() => import('./pages/lumi/LumiDevices'));
const LumiJarvis = lazy(() => import('./pages/lumi/LumiJarvis'));

function PageFallback() {
  return (
    <div className="lumi-surface flex h-screen w-full items-center justify-center">
      <LoadingSpinner label="加载中..." />
    </div>
  );
}

export default function LumiApp() {
  return (
    <AppProvider>
      <HashRouter>
        <Suspense fallback={<PageFallback />}>
          <ErrorBoundary>
            <Routes>
              <Route path="/" element={<CommandCenter />} />
              <Route path="/chat" element={<LumiChat />} />
              <Route path="/settings" element={<LumiSettings />} />
              <Route path="/logs" element={<LumiLogs />} />
              <Route path="/tools" element={<LumiTools />} />
              <Route path="/skills" element={<LumiSkills />} />
              <Route path="/memory" element={<LumiMemory />} />
              <Route path="/tasks" element={<LumiTasks />} />
              <Route path="/media" element={<LumiMedia />} />
              <Route path="/mail" element={<LumiMail />} />
              <Route path="/automation" element={<LumiAutomation />} />
              <Route path="/devices" element={<LumiDevices />} />
              <Route path="/jarvis" element={<LumiJarvis />} />
              <Route path="*" element={<CommandCenter />} />
            </Routes>
          </ErrorBoundary>
        </Suspense>
        {/* 全局权限确认弹窗：任何页面触发高风险操作都会弹出 */}
        <LumiPermissionGate />
      </HashRouter>
    </AppProvider>
  );
}
