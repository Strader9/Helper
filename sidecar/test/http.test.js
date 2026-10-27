// HTTP 面端到端测试（不依赖网络）：用假 router / 假 mcp 起一个临时端口，
// 验证 Rust 侧实际依赖的契约：
// - GET /health 同时返回 providers 与 data；
// - GET /v1/models 是 OpenAI 形态；
// - POST /v1/chat/completions 流式 / 非流式都可用，tool_calls 与 usage 正确；
// - 错误以非 2xx + {error:{message}} 返回；
// - /mcp/tools、/mcp/call 正常转发。

import test from 'node:test';
import assert from 'node:assert/strict';
import { after, beforeEach } from 'node:test';

import { createHttpApp } from '../src/http/app.js';
import { TokenUsage } from '../src/llm/usage.js';

function fakeRouter(chunks) {
  const providers = [
    {
      id: 'a',
      name: 'A',
      model: 'm1',
      protocol: 'openai',
      baseUrl: 'http://a',
      hasApiKey: false,
      healthy: true,
      consecutiveFailures: 0,
      lastError: null,
      lastLatencyMs: 12,
      totalCalls: 1,
      usage: { promptTokens: 0, completionTokens: 0 },
    },
  ];
  return {
    count: 1,
    defaultModel: 'm1',
    health: () => providers,
    async *chatStream() {
      for (const c of chunks) yield c;
    },
  };
}

function fakeMcp() {
  return {
    status: () => [
      { id: 's', name: 'S', transport: 'stdio', status: 'connected', lastError: null },
    ],
    async listAllTools() {
      return {
        servers: [{ id: 's', status: 'connected', tools: [{ name: 'echo', description: '回显' }] }],
        tools: [{ server: 's', name: 'echo', description: '回显', inputSchema: {} }],
      };
    },
    async callTool({ name, arguments: args }) {
      return {
        server: 's',
        tool: name,
        isError: false,
        content: [{ type: 'text', text: JSON.stringify(args ?? {}) }],
      };
    },
    async installServer(config) {
      return {
        server: { id: config.id, name: config.name, transport: 'http', status: 'connected' },
        tools: [{ name: 'created', description: '新工具' }],
      };
    },
    async removeServer(id) {
      return id === 's';
    },
  };
}

let server;
let base;

beforeEach(async () => {
  // 每个用例独立服务，避免上一个用例的 chunk 配置串到下一个。
  if (server) await new Promise((resolve) => server.close(resolve));
  server = null;
  base = null;
});

function startServer(router, mcp) {
  server = createHttpApp({ router, mcp });
  return new Promise((resolve) => {
    server.listen(0, '127.0.0.1', () => {
      const { port } = server.address();
      base = `http://127.0.0.1:${port}`;
      resolve();
    });
  });
}

after(async () => {
  if (server) await new Promise((r) => server.close(r));
});

function completionChunks(overrides = {}) {
  return [
    { type: 'text', text: '你好' },
    { type: 'done', reason: 'stop', usage: new TokenUsage({ promptTokens: 9, completionTokens: 3 }) },
    ...(overrides.extra ?? []),
  ];
}

test('GET /health 返回 providers 与 data（Rust 读这两个字段）', async () => {
  await startServer(fakeRouter([]), fakeMcp());
  const resp = await fetch(`${base}/health`);
  assert.equal(resp.status, 200);
  const body = await resp.json();
  assert.equal(body.status, 'ok');
  assert.equal(body.providers.length, 1);
  assert.equal(body.providers[0].id, 'a');
  assert.equal(body.data.length, 1);
  assert.equal(body.data[0].id, 'm1');
  assert.equal(body.mcp.length, 1);
});

test('GET /v1/models 是 OpenAI 列表形态', async () => {
  await startServer(fakeRouter([]), fakeMcp());
  const resp = await fetch(`${base}/v1/models`);
  assert.equal(resp.status, 200);
  const body = await resp.json();
  assert.equal(body.object, 'list');
  assert.equal(body.data[0].id, 'm1');
  assert.equal(body.data[0].object, 'model');
});

test('POST /v1/chat/completions 非流式聚合内容与 usage', async () => {
  await startServer(fakeRouter(completionChunks()), fakeMcp());
  const resp = await fetch(`${base}/v1/chat/completions`, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ messages: [{ role: 'user', content: 'hi' }], stream: false }),
  });
  assert.equal(resp.status, 200);
  const body = await resp.json();
  assert.equal(body.object, 'chat.completion');
  assert.equal(body.choices[0].message.role, 'assistant');
  assert.equal(body.choices[0].message.content, '你好');
  assert.equal(body.choices[0].finish_reason, 'stop');
  assert.equal(body.usage.completion_tokens, 3);
  assert.equal(body.usage.total_tokens, 12);
});

