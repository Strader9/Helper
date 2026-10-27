// SSE 解码器边界测试：CRLF 行尾、心跳注释、flush 兜底。

import test from 'node:test';
import assert from 'node:assert/strict';

import { SseDecoder } from '../src/llm/sse.js';

test('CRLF 行尾也能正确解析', () => {
  const d = new SseDecoder();
  const out = d.push('data: {"a":1}\r\ndata: [DONE]\r\n');
  assert.equal(out.length, 2);
  assert.deepEqual(out[0], { json: { a: 1 } });
  assert.deepEqual(out[1], { done: true });
});

test('冒号开头的注释行（心跳）被忽略', () => {
  const d = new SseDecoder();
  const out = d.push(': ping\n: keep-alive\n');
  assert.equal(out.length, 0);
});

test('不带 data: 前缀的裸 JSON 行也能解析（Ollama ndjson）', () => {
  const d = new SseDecoder();
  const out = d.push('{"message":{"content":"hi"}}\n');
  assert.equal(out.length, 1);
  assert.equal(out[0].json.message.content, 'hi');
});

test('流结束时 flush 返回最后的不完整行（若可解析）', () => {
  const d = new SseDecoder();
  assert.equal(d.push('data: {"a":2}').length, 0);
  const out = d.flush();
  assert.equal(out.length, 1);
  assert.deepEqual(out[0], { json: { a: 2 } });
  // flush 后清空，再 flush 为空
  assert.equal(d.flush().length, 0);
});
