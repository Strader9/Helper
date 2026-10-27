import React, { useEffect } from "react";
import ReactDOM from "react-dom/client";
import { invoke } from "@tauri-apps/api/core";
import { Toaster } from "sonner";
import EAssistantApp from "./EAssistantApp";

function StartupReadySignal() {
  useEffect(() => {
    invoke("mark_main_window_ready").catch((error) => {
      console.error("Failed to finish startup handoff", error);
    });
  }, []);

  return null;
}

ReactDOM.createRoot(document.getElementById("root")!).render(
  <React.StrictMode>
    <EAssistantApp />
    <StartupReadySignal />
    <Toaster
      position="top-right"
      theme="dark"
      toastOptions={{
        style: {
          background: "rgba(10,14,30,0.9)",
          border: "1px solid rgba(255,255,255,0.1)",
          color: "white",
          fontSize: "13px",
          borderRadius: "12px",
        },
      }}
    />
  </React.StrictMode>
);
