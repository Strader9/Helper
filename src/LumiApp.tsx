import { Component, lazy, Suspense, type ReactNode, useEffect, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { HashRouter, Navigate, Route, Routes } from 'react-router-dom';
import { AppProvider } from './contexts/AppContext';
import { LumiPermissionGate } from './components/lumi/LumiPermissionGate';
import { LoadingSpinner } from './components/lumi/LumiShared';
import MiniChat from './pages/MiniChat';

// This is the only app root. The former particle-dashboard landing page was a
// visual prototype with hard-coded state; Jarvis is now the main window entry.
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
          <div className="text-lg font-bold text-red-400">页面渲染错误</div>
          <div className="max-w-md text-center font-mono text-sm text-white/60">{this.state.error}</div>
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

function MainWindowRoutes() {
  return (
    <Suspense fallback={<PageFallback />}>
      <ErrorBoundary>
        <Routes>
          <Route path="/" element={<Navigate to="/jarvis" replace />} />
          <Route path="/jarvis" element={<LumiJarvis />} />
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
          <Route path="*" element={<Navigate to="/jarvis" replace />} />
        </Routes>
      </ErrorBoundary>
    </Suspense>
  );
}

function WindowMode() {
  const [isMini, setIsMini] = useState<boolean | null>(null);

  useEffect(() => {
    invoke<boolean>('is_mini_window')
      .then(setIsMini)
      .catch(() => setIsMini(false));
  }, []);

  if (isMini === null) return <PageFallback />;
  return isMini ? <MiniChat /> : <MainWindowRoutes />;
}

export default function LumiApp() {
  return (
    <AppProvider>
      <HashRouter>
        <WindowMode />
        <LumiPermissionGate />
      </HashRouter>
    </AppProvider>
  );
}
