// Tauri 适配层：mock useRuntimeStatus
import { useState, useEffect } from 'react';

export interface StructuredRuntimeStatus {
  agentState: string;
  activeTasks: number;
  completedTasks: number;
  failedTasks: number;
  uptimeSeconds: number;
  modelConnected: boolean;
  lastHeartbeat: string;
}

export function useRuntimeStatus() {
  const [status, setStatus] = useState<StructuredRuntimeStatus>({
    agentState: 'idle',
    activeTasks: 0,
    completedTasks: 0,
    failedTasks: 0,
    uptimeSeconds: 0,
    modelConnected: true,
    lastHeartbeat: new Date().toISOString(),
  });

  useEffect(() => {
    const interval = setInterval(() => {
      setStatus(prev => ({
        ...prev,
        uptimeSeconds: prev.uptimeSeconds + 1,
        lastHeartbeat: new Date().toISOString(),
      }));
    }, 1000);
    return () => clearInterval(interval);
  }, []);

  return { status, loading: false, error: null, refresh: async () => {} };
}
