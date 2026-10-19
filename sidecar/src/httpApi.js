// OpenAI 兼容 HTTP 端点。
//
// 让 sidecar 对外表现为一个标准模型服务：Rust 侧（或任何 OpenAI SDK）
// 用 POST /v1/chat/completions 即可复用它的多 provider failover / 健康 /
// token 计量。协议格式与 backends.js 的解析严格对称。

import { TokenUsage } from './llm/backends.js';

function sse(res) {
  res.writeHead(200, {
    'Content-Type': 'text/event-stream; charset=utf-8',
    'Cache-Control': 'no-cache',
    Connection: 'keep-alive',
    'X-Accel-Buffering': 'no',
  });
}

function writeData(res, obj) {
  res.write(`data: ${JSON.stringify(obj)}\n\n`);
}

/** 把 router 的 chunk 转成 OpenAI 兼容 SSE 增量。 */
function toOpenAiDelta(chunk, index) {
  switch (chunk.type) {
    case 'text':
      return { choices: [{ index, delta: { content: chunk.text }, finish_reason: null }] };
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

function usageToOpenAi(usage) {
  const u = usage ?? TokenUsage.zero();
  return {
    prompt_tokens: u.promptTokens,
    completion_tokens: u.completionTokens,
    total_tokens: u.promptTokens + u.completionTokens,
  };
}

function readBody(req) {
  return new Promise((resolve, reject) => {
    const chunks = [];
    req.on('data', (c) => chunks.push(c));
    req.on('end', () => {
      try {
        resolve(JSON.parse(Buffer.concat(chunks).toString('utf8') || '{}'));
      } catch (err) {
        reject(err);
      }
    });
    req.on('error', reject);
  });
}

/**
 * 挂到 sidecar 的 http.Server 上。
 * @param {import('node:http').Server} httpServer
 * @param {import('./llm/router.js').ModelRouter} router
 */
export function attachHttpApi(httpServer, router) {
  httpServer.on('request', async (req, res) => {
    const url = new URL(req.url, 'http://localhost');

    // 健康 / 模型列表
    if (req.method === 'GET' && (url.pathname === '/v1/models' || url.pathname === '/health')) {
      res.writeHead(200, { 'Content-Type': 'application/json' });
      res.end(
        JSON.stringify({
          object: 'list',
          data: router.health().map((p) => ({ id: p.model, object: 'model', owned_by: p.name })),
          providers: router.health(),
        }),
      );
      return;
    }

    // OpenAI 兼容流式补全
    if (req.method === 'POST' && url.pathname === '/v1/chat/completions') {
      let body;
      try {
        body = await readBody(req);
      } catch (err) {
        res.writeHead(400, { 'Content-Type': 'application/json' });
        res.end(JSON.stringify({ error: { message: `invalid json: ${err.message}` } }));
        return;
      }

      const { messages, tools, temperature, model } = body;
      if (!Array.isArray(messages) || messages.length === 0) {
        res.writeHead(400, { 'Content-Type': 'application/json' });
        res.end(JSON.stringify({ error: { message: 'messages is required' } }));
        return;
      }

      const stream = body.stream === true;
      if (!stream) {
        // 非流式：聚合所有 chunk。
        const parts = [];
        const calls = [];
        let usage = TokenUsage.zero();
        let reason = 'stop';
        try {
          for await (const chunk of router.chatStream({ messages, tools, temperature, model })) {
            if (chunk.type === 'text') parts.push(chunk.text);
            else if (chunk.type === 'tool_call') calls.push(chunk.call);
            else if (chunk.type === 'done') {
              usage = chunk.usage ?? usage;
              reason = chunk.reason ?? reason;
            }
          }
        } catch (err) {
          res.writeHead(500, { 'Content-Type': 'application/json' });
          res.end(JSON.stringify({ error: { message: err.message } }));
          return;
        }
        res.writeHead(200, { 'Content-Type': 'application/json' });
        res.end(
          JSON.stringify({
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
          }),
        );
        return;
      }

      // 流式
      sse(res);
      let index = 0;
      try {
        for await (const chunk of router.chatStream({ messages, tools, temperature, model })) {
          const delta = toOpenAiDelta(chunk, index);
          if (delta) writeData(res, delta);
          if (chunk.type === 'done' || chunk.type === 'error') break;
        }
      } catch (err) {
        writeData(res, { error: { message: err.message } });
      }
      res.write('data: [DONE]\n\n');
      res.end();
      return;
    }

    res.writeHead(404, { 'Content-Type': 'application/json' });
    res.end(JSON.stringify({ error: { message: 'not found' } }));
  });
}
