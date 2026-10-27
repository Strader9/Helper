import { useState, useEffect, useCallback, type CSSProperties } from 'react';
import { HashRouter, Navigate, Routes, Route, useNavigate, useLocation } from 'react-router-dom';
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import {
  Brain,
  Ear,
  MessageSquare,
  Mic,
  MicOff,
  Monitor,
  Orbit,
  Palette,
  Puzzle,
  ScrollText,
} from 'lucide-react';
import { AppProvider } from './contexts/AppContext';
import { EAssistantCoreSphere, type EAssistantCoreOrbitTask } from './components/EAssistantCoreSphere';
import Galaxy from './components/reactbits/Galaxy';
import GradientWaves from './components/reactbits/GradientWaves';
import { GridScan } from './components/reactbits/GridScan';
import type { MissionTheme } from './components/MissionCoreMaterial';
import { EAssistantPermissionGate } from './components/e-assistant/EAssistantPermissionGate';
import { EAssistantDeviceSummary } from './components/e-assistant/EAssistantDeviceSummary';
import { useVoice } from './hooks/useVoice';
import { applyTheme } from './lib/theme';
import './index.css';
import './styles/deep-space-command.css';

// 左侧功能入口（对齐 EAssistantCore 截图，点击跳转对应页面）
//
// 原先用 emoji + 纯色圆角块，在深色界面上会显得像儿童积木——emoji 本身
// 自带彩色描边和固定字重，无法与周围灰度层级协调。改成单色线性图标 +
// 玻璃底板后，颜色只作为"身份标识"出现在图标上，不再整块铺开。
const APP_ENTRIES = [
  { icon: ScrollText, label: '运行日志', color: '#7dd3fc', path: '/logs' },
  { icon: Puzzle, label: '能力中心', color: '#6ee7b7', path: '/capabilities' },
  { icon: Brain, label: '记忆化身', color: '#d8b4fe', path: '/memory' },
  { icon: Palette, label: '形象设计室', color: '#67e8f9', path: '/media' },
  // 「声音」原本和底栏的「设置」指向同一页，重复且没内容；
  // 换成贾维斯——语音输入与播报都在那一页，入口才有落点。
  { icon: Ear, label: '实时感知', color: '#d6a43a', path: '/live' },
];

// 底部导航
const BOTTOM_NAV = [
  { icon: Orbit, label: '指挥中心', path: '/' },
  { icon: Monitor, label: '系统', path: '/settings' },
  { icon: MessageSquare, label: '聊天', path: '/chat' },
];

interface ThemeOption {
  id: MissionTheme;
  name: string;
  engine: string;
  description: string;
  accent: string;
}

interface HomeTask {
  id: string;
  goal?: string;
  title?: string;
  status?: string;
}

interface ConversationSummary {
  message_count?: number;
  messageCount?: number;
}

interface MemoryProfileSummary {
  active_memories?: number;
  activeMemories?: number;
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
  const [learningStats, setLearningStats] = useState<{
    interactions: number | null;
    memories: number | null;
    status: string;
  }>({ interactions: null, memories: null, status: '正在读取本机适应数据…' });
  const [tasks, setTasks] = useState<EAssistantCoreOrbitTask[]>([]);

  const { isListening, transcript, startListening, stopListening, supported } = useVoice();

  // 接入真实数据：活跃任务
  useEffect(() => {
    invoke<HomeTask[]>('get_active_tasks', { limit: 6 })
      .then((taskList) => {
        const active = Array.isArray(taskList) ? taskList.slice(0, 3) : [];
        setTasks(active.map((task) => ({
          id: String(task.id),
          title: task.goal || task.title || '未命名任务',
          state: task.status === 'PENDING' || task.status === 'PLANNING' ? 'queued' : 'working',
          active: true,
        })));
      })
      .catch(() => setTasks([]));
  }, []);

  // 适应统计只展示能从数据库验证的数据，不再用演示数字填充失败请求。
  useEffect(() => {
    void Promise.allSettled([
      invoke<ConversationSummary[]>('get_conversations'),
      invoke<MemoryProfileSummary>('get_memory_profile'),
    ]).then(([conversationResult, memoryResult]) => {
      const interactions = conversationResult.status === 'fulfilled'
        ? conversationResult.value.reduce(
          (sum, conversation) => sum + Number(conversation.message_count ?? conversation.messageCount ?? 0),
          0,
        )
        : null;
      const memories = memoryResult.status === 'fulfilled'
        ? Number(memoryResult.value.active_memories ?? memoryResult.value.activeMemories ?? 0)
        : null;
      const unavailable = interactions === null && memories === null;
      setLearningStats({
        interactions,
        memories,
        status: unavailable
          ? '本机适应数据暂时不可用'
          : memories && memories > 0
            ? `已从 ${memories} 条有效记忆中适应你的偏好`
            : '尚未形成长期偏好，可在记忆化身中添加',
      });
    });
  }, []);

  const handleVoiceToggle = useCallback(() => {
    if (isListening) {
      stopListening();
    } else {
      startListening();
    }
  }, [isListening, startListening, stopListening]);

