// 路由故障切换语义测试。
//
// 重点验证重写后的核心正确性：
// - 首字节之前失败：继续切换下一个 provider；
// - 首字节之后失败：绝不切换（否则调用方会收到重复拼接的输出）；
// - 外部 signal 取消：不算 provider 故障，直接结束；
// - 成功后 token 用量累计到对应 provider。

import test from 'node:test';
import assert from 'node:assert/strict';

import { ModelRouter } from '../src/llm/router.js';
import { TokenUsage } from '../src/llm/usage.js';

function providerConfigs() {
  return [
    { id: 'a', name: 'A', protocol: 'ollama', baseUrl: 'http://a', model: 'm' },
    { id: 'b', name: 'B', protocol: 'ollama', baseUrl: 'http://b', model: 'm' },
  ];
}

function staticBackend(chunks) {
  return {
    async *stream() {
      for (const c of chunks) yield c;
    },
  };
}

function textThenErrorBackend(text, error) {
  return {
    async *stream() {
      yield { type: 'text', text };
      throw new Error(error);
    },
  };
}

function failBackend(error) {
  return {
    async *stream() {
      throw new Error(error);
    },
  };
}

async function collect(router, req) {
  const chunks = [];
  for await (const c of router.chatStream(req)) chunks.push(c);
  return chunks;
}

test('首字节之前失败：切换到下一个 provider', async () => {
  const router = new ModelRouter(providerConfigs());
  router.providers[0].backend = failBackend('a down');
  router.providers[1].backend = staticBackend([
    { type: 'text', text: 'from-b' },
    { type: 'done', reason: 'stop', usage: new TokenUsage({ completionTokens: 2 }) },
  ]);

  const chunks = await collect(router, { messages: [{ role: 'user', content: 'hi' }] });

  assert.ok(chunks.some((c) => c.type === 'text' && c.text === 'from-b'));
  assert.equal(router.providers[0].consecutiveFailures, 1);
  assert.equal(router.providers[1].totalCalls, 1);
  assert.equal(router.providers[1].totalTokens.completionTokens, 2);
});

test('首字节之后失败：不再切换，只报一次错误，绝不重复输出', async () => {
  const router = new ModelRouter(providerConfigs());
  router.providers[0].backend = textThenErrorBackend('from-a', 'stream reset');
  router.providers[1].backend = staticBackend([
    { type: 'text', text: 'from-b' },
    { type: 'done', reason: 'stop', usage: TokenUsage.zero() },
  ]);

  const chunks = await collect(router, { messages: [{ role: 'user', content: 'hi' }] });

  assert.ok(chunks.some((c) => c.type === 'text' && c.text === 'from-a'));
  assert.ok(!chunks.some((c) => c.type === 'text' && c.text === 'from-b'), '不能出现 B 的输出');
  assert.equal(chunks.filter((c) => c.type === 'error').length, 1);
  assert.match(chunks.at(-1).message, /provider a/);
  // B 从未被调用过。
  assert.equal(router.providers[1].totalCalls, 0);
  // A 记录了一次失败。
  assert.equal(router.providers[0].consecutiveFailures, 1);
});

test('全部 provider 在首字节前失败：返回汇总 error chunk', async () => {
  const router = new ModelRouter(providerConfigs());
  router.providers[0].backend = failBackend('a down');
  router.providers[1].backend = failBackend('b down');

  const chunks = await collect(router, { messages: [{ role: 'user', content: 'hi' }] });

  assert.equal(chunks.length, 1);
  assert.equal(chunks[0].type, 'error');
  assert.match(chunks[0].message, /all 2 providers failed/);
  assert.equal(router.providers[0].consecutiveFailures, 1);
  assert.equal(router.providers[1].consecutiveFailures, 1);
});

test('请求被外部 signal 取消：直接结束，不切换 provider', async () => {
  const router = new ModelRouter(providerConfigs());
  router.providers[0].backend = failBackend('a down');
  router.providers[1].backend = staticBackend([
    { type: 'text', text: 'from-b' },
    { type: 'done', reason: 'stop', usage: TokenUsage.zero() },
  ]);

  const controller = new AbortController();
  controller.abort();

  const chunks = await collect(router, {
    messages: [{ role: 'user', content: 'hi' }],
    signal: controller.signal,
  });

  assert.equal(chunks.length, 1);
  assert.equal(chunks[0].type, 'error');
  assert.match(chunks[0].message, /取消/);
  assert.equal(router.providers[1].totalCalls, 0);
});

test('没有任何 provider：返回 no model provider configured', async () => {
  const router = new ModelRouter([]);
  const chunks = await collect(router, { messages: [{ role: 'user', content: 'hi' }] });
  assert.equal(chunks[0].type, 'error');
  assert.match(chunks[0].message, /no model provider configured/);
});

test('指定 model 优先匹配对应 provider', async () => {
  const router = new ModelRouter([
    { id: 'a', name: 'A', protocol: 'ollama', baseUrl: 'http://a', model: 'm1' },
    { id: 'b', name: 'B', protocol: 'ollama', baseUrl: 'http://b', model: 'm2' },
  ]);
  router.providers[1].backend = staticBackend([
    { type: 'text', text: 'ok' },
    { type: 'done', reason: 'stop', usage: TokenUsage.zero() },
  ]);

  const chunks = await collect(router, {
    model: 'm2',
    messages: [{ role: 'user', content: 'hi' }],
  });

  assert.ok(chunks.some((c) => c.type === 'text'));
  // A 排前但 model 不匹配，不应被调用；只有 B 记录了调用。
  assert.equal(router.providers[0].totalCalls, 0);
  assert.equal(router.providers[1].totalCalls, 1);
});
