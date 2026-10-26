// Tauri 适配层：mock useSocket
import { useEffect } from 'react';
import { socketService } from '../services/socketService';

export function useSocket() {
  useEffect(() => {
    socketService.connect();
    return () => socketService.disconnect();
  }, []);

  return {
    socket: socketService,
    connected: true,
    on: socketService.on.bind(socketService),
    emit: socketService.emit.bind(socketService),
  };
}

export function initializeSharedSocketRuntime() {
  socketService.connect();
}
