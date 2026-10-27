// Socket.IO 实时通道（前端 / 多端可直接订阅）。
//
// 事件契约（与旧版保持一致）：
//   连接成功 → 服务端推 sidecar:hello { version, providers }
//   providers:health（ack）→ { providers: [...] }
//   chat:start { messages, tools?, temperature?, model?, extraOptions? }
//     先 ack { ok }，随后逐条推 chat:chunk（ChatChunk 语义），
//     最后推一条 { type: 'end' } 表示通道结束。
//
// 每个连接同一时刻只允许一个生成任务：新的 chat:start 会取消旧任务，
// 连接断开也会取消正在进行的上游生成。

import { SIDECAR_VERSION } from './config.js';

export function registerSocket(io, router, _mcp, { logger } = {}) {
  io.on('connection', (socket) => {
    socket.emit('sidecar:hello', { version: SIDECAR_VERSION, providers: router.count });

    socket.on('providers:health', (ack) => {
      if (typeof ack === 'function') ack({ providers: router.health() });
    });

    let active = null;

    socket.on('chat:start', async (payload, ack) => {
      const { messages, tools, temperature, model, extraOptions } = payload ?? {};
      if (!Array.isArray(messages) || messages.length === 0) {
        if (typeof ack === 'function') ack({ ok: false, error: 'messages 不能为空' });
        return;
      }

      active?.abort();
      const abort = new AbortController();
      active = abort;
      if (typeof ack === 'function') ack({ ok: true });

      try {
        for await (const chunk of router.chatStream({
          messages,
          tools,
          temperature,
          model,
          extraOptions,
          signal: abort.signal,
        })) {
          if (abort.signal.aborted || socket.disconnected) break;
          socket.emit('chat:chunk', chunk);
        }
      } catch (err) {
        if (!abort.signal.aborted) {
          socket.emit('chat:chunk', { type: 'error', message: err.message });
        }
      }

      if (!abort.signal.aborted && !socket.disconnected) {
        socket.emit('chat:chunk', { type: 'end' });
      }
      if (active === abort) active = null;
    });

    socket.on('disconnect', () => {
      active?.abort();
      active = null;
    });
  });

  logger?.debug?.('Socket.IO 通道已注册');
}