  // 首页话筒是 EAssistant 的语音入口，识别完成后直接转入 EAssistant 对话。
  useEffect(() => {
    if (transcript && transcript.trim()) {
      navigate('/live', { state: { voiceText: transcript } });
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
          <span className="mission-brand__mark mission-brand__mark--logo">
            <img src="/brand/e-assisant-logo.png" alt="" aria-hidden="true" />
          </span>
          <span>
            <strong>E-ASSISANT</strong>
            <small>PERSONAL AI SYSTEM</small>
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
        <EAssistantCoreSphere
          tasks={tasks}
          showTaskStatus={false}
          theme={activeTheme}
          state={isListening ? 'attention' : tasks.some(task => task.state === 'working') ? 'working' : 'ready'}
          labels={{
            aria: 'E-Assisant 指挥中心',
            eAssistantCore: 'E-ASSISANT CORE',
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
            <span>ADAPTATION STREAM</span>
            <span className="mission-live"><i />本机</span>
          </div>
          <p>{learningStats.status}</p>
          <div className="mission-metrics">
            <div><strong>{learningStats.interactions ?? '—'}</strong><span>对话消息</span></div>
            <div><strong>{learningStats.memories ?? '—'}</strong><span>有效记忆</span></div>
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

      <EAssistantDeviceSummary onOpen={() => navigate('/devices')} />

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
import { LoadingSpinner } from './components/e-assistant/EAssistantShared';

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
        <div className="eassistant-surface flex h-screen w-full flex-col items-center justify-center gap-4 text-white">
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

const EAssistantChat = lazy(() => import('./pages/e-assistant/EAssistantChat'));
const EAssistantSettings = lazy(() => import('./pages/e-assistant/EAssistantSettings'));
const EAssistantLogs = lazy(() => import('./pages/e-assistant/EAssistantLogs'));
const EAssistantCapabilities = lazy(() => import('./pages/e-assistant/EAssistantCapabilities'));
const EAssistantMemory = lazy(() => import('./pages/e-assistant/EAssistantMemory'));
const EAssistantTasks = lazy(() => import('./pages/e-assistant/EAssistantTasks'));
const EAssistantMedia = lazy(() => import('./pages/e-assistant/EAssistantMedia'));
const EAssistantMail = lazy(() => import('./pages/e-assistant/EAssistantMail'));
const EAssistantDevices = lazy(() => import('./pages/e-assistant/EAssistantDevices'));
const EAssistantLive = lazy(() => import('./pages/e-assistant/EAssistantLive'));
const MiniChat = lazy(() => import('./pages/MiniChat'));

function PageFallback() {
  return (
    <div className="eassistant-surface flex h-screen w-full items-center justify-center">
      <LoadingSpinner label="加载中..." />
    </div>
  );
}

export default function EAssistantApp() {
  const [isMiniWindow, setIsMiniWindow] = useState<boolean | null>(null);

  useEffect(() => {
    invoke<Array<{ key: string; value: string }>>('get_settings')
      .then((items) => applyTheme(items.find((item) => item.key === 'ui.theme')?.value ?? 'dark'))
      .catch(() => applyTheme('dark'));

    let unlisten: (() => void) | undefined;
    void listen<string>('theme:changed', (event) => applyTheme(String(event.payload))).then((stop) => {
      unlisten = stop;
    });

    return () => unlisten?.();
  }, []);

  useEffect(() => {
    invoke<boolean>('is_mini_window')
      .then(setIsMiniWindow)
      .catch(() => setIsMiniWindow(false));
  }, []);

  if (isMiniWindow === null) {
    return <div className="h-screen w-screen bg-transparent" />;
  }

  if (isMiniWindow) {
    return (
      <Suspense fallback={<div className="h-screen w-screen bg-transparent" />}>
        <MiniChat />
      </Suspense>
    );
  }

  return (
    <AppProvider>
      <HashRouter>
        <Suspense fallback={<PageFallback />}>
          <ErrorBoundary>
            <Routes>
              <Route path="/" element={<CommandCenter />} />
              <Route path="/chat" element={<EAssistantChat />} />
              <Route path="/settings" element={<EAssistantSettings />} />
              <Route path="/logs" element={<EAssistantLogs />} />
              <Route path="/capabilities" element={<EAssistantCapabilities />} />
              <Route path="/tools" element={<Navigate to="/capabilities?tab=tools" replace />} />
              <Route path="/skills" element={<Navigate to="/capabilities?tab=skills" replace />} />
              <Route path="/memory" element={<EAssistantMemory />} />
              <Route path="/tasks" element={<EAssistantTasks />} />
              <Route path="/media" element={<EAssistantMedia />} />
              <Route path="/mail" element={<EAssistantMail />} />
              <Route path="/automation" element={<Navigate to="/chat" replace />} />
              <Route path="/devices" element={<EAssistantDevices />} />
              <Route path="/live" element={<EAssistantLive />} />
              <Route path="*" element={<CommandCenter />} />
            </Routes>
          </ErrorBoundary>
        </Suspense>
        {/* 全局权限确认弹窗：任何页面触发高风险操作都会弹出 */}
        <EAssistantPermissionGate />
      </HashRouter>
    </AppProvider>
  );
}
