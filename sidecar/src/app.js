// sidecar 组装工厂：把模型路由、MCP 管理器、HTTP 面、Socket.IO 面装到一起，
// 但不监听端口。server.js 与测试都从这里拿实例，保证启动路径与测试路径一致。

import { Server } from 'socket.io';
import { createLogger } from './logging.js';
import { loadProviders, loadMcpServers, saveMcpServers } from './config.js';
import { ModelRouter } from './llm/router.js';
import { McpManager } from './mcp/index.js';
import { createHttpApp } from './http/app.js';
import { registerSocket } from './socket.js';

export function createSidecarServer({ providers, mcpServers, logger } = {}) {
  const log = logger ?? createLogger('sidecar');
  const router = new ModelRouter(providers ?? loadProviders(log), { logger: log });
  const mcp = new McpManager(mcpServers ?? loadMcpServers(log), {
    logger: log,
    // 测试或嵌入方显式传配置时不碰真实配置文件。
    onChange: mcpServers === undefined ? saveMcpServers : undefined,
  });

  const httpServer = createHttpApp({ router, mcp, logger: log });
  const io = new Server(httpServer, {
    cors: { origin: '*', methods: ['GET', 'POST'] },
    maxHttpBufferSize: 1e7,
  });
  registerSocket(io, router, mcp, { logger: log });

  /** 优雅关闭：先停 Socket.IO，再停 HTTP，最后断开 MCP 子进程。 */
  const close = async () => {
    await new Promise((resolve) => io.close(resolve));
    await new Promise((resolve) => httpServer.close(resolve));
    await mcp.closeAll();
  };

  return { httpServer, io, router, mcp, logger: log, close };
}
