// HTTP 面：让 sidecar 对外表现为一个标准 OpenAI 兼容模型服务 + MCP 薄网关。
//
// 路由：
//   GET  /health               健康探针（Rust 侧 get_model_gateway_status 读取 providers/data）
//   GET  /v1/models            OpenAI 模型列表
//   POST /v1/chat/completions  OpenAI 兼容补全（stream / 非 stream）
//   GET  /mcp/tools            列出 MCP 工具池
//   POST /mcp/call             调用 MCP 工具
//   GET  /mcp/status           MCP server 连接状态
//
// 所有对话请求都把「客户端断连」转成 AbortSignal 向下传，及时中止上游生成。

import { createServer } from 'node:http';
import { TokenUsage } from '../llm/usage.js';
import { SIDECAR_VERSION } from '../config.js';

const MAX_BODY_BYTES = 16 * 1024 * 1024;
const LOCAL_CONTROL_HEADER = 'x-e-assisant-local';
const LOCAL_CONTROL_VALUE = 'capability-manager-v1';

class HttpError extends Error {
  constructor(status, message) {
    super(message);
    this.status = status;
  }
}

function sendJson(res, status, obj) {
  if (res.headersSent || res.destroyed) return;
  res.writeHead(status, { 'Content-Type': 'application/json; charset=utf-8' });
  res.end(JSON.stringify(obj));
}

function sseHeaders(res) {
  res.writeHead(200, {
    'Content-Type': 'text/event-stream; charset=utf-8',
    'Cache-Control': 'no-cache',
    Connection: 'keep-alive',
    'X-Accel-Buffering': 'no',
  });
}

function writeSse(res, obj) {
  if (res.destroyed || res.writableEnded) return false;
  res.write(`data: ${JSON.stringify(obj)}\n\n`);
  return true;
}

/** 读取请求体：JSON 解析、体积上限、客户端断连即失败。 */
function readBody(req, { signal } = {}) {
  return new Promise((resolve, reject) => {
    const chunks = [];
    let size = 0;
    let settled = false;

    const onAbort = () => {
      if (settled) return;
      settled = true;
      reject(new HttpError(499, '请求已取消'));
    };
    signal?.addEventListener('abort', onAbort);

    req.on('data', (chunk) => {
      if (settled) return;
      size += chunk.length;
      if (size > MAX_BODY_BYTES) {
        settled = true;
        reject(new HttpError(413, `请求体超过 ${MAX_BODY_BYTES} 字节上限`));
        req.destroy();
        return;
      }
      chunks.push(chunk);
    });
    req.on('end', () => {
      if (settled) return;
      settled = true;
      const text = Buffer.concat(chunks).toString('utf8');
      if (!text) {
        resolve({});
        return;
      }
      try {
        resolve(JSON.parse(text));
      } catch (err) {
        reject(new HttpError(400, `invalid json: ${err.message}`));
      }
    });
    req.on('error', (err) => {
      if (settled) return;
      settled = true;
      reject(err);
    });
  });
}

function usageToOpenAi(usage) {
  const u = usage ?? TokenUsage.zero();
  return {
    prompt_tokens: u.promptTokens,
    completion_tokens: u.completionTokens,
    total_tokens: u.promptTokens + u.completionTokens,
  };
}

/** router 内部 ChatChunk → OpenAI 兼容 SSE 增量。 */
function toOpenAiDelta(chunk, index) {
  switch (chunk.type) {
    case 'text':
      return {
        choices: [{ index, delta: { content: chunk.text }, finish_reason: null }],
      };
    case 'tool_call':
      return {
        choices: [
          {
            index,
            delta: {
              tool_calls: [
                {
                  index: 0,
                  id: chunk.call?.id ?? `call_${index}`,
                  type: 'function',
                  function: chunk.call?.function ?? { name: '', arguments: '' },
                },
              ],
            },
            finish_reason: null,
          },
        ],
      };
    case 'done':
      return {
        choices: [{ index, delta: {}, finish_reason: chunk.reason ?? 'stop' }],
        usage: usageToOpenAi(chunk.usage),
      };
    case 'error':
      return { error: { message: chunk.message } };
    default:
      return null;
  }
}

