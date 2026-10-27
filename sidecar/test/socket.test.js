// Socket.IO 面端到端测试：用 createSidecarServer 组装完整服务（假 router，不触网），
// 验证 sidecar:hello / providers:health / chat:start → chat:chunk → end 事件契约。

import test from 'node:test';
import assert from 'node:assert/strict';
import { io } from 'socket.io-client';

import { createSidecarServer } from '../src/app.js';
import { TokenUsage } from '../src/llm/usage.js';

const sidecar = createSidecarServer({
  providers: [
    { id: 'a', name: 'A', protocol: 'ollama', baseUrl: 'http://127.0.0.1:1', model: 'm1' },
  ],
  mcpServers: [],
});

test.before(async () => {
  // 替换为不触网的假对话流。
  sidecar.router.chatStream = async function* () {
    yield { type: 'text', text: 'socket-ok' };
    yield { type: 'done', reason: 'stop', usage: TokenUsage.zero() };
  };
  await new Promise((resolve) => sidecar.httpServer.listen(0, '127.0.0.1', resolve));
});

test.after(async () => {
  await sidecar.close();
});

test('连接后收到 sidecar:hello，providers:health 可 ack，chat:start 流式结束', async () => {
  const { port } = sidecar.httpServer.address();
  const socket = io(`http://127.0.0.1:${port}`, {
    transports: ['websocket'],
    forceNew: true,
  });
  // hello 在服务端 connection 回调里立即发送，必须在等待 connect 前订阅，
  // 否则本机事件循环足够快时会先收到 hello、后注册监听器并永久等待。
  const helloPromise = new Promise((resolve) => socket.once('sidecar:hello', resolve));

  await new Promise((resolve, reject) => {
    socket.on('connect', resolve);
    socket.on('connect_error', reject);
  });

  const hello = await helloPromise;
  assert.equal(hello.version, '0.1.0');
  assert.equal(hello.providers, 1);

  const health = await new Promise((resolve) =>
    socket.emit('providers:health', resolve),
  );
  assert.equal(health.providers.length, 1);
  assert.equal(health.providers[0].id, 'a');

  const chunks = [];
  const finished = new Promise((resolve) => {
    socket.on('chat:chunk', (chunk) => {
      chunks.push(chunk);
      if (chunk.type === 'end') resolve();
    });
  });

  const ack = await new Promise((resolve) =>
    socket.emit(
      'chat:start',
      { messages: [{ role: 'user', content: 'hi' }] },
      resolve,
    ),
  );
  assert.deepEqual(ack, { ok: true });

  await finished;
  assert.ok(chunks.some((c) => c.type === 'text' && c.text === 'socket-ok'));
  assert.ok(chunks.some((c) => c.type === 'done'));
  assert.equal(chunks.at(-1).type, 'end');

  socket.close();
});

test('chat:start 缺少 messages 时 ack 失败', async () => {
  const { port } = sidecar.httpServer.address();
  const socket = io(`http://127.0.0.1:${port}`, {
    transports: ['websocket'],
    forceNew: true,
  });
  await new Promise((resolve) => socket.on('connect', resolve));

  const ack = await new Promise((resolve) =>
    socket.emit('chat:start', {}, resolve),
  );
  assert.equal(ack.ok, false);
  assert.match(ack.error, /messages/);

  socket.close();
});
