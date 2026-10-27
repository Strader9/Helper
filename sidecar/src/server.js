// E-Assisant 常驻侧车入口。
//
// 职责：
// 1. 加载模型 provider 配置，起多 provider 模型网关（OpenAI 兼容 + Ollama）；
// 2. 暴露流式对话通道（HTTP SSE + Socket.IO）与健康查询；
// 3. 承载 MCP 客户端，把用户配置的 MCP server 统一成工具池；
// 4. 监听 SIGINT/SIGTERM 优雅退出（含 MCP stdio 子进程清理）。
//
// 组装逻辑在 app.js，本文件只负责「读配置 → 监听 → 信号处理」。

import { createSidecarServer } from './app.js';
import { listenConfig } from './config.js';

const { httpServer, router, mcp, logger, close } = createSidecarServer();
const { port, host } = listenConfig();

httpServer.listen(port, host, () => {
  logger.info(
    `listening on http://${host}:${port} (providers=${router.count}, default=${router.defaultModel}, mcp_servers=${mcp.count})`,
  );
});

let shuttingDown = false;
async function shutdown(signal) {
  if (shuttingDown) return;
  shuttingDown = true;
  logger.info(`收到 ${signal}，开始关闭…`);
  // 兜底：2 秒内没关干净就强制退出，避免 MCP 子进程挂住整个进程。
  const forceTimer = setTimeout(() => process.exit(0), 2000);
  forceTimer.unref();
  try {
    await close();
  } catch (err) {
    logger.error('关闭过程异常:', err);
  }
  process.exit(0);
}

process.on('SIGINT', () => shutdown('SIGINT'));
process.on('SIGTERM', () => shutdown('SIGTERM'));
process.on('uncaughtException', (err) => logger.error('uncaughtException:', err));
process.on('unhandledRejection', (err) => logger.error('unhandledRejection:', err));

httpServer.on('error', (err) => {
  if (err.code === 'EADDRINUSE') {
    logger.error(`端口 ${port} 已被占用，sidecar 启动失败（可设置 SIDECAR_PORT 换端口）`);
  } else {
    logger.error('HTTP 服务异常:', err);
  }
  process.exitCode = 1;
});
