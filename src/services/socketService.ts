// Tauri 适配层：mock socketService，使用 Tauri event 替代
import { listen, type UnlistenFn } from '@tauri-apps/api/event';

type SocketHandler = (data: any) => void;

class MockSocketService {
  private handlers: Map<string, Set<SocketHandler>> = new Map();
  private unlisteners: Map<string, UnlistenFn> = new Map();

  connect() {
    // Tauri 模式下不需要 socket 连接
  }

  disconnect() {
    this.unlisteners.forEach(fn => fn());
    this.unlisteners.clear();
  }

  on(event: string, handler: SocketHandler) {
    if (!this.handlers.has(event)) {
      this.handlers.set(event, new Set());
    }
    this.handlers.get(event)!.add(handler);

    // 同时监听 Tauri 事件
    listen(event, (payload) => {
      this.handlers.get(event)?.forEach(h => h(payload.payload));
    }).then(unlisten => {
      this.unlisteners.set(`${event}_${Date.now()}`, unlisten);
    }).catch(() => {});

    return () => this.off(event, handler);
  }

  off(event: string, handler: SocketHandler) {
    this.handlers.get(event)?.delete(handler);
  }

  emit(event: string, data?: any) {
    // Tauri 模式下通过 invoke 发送
    console.log('[socket mock] emit:', event, data);
  }

  get connected() {
    return true;
  }
}

export const socketService = new MockSocketService();
export type Socket = any;
