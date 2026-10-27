import { useCallback, useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { Routes, Route, Navigate, useNavigate } from "react-router-dom";
import { TooltipProvider } from "./components/ui/primitives";
import Sidebar from "./components/Sidebar";
import { CommandPalette } from "./components/CommandPalette";
import Chat from "./pages/Chat";
import Dashboard from "./pages/Dashboard";
import Settings from "./pages/Settings";
import Logs from "./pages/Logs";
import GitHub from "./pages/GitHub";
import MiniChat from "./pages/MiniChat";
import WebStudio from "./pages/WebStudio";
import Tasks from "./pages/Tasks";
import Media from "./pages/Media";
import Mail from "./pages/Mail";
import Tools from "./pages/Tools";
import Memory from "./pages/Memory";
import Skills from "./pages/Skills";
import Devices from "./pages/Devices";
import { CommandCenter } from "./pages/CommandCenter";
import { useLocation } from "react-router-dom";
import { applyTheme } from "./lib/theme";

const COLLAPSE_KEY = "pcg.sidebar.collapsed";

function App() {
  const [isMini, setIsMini] = useState<boolean | null>(null);
  const [collapsed, setCollapsed] = useState<boolean>(() => {
    try {
      return localStorage.getItem(COLLAPSE_KEY) === "1";
    } catch {
      return false;
    }
  });
  const [commandOpen, setCommandOpen] = useState(false);
  const navigate = useNavigate();
  const location = useLocation();
  const isCommandCenter = location.pathname === "/";

  useEffect(() => {
    invoke<boolean>("is_mini_window")
      .then(setIsMini)
      .catch(() => setIsMini(false));
  }, []);

  useEffect(() => {
    invoke<{ key: string; value: string }[]>("get_settings")
      .then((items) => {
        const theme = items.find((s) => s.key === "ui.theme")?.value ?? "dark";
        applyTheme(theme);
      })
      .catch(() => applyTheme("dark"));

    let unlisten: (() => void) | null = null;
    listen<string>("theme:changed", (event) => {
      applyTheme(String(event.payload));
    }).then((fn) => {
      unlisten = fn;
    });
    return () => {
      if (unlisten) unlisten();
    };
  }, []);

  useEffect(() => {
    let unlisten: (() => void) | null = null;
    listen("navigate:settings", () => navigate("/settings")).then((fn) => {
      unlisten = fn;
    });
    return () => {
      if (unlisten) unlisten();
    };
  }, [navigate]);

  const toggleCollapse = useCallback(() => {
    setCollapsed((prev) => {
      const next = !prev;
      try {
        localStorage.setItem(COLLAPSE_KEY, next ? "1" : "0");
      } catch {
        /* 隐私模式下写不进去，忽略即可 */
      }
      return next;
    });
  }, []);

  // ⌘K / Ctrl+K 打开命令面板。Esc 关闭交给 Dialog 自己处理。
  useEffect(() => {
    function onKey(e: KeyboardEvent) {
      const mod = e.metaKey || e.ctrlKey;
      if (mod && e.key.toLowerCase() === "k") {
        e.preventDefault();
        setCommandOpen((v) => !v);
      }
    }
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, []);

  if (isMini === null) {
    return (
      <div className="ds flex h-screen items-center justify-center bg-[var(--color-background)]">
        <div className="h-5 w-5 animate-ds-spin rounded-full border-2 border-[var(--color-line-strong)] border-t-[var(--color-brand)]" />
      </div>
    );
  }

  if (isMini) {
    return <MiniChat />;
  }

  // 指挥中心：全屏布局，无侧栏
  if (isCommandCenter) {
    return (
      <TooltipProvider delayDuration={240}>
        <Routes>
          <Route path="/" element={<CommandCenter />} />
        </Routes>
        <CommandPalette open={commandOpen} onOpenChange={setCommandOpen} />
      </TooltipProvider>
    );
  }

  return (
    <TooltipProvider delayDuration={240}>
      <div className="ds flex h-screen w-screen overflow-hidden bg-[var(--color-canvas)]">
        <Sidebar
          collapsed={collapsed}
          onToggleCollapse={toggleCollapse}
          onOpenCommand={() => setCommandOpen(true)}
        />
        <main className="flex min-w-0 flex-1 flex-col overflow-hidden bg-[var(--color-background)]">
          <Routes>
            <Route path="/chat" element={<Chat />} />
            <Route path="/dashboard" element={<Dashboard />} />
            <Route path="/tasks" element={<Tasks />} />
            <Route path="/media" element={<Media />} />
            <Route path="/mail" element={<Mail />} />
            <Route path="/automation" element={<Navigate to="/chat" replace />} />
            <Route path="/tools" element={<Tools />} />
            <Route path="/memory" element={<Memory />} />
            <Route path="/skills" element={<Skills />} />
            <Route path="/devices" element={<Devices />} />
            <Route path="/settings" element={<Settings />} />
            <Route path="/logs" element={<Logs />} />
            <Route path="/github" element={<GitHub />} />
            <Route path="/web-studio" element={<WebStudio />} />
            <Route path="*" element={<Navigate to="/chat" replace />} />
          </Routes>
        </main>
        <CommandPalette open={commandOpen} onOpenChange={setCommandOpen} />
      </div>
    </TooltipProvider>
  );
}

export default App;
