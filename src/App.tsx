import { useState, useEffect } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { Routes, Route, Navigate, useNavigate } from "react-router-dom";
import Sidebar from "./components/Sidebar";
import Chat from "./pages/Chat";
import Dashboard from "./pages/Dashboard";
import Settings from "./pages/Settings";
import Logs from "./pages/Logs";
import GitHub from "./pages/GitHub";
import MiniChat from "./pages/MiniChat";
import WebStudio from "./pages/WebStudio";
import "./App.css";
import "./cyberpunk.css";

/**
 * 把主题写到 <html data-theme>，CSS 里靠 `:root[data-theme="light"]`
 * 覆盖颜色变量。默认的 dark 不设属性——这样 CSS 里的深色定义
 * 不需要额外维护一份 `[data-theme="dark"]` 副本。
 */
function applyTheme(theme: string) {
  const root = document.documentElement;
  if (theme === "light") {
    root.setAttribute("data-theme", "light");
  } else {
    root.removeAttribute("data-theme");
  }
}

function App() {
  const [isMini, setIsMini] = useState<boolean | null>(null);
  const navigate = useNavigate();

  useEffect(() => {
    invoke<boolean>("is_mini_window")
      .then(setIsMini)
      .catch(() => setIsMini(false));
  }, []);

  // 主题：启动时从设置里读一次并应用到 <html>，让浅色用户的界面在
  // 第一帧就是对的（而不是先闪一下深色再切过去）。
  useEffect(() => {
    invoke<{ key: string; value: string }[]>("get_settings")
      .then((items) => {
        const theme = items.find((s) => s.key === "ui.theme")?.value ?? "dark";
        applyTheme(theme);
      })
      .catch(() => applyTheme("dark"));

    // 设置页保存主题后广播，让主界面立刻跟随，无需刷新。
    let unlisten: (() => void) | null = null;
    listen<string>("theme:changed", (event) => {
      applyTheme(String(event.payload));
    }).then((fn) => { unlisten = fn; });
    return () => { if (unlisten) unlisten(); };
  }, []);

  // V21: 托盘菜单"设置"导航
  useEffect(() => {
    let unlisten: (() => void) | null = null;
    listen("navigate:settings", () => {
      navigate("/settings");
    }).then((fn) => { unlisten = fn; });
    return () => { if (unlisten) unlisten(); };
  }, [navigate]);

  if (isMini === null) {
    return <div className="app-loading">Loading...</div>;
  }

  if (isMini) {
    return <MiniChat />;
  }

  return (
    <div className="app-container">
      <Sidebar />
      <main className="main-content">
        <Routes>
          <Route path="/" element={<Navigate to="/chat" replace />} />
          <Route path="/chat" element={<Chat />} />
          <Route path="/dashboard" element={<Dashboard />} />
          <Route path="/settings" element={<Settings />} />
          <Route path="/logs" element={<Logs />} />
          <Route path="/github" element={<GitHub />} />
          <Route path="/web-studio" element={<WebStudio />} />
        </Routes>
      </main>
    </div>
  );
}

export default App;
