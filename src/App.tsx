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
import "./App.css";
import "./cyberpunk.css";

function App() {
  const [isMini, setIsMini] = useState<boolean | null>(null);
  const navigate = useNavigate();

  useEffect(() => {
    invoke<boolean>("is_mini_window")
      .then(setIsMini)
      .catch(() => setIsMini(false));
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
        </Routes>
      </main>
    </div>
  );
}

export default App;