/** 模型清单（OpenAI 形态）。 */
function modelList(router) {
  const providers = router.health();
  return {
    object: 'list',
    data: providers
      .filter((p) => p.model)
      .map((p) => ({ id: p.model, object: 'model', created: 0, owned_by: p.name })),
    providers,
  };
}

/** 非流式补全：聚合 router 的全部 chunk。 */
async function collectCompletion(router, body) {
  const parts = [];
  const calls = [];
  let usage = TokenUsage.zero();
  let reason = 'stop';
  let errorMessage = null;

  for await (const chunk of router.chatStream({
    messages: body.messages,
    tools: body.tools,
    temperature: body.temperature,
    model: body.model,
    extraOptions: body.extraOptions,
    signal: body.signal,
  })) {
    if (chunk.type === 'text') parts.push(chunk.text);
    else if (chunk.type === 'tool_call') calls.push(chunk.call);
    else if (chunk.type === 'done') {
      usage = chunk.usage ?? usage;
      reason = chunk.reason ?? reason;
    } else if (chunk.type === 'error') {
      errorMessage = chunk.message;
    }
  }

  return { parts, calls, usage, reason, errorMessage };
}

/**
 * 把路由挂到一个 http.Server 上。
 * @param {import('node:http').Server} httpServer
 * @param {object} router ModelRouter
 * @param {object} [mcp] McpManager
 * @param {object} [options]
 */