test('非流式 tool_calls 透传到 message', async () => {
  const router = fakeRouter([
    {
      type: 'tool_call',
      call: {
        id: 'call_1',
        type: 'function',
        function: { name: 'echo', arguments: '{"text":"hi"}' },
      },
    },
    { type: 'done', reason: 'tool_calls', usage: TokenUsage.zero() },
  ]);
  await startServer(router, fakeMcp());
  const resp = await fetch(`${base}/v1/chat/completions`, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ messages: [{ role: 'user', content: 'x' }] }),
  });
  const body = await resp.json();
  assert.equal(body.choices[0].message.tool_calls.length, 1);
  assert.equal(body.choices[0].message.tool_calls[0].function.name, 'echo');
  assert.equal(body.choices[0].finish_reason, 'tool_calls');
});

test('流式 SSE：内容增量 + finish_reason + [DONE]', async () => {
  await startServer(fakeRouter(completionChunks()), fakeMcp());
  const resp = await fetch(`${base}/v1/chat/completions`, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ messages: [{ role: 'user', content: 'hi' }], stream: true }),
  });
  assert.equal(resp.status, 200);
  assert.match(resp.headers.get('content-type'), /text\/event-stream/);
  const text = await resp.text();
  assert.match(text, /你好/);
  assert.match(text, /finish_reason":"stop/);
  assert.match(text, /completion_tokens":3/);
  assert.match(text, /data: \[DONE\]/);
});

test('messages 缺失返回 400', async () => {
  await startServer(fakeRouter([]), fakeMcp());
  const resp = await fetch(`${base}/v1/chat/completions`, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({}),
  });
  assert.equal(resp.status, 400);
  const body = await resp.json();
  assert.match(body.error.message, /messages/);
});

test('非法 JSON 返回 400', async () => {
  await startServer(fakeRouter([]), fakeMcp());
  const resp = await fetch(`${base}/v1/chat/completions`, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: '{not json',
  });
  assert.equal(resp.status, 400);
});

test('router 返回 error chunk 时非流式返回 502', async () => {
  await startServer(fakeRouter([{ type: 'error', message: 'all providers failed' }]), fakeMcp());
  const resp = await fetch(`${base}/v1/chat/completions`, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ messages: [{ role: 'user', content: 'hi' }] }),
  });
  assert.equal(resp.status, 502);
  const body = await resp.json();
  assert.match(body.error.message, /all providers failed/);
});

test('GET /mcp/tools 转发工具池', async () => {
  await startServer(fakeRouter([]), fakeMcp());
  const resp = await fetch(`${base}/mcp/tools`);
  assert.equal(resp.status, 200);
  const body = await resp.json();
  assert.equal(body.tools.length, 1);
  assert.equal(body.tools[0].name, 'echo');
  assert.equal(body.servers[0].id, 's');
});

test('POST /mcp/call 转发调用', async () => {
  await startServer(fakeRouter([]), fakeMcp());
  const resp = await fetch(`${base}/mcp/call`, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ name: 'echo', arguments: { text: 'hi' } }),
  });
  assert.equal(resp.status, 200);
  const body = await resp.json();
  assert.equal(body.isError, false);
  assert.equal(body.server, 's');
  assert.match(body.content[0].text, /hi/);
});

test('POST /mcp/call 缺 name 返回 400', async () => {
  await startServer(fakeRouter([]), fakeMcp());
  const resp = await fetch(`${base}/mcp/call`, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({}),
  });
  assert.equal(resp.status, 400);
});

test('POST /mcp/servers 安装并返回发现的工具', async () => {
  await startServer(fakeRouter([]), fakeMcp());
  const resp = await fetch(`${base}/mcp/servers`, {
    method: 'POST',
    headers: {
      'Content-Type': 'application/json',
      'X-E-Assisant-Local': 'capability-manager-v1',
    },
    body: JSON.stringify({ id: 'new-source', name: 'New Source', url: 'https://example.com/mcp' }),
  });
  assert.equal(resp.status, 201);
  const body = await resp.json();
  assert.equal(body.server.id, 'new-source');
  assert.equal(body.tools[0].name, 'created');
});

test('MCP 工具源配置拒绝缺少桌面端控制头的请求', async () => {
  await startServer(fakeRouter([]), fakeMcp());
  const resp = await fetch(`${base}/mcp/servers`, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ id: 'web-injected', url: 'https://example.com/mcp' }),
  });
  assert.equal(resp.status, 403);
});

test('DELETE /mcp/servers/:id 移除工具源', async () => {
  await startServer(fakeRouter([]), fakeMcp());
  const resp = await fetch(`${base}/mcp/servers/s`, {
    method: 'DELETE',
    headers: { 'X-E-Assisant-Local': 'capability-manager-v1' },
  });
  assert.equal(resp.status, 200);
  assert.deepEqual(await resp.json(), { removed: true, id: 's' });
});

test('未知路由返回 404', async () => {
  await startServer(fakeRouter([]), fakeMcp());
  const resp = await fetch(`${base}/nope`);
  assert.equal(resp.status, 404);
});
