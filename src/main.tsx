import React from "react";
import ReactDOM from "react-dom/client";
import { Toaster } from "sonner";
import LumiApp from "./LumiApp";
import "./index.css";

ReactDOM.createRoot(document.getElementById("root")!).render(
  <React.StrictMode>
    <LumiApp />
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