export function attachHttpApi(httpServer, router, mcp, { logger } = {}) {
  httpServer.on('request', async (req, res) => {
    const url = new URL(req.url, 'http://localhost');
    const pathname = url.pathname;

    // CORS 预检 / 放行（仅监听 127.0.0.1，保持本地可调用）。
    res.setHeader('Access-Control-Allow-Origin', '*');
    res.setHeader('Access-Control-Allow-Methods', 'GET,POST,DELETE,OPTIONS');
    res.setHeader(
      'Access-Control-Allow-Headers',
      'Content-Type, Authorization, X-E-Assisant-Local',
    );
    if (req.method === 'OPTIONS') {
      res.writeHead(204);
      res.end();
      return;
    }

    try {
      // ===== 健康 / 模型列表 =====
      if (req.method === 'GET' && pathname === '/health') {
        const list = modelList(router);
        sendJson(res, 200, {
          status: 'ok',
          version: SIDECAR_VERSION,
          providers: list.providers,
          mcp: mcp?.status() ?? [],
          data: list.data,
        });
        return;
      }

      if (req.method === 'GET' && pathname === '/v1/models') {
        sendJson(res, 200, modelList(router));
        return;
      }

      // ===== OpenAI 兼容补全 =====
      if (req.method === 'POST' && pathname === '/v1/chat/completions') {
        const abort = new AbortController();
        // IncomingMessage 的 `close` 在请求体正常读取完后也可能触发；用它取消会让
        // 每个正常 POST 都在生成前被中断。只有请求被截断，或响应在完成前断开，
        // 才向模型路由传播取消信号。
        req.on('aborted', () => abort.abort());
        res.on('close', () => {
          if (!res.writableEnded) abort.abort();
        });

        let body;
        try {
          body = await readBody(req, { signal: abort.signal });
        } catch (err) {
          if (err instanceof HttpError && err.status === 499) return;
          sendJson(res, err.status ?? 400, { error: { message: err.message } });
          return;
        }

        if (!Array.isArray(body.messages) || body.messages.length === 0) {
          sendJson(res, 400, { error: { message: 'messages is required' } });
          return;
        }

        const model = body.model ?? router.defaultModel;
        const stream = body.stream === true;

        if (!stream) {
          const { parts, calls, usage, reason, errorMessage } = await collectCompletion(router, {
            ...body,
            signal: abort.signal,
          });
          if (abort.signal.aborted) return;
          if (errorMessage) {
            sendJson(res, 502, { error: { message: errorMessage } });
            return;
          }
          sendJson(res, 200, {
            id: `chatcmpl-${Date.now()}`,
            object: 'chat.completion',
            created: Math.floor(Date.now() / 1000),
            model,
            choices: [
              {
                index: 0,
                message: {
                  role: 'assistant',
                  content: parts.join(''),
                  ...(calls.length ? { tool_calls: calls } : {}),
                },
                finish_reason: reason,
              },
            ],
            usage: usageToOpenAi(usage),
          });
          return;
        }

        // 流式：SSE。
        sseHeaders(res);
        const reqWithSignal = { ...body, signal: abort.signal };
        try {
          for await (const chunk of router.chatStream(reqWithSignal)) {
            if (abort.signal.aborted || res.destroyed) break;
            const delta = toOpenAiDelta(chunk, 0);
            if (delta) writeSse(res, delta);
            if (chunk.type === 'done' || chunk.type === 'error') break;
          }
        } catch (err) {
          writeSse(res, { error: { message: err.message } });
        }
        if (!res.destroyed && !res.writableEnded) {
          res.write('data: [DONE]\n\n');
          res.end();
        }
        return;
      }

      // ===== MCP 转发 =====
      if (req.method === 'GET' && pathname === '/mcp/status') {
        sendJson(res, 200, { servers: mcp?.status() ?? [] });
        return;
      }

      if (req.method === 'POST' && pathname === '/mcp/servers') {
        if (req.headers[LOCAL_CONTROL_HEADER] !== LOCAL_CONTROL_VALUE) {
          sendJson(res, 403, { error: { message: '拒绝未经授权的本机配置请求' } });
          return;
        }
        if (!mcp) {
          sendJson(res, 503, { error: { message: 'sidecar 未启用 MCP 模块' } });
          return;
        }
        let body;
        try {
          body = await readBody(req);
          const installed = await mcp.installServer(body);
          sendJson(res, 201, installed);
        } catch (err) {
          sendJson(res, err.status ?? 400, { error: { message: err.message } });
        }
        return;
      }

      const removeServerMatch = pathname.match(/^\/mcp\/servers\/([^/]+)$/);
      if (req.method === 'DELETE' && removeServerMatch) {
        if (req.headers[LOCAL_CONTROL_HEADER] !== LOCAL_CONTROL_VALUE) {
          sendJson(res, 403, { error: { message: '拒绝未经授权的本机配置请求' } });
          return;
        }
        if (!mcp) {
          sendJson(res, 503, { error: { message: 'sidecar 未启用 MCP 模块' } });
          return;
        }
        try {
          const id = decodeURIComponent(removeServerMatch[1]);
          const removed = await mcp.removeServer(id);
          if (!removed) {
            sendJson(res, 404, { error: { message: `MCP server 不存在：${id}` } });
            return;
          }
          sendJson(res, 200, { removed: true, id });
        } catch (err) {
          sendJson(res, 500, { error: { message: err.message } });
        }
        return;
      }

      if (req.method === 'GET' && pathname === '/mcp/tools') {
        if (!mcp) {
          sendJson(res, 200, { servers: [], tools: [] });
          return;
        }
        try {
          sendJson(res, 200, await mcp.listAllTools());
        } catch (err) {
          sendJson(res, 500, { error: { message: err.message } });
        }
        return;
      }

      if (req.method === 'POST' && pathname === '/mcp/call') {
        if (!mcp) {
          sendJson(res, 503, { error: 'sidecar 未启用 MCP 模块' });
          return;
        }
        let body;
        try {
          body = await readBody(req);
        } catch (err) {
          sendJson(res, err.status ?? 400, { error: { message: err.message } });
          return;
        }
        const { name, arguments: args, server } = body;
        if (typeof name !== 'string' || name.trim().length === 0) {
          sendJson(res, 400, { error: { message: 'name is required' } });
          return;
        }
        try {
          const result = await mcp.callTool({ name, arguments: args, server });
          sendJson(res, result.isError ? 502 : 200, result);
        } catch (err) {
          sendJson(res, 500, { error: { message: err.message } });
        }
        return;
      }

      sendJson(res, 404, { error: { message: `not found: ${req.method} ${pathname}` } });
    } catch (err) {
      logger?.error?.('HTTP 处理异常:', err);
      sendJson(res, 500, { error: { message: err.message ?? 'internal error' } });
    }
  });
}

/**
 * 创建已挂好路由的 http.Server（不监听端口，便于测试与上层组装）。
 */
export function createHttpApp({ router, mcp, logger }) {
  const httpServer = createServer();
  attachHttpApi(httpServer, router, mcp, { logger });
  return httpServer;
}
