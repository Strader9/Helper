import { useState, useEffect, useCallback, type CSSProperties } from 'react';
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
  ShieldCheck,
  UserRound,
  Users,
  Wrench,
} from 'lucide-react';
import { AppProvider } from './contexts/AppContext';
import { LumiCoreSphere, type LumiCoreOrbitTask } from './components/LumiCoreSphere';
import Galaxy from './components/reactbits/Galaxy';
import GradientWaves from './components/reactbits/GradientWaves';
import { GridScan } from './components/reactbits/GridScan';
import type { MissionTheme } from './components/MissionCoreMaterial';
import { LumiPermissionGate } from './components/lumi/LumiPermissionGate';
import { useVoice } from './hooks/useVoice';
import './index.css';
import './styles/deep-space-command.css';

// 左侧功能入口（对齐 LumiCore 截图，点击跳转对应页面）
//
// 原先用 emoji + 纯色圆角块，在深色界面上会显得像儿童积木——emoji 本身
// 自带彩色描边和固定字重，无法与周围灰度层级协调。改成单色线性图标 +
// 玻璃底板后，颜色只作为"身份标识"出现在图标上，不再整块铺开。
const APP_ENTRIES = [
  { icon: ScrollText, label: '运行日志', color: '#7dd3fc', path: '/logs' },
  { icon: Wrench, label: '工具', color: '#fcd34d', path: '/tools' },
  { icon: Puzzle, label: '技能大厅', color: '#6ee7b7', path: '/skills' },
  { icon: Users, label: '设备', color: '#93c5fd', path: '/devices' },
  { icon: Brain, label: '记忆化身', color: '#d8b4fe', path: '/memory' },
  { icon: Palette, label: '形象设计室', color: '#67e8f9', path: '/media' },
  // 「声音」原本和底栏的「设置」指向同一页，重复且没内容；
  // 换成贾维斯——语音输入与播报都在那一页，入口才有落点。
  { icon: Ear, label: '贾维斯', color: '#a5b4fc', path: '/jarvis' },
  { icon: Music, label: '自动化', color: '#fda4af', path: '/automation' },
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

interface ThemeOption {
  id: MissionTheme;
  name: string;
  engine: string;
  description: string;
  accent: string;
}

const THEME_OPTIONS: ThemeOption[] = [
  { id: 'oscillation', name: '振荡', engine: 'GRADIENT WAVES', description: '雾状流体', accent: '#ff9ffc' },
  { id: 'nebula', name: '星云', engine: 'GALAXY', description: '璀璨深空', accent: '#a78bfa' },
  { id: 'cyber', name: '赛博', engine: 'GRID SCAN', description: '终端矩阵', accent: '#35ff82' },
];

function CommandCenter() {
  const navigate = useNavigate();
  const location = useLocation();
  const [activeTheme, setActiveTheme] = useState<MissionTheme>('nebula');
  const [learningStats, setLearningStats] = useState({ interactions: 128, optimizations: 3, status: '正在学习你的使用习惯...' });
  const [tasks, setTasks] = useState<LumiCoreOrbitTask[]>([
    { id: '1', title: '系统监控', state: 'working', active: true },
    { id: '2', title: '记忆整理', state: 'queued', active: true },
  ]);

  const { isListening, transcript, startListening, stopListening, supported } = useVoice();

  // 接入真实数据：活跃任务
  useEffect(() => {
    invoke<any>('get_active_task')
      .then((task) => {
        if (task && task.id) {
          setTasks([{
            id: String(task.id),
            title: task.title || '执行中任务',
            state: task.status === 'completed' ? 'completed' : task.status === 'failed' ? 'attention' : 'working',
            active: true,
          }]);
        }
      })
      .catch(() => { /* 保持默认任务 */ });

    invoke<any[]>('get_tasks')
      .then((taskList) => {
        if (Array.isArray(taskList) && taskList.length > 0) {
          const active = taskList.filter((t: any) => t.status === 'running' || t.status === 'pending').slice(0, 3);
          if (active.length > 0) {
            setTasks(active.map((t: any) => ({
              id: String(t.id),
              title: t.title || t.name || '任务',
              state: t.status === 'running' ? 'working' : 'queued',
              active: true,
            })));
          }
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

  // 语音识别结果自动填入聊天
  useEffect(() => {
    if (transcript && transcript.trim()) {
      // 语音识别完成后跳转到聊天页并传递文本
      navigate('/chat', { state: { voiceText: transcript } });
    }
  }, [transcript, navigate]);

  return (
    <div className="mission-control relative h-screen w-full overflow-hidden" data-mission-theme={activeTheme}>
      <div className="mission-control__background" aria-hidden="true">
        {activeTheme === 'oscillation' && (
          <GradientWaves
            className="mission-control__theme-effect"
            horizonColor="#5227ff"
            waveColor="#ff9ffc"
            crestColor="#ffffff"
            backgroundColor="#02030a"
            speed={0.4}
            amplitude={2.5}
            waveScale={0.6}
            waveRatio={0.9}
            swell={35}
            turbulence={20}
            tilt={64}
            zoom={1}
            height={5.5}
            fogDepth={15}
            relief={1}
            lightAngle={120}
            brightness={1}
            opacity={1}
            grain={0.2}
            quality="medium"
            intro
            mouseInteraction
            parallaxStrength={0.5}
            mouseStrength={0.6}
            swellOnClick
            dpr={1}
          />
        )}
        {activeTheme === 'nebula' && (
          <Galaxy
            className="mission-control__theme-effect mission-control__galaxy"
            starColors={['#67e8f9', '#60a5fa', '#c084fc', '#f6c76e']}
            density={1.18}
            starSpeed={0.5}
            speed={0.86}
            layers={6}
            starSize={1.08}
            glowIntensity={0.48}
            flareIntensity={1.15}
            twinkleIntensity={0.58}
            rotationSpeed={0.08}
            mouseInteraction
            mouseRepulsion
            repulsionStrength={1.4}
            clickWarp
            intro
            transparent={false}
            backgroundColor="#01030a"
            opacity={1}
            dpr={1}
          />
        )}
        {activeTheme === 'cyber' && (
          <GridScan
            className="mission-control__theme-effect"
            linesColor="#174635"
            scanColor="#35ff82"
            cells={11}
            lineThickness={1}
            lineStyle="solid"
            depth={3.6}
            depthFade={0.66}
            parallax={1}
            points
            reflection={0.42}
            shading={0.78}
            scanDirection="pingpong"
            scanDuration={2.8}
            scanDelay={1.2}
            scanWidth={1}
            trail={0.58}
            glow={0.82}
            autoScan
            interactive
            clickEffect="ping"
            enableWebcam={false}
            showPreview={false}
            enableGyro={false}
            intro
            dpr={1}
          />
        )}
      </div>

      <header className="mission-header">
        <div className="mission-brand">
          <span className="mission-brand__mark"><ShieldCheck size={15} /></span>
          <span>
            <strong>PC GUARDIAN</strong>
            <small>LOCAL-FIRST AI COMMAND</small>
          </span>
        </div>
        <div className="mission-header__coordinate" aria-hidden="true">ORBITAL NODE · CN-01</div>
        <div className="mission-header__status">
          <span><i />系统安全</span>
          <span>{String(tasks.filter(task => task.active).length).padStart(2, '0')} 项任务</span>
        </div>
      </header>

      <aside className="mission-rail" aria-label="功能入口">
        <div className="mission-section-label">SYSTEMS</div>
        {APP_ENTRIES.map((entry, i) => {
          const Icon = entry.icon;
          return (
            <button key={entry.label} className="mission-rail__item" onClick={() => navigate(entry.path)}>
              <span className="mission-rail__index">{String(i + 1).padStart(2, '0')}</span>
              <span className="mission-rail__icon" style={{ '--mission-accent': entry.color } as CSSProperties}>
                <Icon size={16} strokeWidth={1.55} />
              </span>
              <span className="mission-rail__label">{entry.label}</span>
            </button>
          );
        })}
      </aside>

      <main className="mission-core-stage">
        <LumiCoreSphere
          tasks={tasks}
          theme={activeTheme}
          state={isListening ? 'attention' : tasks.some(task => task.state === 'working') ? 'working' : 'ready'}
          labels={{
            aria: 'PC Guardian 指挥中心',
            lumiCore: 'GUARDIAN CORE',
            ready: '系统待命',
            working: '任务执行中',
            attention: '语音链路开启',
            noTasks: '当前轨道无任务',
          }}
        />
      </main>

      <aside className="mission-intelligence" aria-label="视觉主题与学习状态">
        <div className="mission-panel mission-panel--personas">
          <div className="mission-panel__header">
            <span>VISUAL MATRIX</span>
            <span>皮肤主题</span>
          </div>
          <div className="mission-personas">
            {THEME_OPTIONS.map((theme) => {
              const selected = activeTheme === theme.id;
              return (
                <button
                  key={theme.id}
                  className={`mission-persona ${selected ? 'is-active' : ''}`}
                  style={{ '--mission-accent': theme.accent } as CSSProperties}
                  onClick={() => setActiveTheme(theme.id)}
                  aria-pressed={selected}
                >
                  <span className="mission-persona__signal" />
                  <span className="mission-persona__body">
                    <span className="mission-persona__name">{theme.name}</span>
                    <span className="mission-persona__meta">{theme.engine} · {theme.description}</span>
                  </span>
                  <span className="mission-persona__level">{selected ? 'ON' : '—'}</span>
                  <span className="mission-persona__track"><i style={{ width: selected ? '100%' : '18%' }} /></span>
                </button>
              );
            })}
          </div>
        </div>

        <div className="mission-panel mission-panel--learning">
          <div className="mission-panel__header">
            <span>LEARNING STREAM</span>
            <span className="mission-live"><i />实时</span>
          </div>
          <p>{learningStats.status}</p>
          <div className="mission-metrics">
            <div><strong>{learningStats.interactions}</strong><span>交互样本</span></div>
            <div><strong>{learningStats.optimizations}</strong><span>今日优化</span></div>
          </div>
        </div>
      </aside>

      <div className="mission-voice">
        <button
          className={isListening ? 'is-listening' : ''}
          onClick={handleVoiceToggle}
          title={supported ? '点击开始语音会话' : '当前浏览器不支持语音识别'}
        >
          {isListening ? <MicOff size={20} strokeWidth={1.65} /> : <Mic size={20} strokeWidth={1.65} />}
        </button>
        <span>{isListening ? `正在监听 ${transcript || ''}` : 'VOICE LINK · 点击建立语音链路'}</span>
      </div>

      <nav className="mission-dock" aria-label="主导航">
        {BOTTOM_NAV.map((item) => {
          const Icon = item.icon;
          const isActive = location.pathname === item.path;
          return (
            <button
              key={item.label}
              className={isActive ? 'is-active' : ''}
              onClick={() => navigate(item.path)}
              title={item.label}
            >
              <Icon size={16} strokeWidth={1.55} />
              <span>{item.label}</span>
            </button>
          );
        })}
      </nav>

      <div className="mission-footer-data" aria-hidden="true">
        <span>ENCRYPTED LOCAL CHANNEL</span>
        <span>OLLAMA / SQLITE / TAURI</span>
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
