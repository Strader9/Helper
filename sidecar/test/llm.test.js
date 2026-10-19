// 模型层单元测试。跑法：`npm test` 或 `node --test`。
// 不依赖网络：用注入的假后端验证 failover / health / token 累计，
// SSE 解析与用量归一化用纯函数直测。

import test from 'node:test';
import assert from 'node:assert/strict';

import { SseDecoder, normalizeUsage, TokenUsage, backendFor, OllamaBackend, OpenAiCompatBackend } from '../src/llm/backends.js';
import { ModelRouter } from '../src/llm/router.js';test('SseDecoder 解析完整行', () => {
  const d = new SseDecoder();
  const out = d.push('data: {"a":1}\n');
  assert.equal(out.length, 1);
  assert.deepEqual(out[0], { json: { a: 1 } });
});

test('SseDecoder 处理跨 chunk 的不完整行', () => {
  const d = new SseDecoder();
  const out1 = d.push('data: {"con');
  assert.equal(out1.length, 0);
  const out2 = d.push('tent":"hi"}\ndata: [DONE]\n');
  assert.equal(out2.length, 2);
  assert.deepEqual(out2[0], { json: { content: 'hi' } });
  assert.deepEqual(out2[1], { done: true });
});

test('SseDecoder 跳过心跳与坏行', () => {
  const d = new SseDecoder();
  const out = d.push(': ping\nnot-json\ndata: [DONE]\n');
  assert.equal(out.length, 1);
  assert.deepEqual(out[0], { done: true });
});

test('normalizeUsage 兼容 OpenAI 与 Ollama 两套字段', () => {
  const oai = normalizeUsage({ prompt_tokens: 100, completion_tokens: 50 });
  assert.equal(oai.promptTokens, 100);
  assert.equal(oai.completionTokens, 50);

  const olla = normalizeUsage({ prompt_eval_count: 300, eval_count: 20 });
  assert.equal(olla.promptTokens, 300);
  assert.equal(olla.completionTokens, 20);
});

test('normalizeUsage 对空输入返回零用量', () => {
  const z = normalizeUsage(null);
  assert.equal(z.isMeasured, false);
  assert.equal(z.promptTokens, 0);
});

test('TokenUsage.merge 累计', () => {
  const a = new TokenUsage({ promptTokens: 10, completionTokens: 5 });
  const b = new TokenUsage({ promptTokens: 20, completionTokens: 15 });
  a.merge(b);
  assert.equal(a.promptTokens, 30);
  assert.equal(a.completionTokens, 20);
});

test('backendFor 按 protocol 分派', () => {
  assert.ok(backendFor({ protocol: 'ollama', baseUrl: 'http://x' }) instanceof OllamaBackend);
  assert.ok(backendFor({ protocol: 'openai', baseUrl: 'http://x' }) instanceof OpenAiCompatBackend);
  // 缺省 protocol 走 openai 兼容
  assert.ok(backendFor({ baseUrl: 'http://x' }) instanceof OpenAiCompatBackend);
});

// 注入假后端：第一个失败，第二个成功，验证 failover 与 health。
function fakeBackend(chunksOrError, { fail = false } = {}) {
  return {
    async *stream() {
      if (fail) throw new Error('backend down');
      for (const c of chunksOrError) yield c;
    },
    async healthCheck() {
      return { ok: !fail, status: fail ? 500 : 200 };
    },
  };
}

test('ModelRouter 首个 provider 失败时切换到下一个', async () => {
  const router = new ModelRouter([
    { id: 'a', name: 'A', protocol: 'ollama', baseUrl: 'http://a', model: 'm' },
    { id: 'b', name: 'B', protocol: 'ollama', baseUrl: 'http://b', model: 'm' },
  ]);
  router.providers[0].backend = fakeBackend([], { fail: true });
  router.providers[1].backend = fakeBackend([{ type: 'text', text: 'ok' }, { type: 'done', reason: 'stop', usage: new TokenUsage({ completionTokens: 2 }) }]);

  const chunks = [];
  for await (const c of router.chatStream({ messages: [{ role: 'user', content: 'hi' }] })) {
    chunks.push(c);
  }

  assert.ok(chunks.some((c) => c.type === 'text' && c.text === 'ok'));
  assert.equal(router.providers[0].consecutiveFailures, 1);
  assert.equal(router.providers[1].consecutiveFailures, 0);
  assert.equal(router.providers[1].totalTokens.completionTokens, 2);
});

test('ModelRouter 全部失败时返回 error chunk', async () => {
  const router = new ModelRouter([
    { id: 'a', name: 'A', protocol: 'ollama', baseUrl: 'http://a', model: 'm' },
  ]);
  router.providers[0].backend = fakeBackend([], { fail: true });

  const chunks = [];
  for await (const c of router.chatStream({ messages: [{ role: 'user', content: 'hi' }] })) {
    chunks.push(c);
  }
  assert.ok(chunks.some((c) => c.type === 'error'));
});

test('ModelRouter 健康度：连续失败达阈值后不再候选', () => {
  const router = new ModelRouter([
    { id: 'a', name: 'A', protocol: 'ollama', baseUrl: 'http://a', model: 'm' },
    { id: 'b', name: 'B', protocol: 'ollama', baseUrl: 'http://b', model: 'm' },
  ]);
  // 让 a 连续失败 3 次
  for (let i = 0; i < 3; i++) {
    router.providers[0].consecutiveFailures += 1;
  }
  assert.equal(router.providers[0].healthy, false);
  // healthy 的快照不泄露 key
  const snap = router.health()[0];
  assert.equal(snap.hasApiKey, false);
  assert.equal(snap.healthy, false);
});

// OpenAiCompatBackend 工具调用聚合：mock fetch 返回分片 SSE。
test('OpenAiCompatBackend 聚合分片的 tool_calls', async () => {
  const sseLines = [
    'data: {"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"call_1","type":"function","function":{"name":"read_file","arguments":""}}]},"finish_reason":null}]}\n',
    'data: {"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"arguments":"{\\"path\\":"}}]},"finish_reason":null}]}\n',
    'data: {"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"arguments":"\\"/etc/hosts\\"}"}}]},"finish_reason":null}]}\n',
    'data: {"choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}],"usage":{"prompt_tokens":10,"completion_tokens":5}}\n',
    'data: [DONE]\n',
  ];
  const body = new ReadableStream({
    start(controller) {
      const enc = new TextEncoder();
      for (const line of sseLines) controller.enqueue(enc.encode(line));
      controller.close();
    },
  });
  globalThis.fetch = async () =>
    new Response(body, { status: 200, headers: { 'Content-Type': 'text/event-stream' } });

  const backend = new OpenAiCompatBackend({ baseUrl: 'http://x', apiKey: 'k' });
  const chunks = [];
  for await (const c of backend.stream('m', { messages: [{ role: 'user', content: 'hi' }] })) {
    chunks.push(c);
  }

  const calls = chunks.filter((c) => c.type === 'tool_call');
  assert.equal(calls.length, 1, '应聚合为一个完整 tool_call');
  assert.equal(calls[0].call.function.name, 'read_file');
  assert.equal(calls[0].call.function.arguments, '{"path":"/etc/hosts"}');

  const done = chunks.find((c) => c.type === 'done');
  assert.equal(done.usage.completionTokens, 5);
});
