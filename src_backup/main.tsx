import React from "react";
import ReactDOM from "react-dom/client";
import { HashRouter } from "react-router-dom";
import { Toaster } from "sonner";
import App from "./App";
// 设计系统唯一入口（对齐 EAssistantCore）：token、基础层、工具类全在这一个文件里。
import "./styles/system.css";

ReactDOM.createRoot(document.getElementById("root")!).render(
  <React.StrictMode>
    <HashRouter>
      <App />
      <Toaster
        position="bottom-right"
        theme="dark"
        toastOptions={{
          style: {
            background: "var(--color-elevated)",
            border: "1px solid var(--color-line-strong)",
            color: "var(--color-foreground)",
            fontSize: "13px",
            borderRadius: "12px",
            boxShadow: "var(--shadow-lg)",
          },
        }}
      />
    </HashRouter>
  </React.StrictMode>
);
