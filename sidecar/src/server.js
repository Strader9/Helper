// PC Guardian 常驻侧车入口。
//
// 职责（第一阶段）：加载模型 provider 配置，起 Socket.IO 服务，
// 暴露流式对话通道与健康查询。后续阶段在此之上叠加 agent 编排、
// 定时/自主调度、MCP 客户端与本地工具桥调用。

import { createServer } from 'node:http';
import { Server } from 'socket.io';
import { ModelRouter } from './llm/router.js';

const PORT = Number(process.env.SIDECAR_PORT ?? 4770);
const HOST = process.env.SIDECAR_HOST ?? '127.0.0.1';

/**
 * 加载 provider 配置。优先级：
 *   1. SIDECAR_PROVIDERS 环境变量（JSON 数组）
 *   2. ./providers.json（与 package.json 同级）
 *   3. 默认本地 Ollama
 *
 * 每项的 { apiKey } 只从环境变量/文件读，绝不写进日志。
 */
export function loadProviders() {
  if (process.env.SIDECAR_PROVIDERS) {
    try {
      return JSON.parse(process.env.SIDECAR_PROVIDERS);
    } catch (err) {
      console.error('[sidecar] 解析 SIDECAR_PROVIDERS 失败，回退默认配置:', err.message);
    }
  }
  return [
    {
      id: 'ollama-local',
      name: 'Ollama 本地',
      protocol: 'ollama',
      baseUrl: process.env.OLLAMA_BASE_URL ?? 'http://localhost:11434',
      model: process.env.SIDECAR_DEFAULT_MODEL ?? 'qwen3:8b',
    },
  ];
}

function buildServer() {
  const router = new ModelRouter(loadProviders());
  const httpServer = createServer();
  const io = new Server(httpServer, {
    cors: { origin: '*', methods: ['GET', 'POST'] },
    maxHttpBufferSize: 1e7,
  });

  io.on('connection', (socket) => {
    socket.emit('sidecar:hello', { version: '0.1.0', providers: router.count });

    socket.on('providers:health', (ack) => {
      if (typeof ack === 'function') ack({ providers: router.health() });
    });

    socket.on('chat:start', async (payload, ack) => {
      const { messages, tools, temperature, model, extraOptions } = payload ?? {};
      if (!Array.isArray(messages) || messages.length === 0) {
        if (typeof ack === 'function') ack({ ok: false, error: 'messages 不能为空' });
        return;
      }
      if (typeof ack === 'function') ack({ ok: true });

      try {
        for await (const chunk of router.chatStream({
          messages,
          tools,
          temperature,
          model,
          extraOptions,
        })) {
          socket.emit('chat:chunk', chunk);
        }
      } catch (err) {
        socket.emit('chat:chunk', { type: 'error', message: err.message });
      }
      socket.emit('chat:chunk', { type: 'end' });
    });
  });

  return { httpServer, io, router };
}

const { httpServer, router } = buildServer();
httpServer.listen(PORT, HOST, () => {
  console.log(
    `[sidecar] listening on http://${HOST}:${PORT} (providers=${router.count}, default=${router.defaultModel})`,
  );
});
