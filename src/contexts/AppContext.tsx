// Tauri 适配层：扩展版 AppContext，提供 LumiCore 页面所需的接口
import { createContext, useContext, useState, useCallback, ReactNode } from 'react';

export type OperationMode = 'chat' | 'assistant' | 'autonomous';

export interface ToolOverride {
  enabled: boolean;
}

interface AppState {
  user: any | null;
  lang: 'zh' | 'en';
  setLang: (lang: 'zh' | 'en') => void;
  resolvedAppearanceMode: 'dark' | 'light';
  operationMode: OperationMode;
  setOperationMode: (mode: OperationMode) => void;
  t: (key: string, fallback?: string) => string;
  refreshUser: () => Promise<void>;
  handleLogin: () => void;
  isLoginModalOpen: boolean;
  setIsLoginModalOpen: (open: boolean) => void;
  // LumiCore 兼容：工具覆盖
  toolOverrides: Record<string, ToolOverride>;
  setToolOverride: (name: string, override: ToolOverride) => void;
  // LumiCore 兼容：设置缓存
  settings: Record<string, string>;
  setSetting: (key: string, value: string) => void;
  loadSettings: () => Promise<void>;
}

const AppContext = createContext<AppState | null>(null);

export function AppProvider({ children }: { children: ReactNode }) {
  const [lang, setLang] = useState<'zh' | 'en'>('zh');
  const [operationMode, setOperationMode] = useState<OperationMode>('assistant');
  const [toolOverrides, setToolOverrides] = useState<Record<string, ToolOverride>>({});
  const [settings, setSettingsState] = useState<Record<string, string>>({});

  const t = (key: string, fallback?: string) => fallback || key;

  const setToolOverride = useCallback((name: string, override: ToolOverride) => {
    setToolOverrides(prev => ({ ...prev, [name]: override }));
  }, []);

  const setSetting = useCallback((key: string, value: string) => {
    setSettingsState(prev => ({ ...prev, [key]: value }));
  }, []);

  const loadSettings = useCallback(async () => {
    try {
      const { invoke } = await import('@tauri-apps/api/core');
      const items = await invoke<{ key: string; value: string }[]>('get_settings');
      const map: Record<string, string> = {};
      items.forEach(item => { map[item.key] = item.value; });
      setSettingsState(map);
    } catch {
      // Tauri 不可用时保持空设置
    }
  }, []);

  const value: AppState = {
    user: null,
    lang,
    setLang,
    resolvedAppearanceMode: 'dark',
    operationMode,
    setOperationMode,
    t,
    refreshUser: async () => {},
    handleLogin: () => {},
    isLoginModalOpen: false,
    setIsLoginModalOpen: () => {},
    toolOverrides,
    setToolOverride,
    settings,
    setSetting,
    loadSettings,
  };

  return <AppContext.Provider value={value}>{children}</AppContext.Provider>;
}

export function useApp() {
  const ctx = useContext(AppContext);
  if (!ctx) throw new Error('useApp must be used within AppProvider');
  return ctx;
}
