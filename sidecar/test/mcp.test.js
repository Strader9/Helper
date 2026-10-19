// MCP 客户端单元测试。跑法：`npm test` 或 `node --test`。
//
// 用本地 stdio fixture（fixtures/echo-mcp-server.mjs）做真实子进程连接，
// 验证 sidecar 的 MCP 客户端能 list / call 外部工具，且失败路径不崩。

import test from 'node:test';
import assert from 'node:assert/strict';
import { fileURLToPath } from 'node:url';
import { dirname, join } from 'node:path';

import { McpManager } from '../src/mcp/index.js';

const __dirname = dirname(fileURLToPath(import.meta.url));
const FIXTURE = join(__dirname, '..', 'fixtures', 'echo-mcp-server.mjs');
const NODE = process.execPath;

function stdioServerConfig(extra = {}) {
  return {
    id: 'echo',
    name: 'Echo Fixture',
    transport: 'stdio',
    command: NODE,
    args: [FIXTURE],
    ...extra,
  };
}

test('listAllTools 拉取 stdio server 的工具并带 server 归属', async () => {
  const mcp = new McpManager([stdioServerConfig()]);
  const { servers, tools } = await mcp.listAllTools();

  assert.equal(servers.length, 1);
  assert.equal(servers[0].id, 'echo');
  assert.equal(servers[0].status, 'connected');

  const names = tools.map((t) => t.name).sort();
  assert.deepEqual(names, ['add', 'echo']);
  assert.ok(tools.every((t) => t.server === 'echo'));

  await mcp.servers[0].disconnect();
});

test('callTool 调用 add 返回正确结果', async () => {
  const mcp = new McpManager([stdioServerConfig()]);
  const result = await mcp.callTool({ name: 'add', arguments: { a: 2, b: 3 } });

  assert.equal(result.isError, false);
  assert.equal(result.server, 'echo');
  assert.equal(result.content[0].text, '5');

  await mcp.servers[0].disconnect();
});

test('callTool 调用 echo 回显文本', async () => {
  const mcp = new McpManager([stdioServerConfig()]);
  const result = await mcp.callTool({ name: 'echo', arguments: { text: '你好' } });

  assert.equal(result.isError, false);
  assert.equal(result.content[0].text, '你好');

  await mcp.servers[0].disconnect();
});

test('callTool 调用不存在的工具返回 isError', async () => {
  const mcp = new McpManager([stdioServerConfig()]);
  const result = await mcp.callTool({ name: 'nope' });

  assert.equal(result.isError, true);
  assert.match(result.error, /没有任何已连接的 MCP server/);

  await mcp.servers[0].disconnect();
});

test('同名工具多 server 时要求显式指定 server', async () => {
  const mcp = new McpManager([
    stdioServerConfig({ id: 'a' }),
    stdioServerConfig({ id: 'b' }),
  ]);
  const result = await mcp.callTool({ name: 'add', arguments: { a: 1, b: 2 } });

  assert.equal(result.isError, true);
  assert.match(result.error, /多个 MCP server/);

  await Promise.all(mcp.servers.map((s) => s.disconnect()));
});

test('显式指定 server 可消歧同名工具', async () => {
  const mcp = new McpManager([
    stdioServerConfig({ id: 'a' }),
    stdioServerConfig({ id: 'b' }),
  ]);
  const result = await mcp.callTool({ name: 'add', arguments: { a: 4, b: 4 }, server: 'b' });

  assert.equal(result.isError, false);
  assert.equal(result.server, 'b');
  assert.equal(result.content[0].text, '8');

  await Promise.all(mcp.servers.map((s) => s.disconnect()));
});
