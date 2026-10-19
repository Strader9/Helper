// MCP stdio 测试服务器：注册 echo / add 两个工具，跑在 stdin/stdout 上。
// 供 test/mcp.test.js 以子进程方式 spawn，验证 sidecar 的 MCP 客户端转发链路。
import { Server } from '@modelcontextprotocol/sdk/server/index.js';
import { StdioServerTransport } from '@modelcontextprotocol/sdk/server/stdio.js';
import {
  ListToolsRequestSchema,
  CallToolRequestSchema,
} from '@modelcontextprotocol/sdk/types.js';

const server = new Server(
  { name: 'echo-fixture', version: '1.0.0' },
  { capabilities: { tools: {} } },
);

server.setRequestHandler(ListToolsRequestSchema, async () => ({
  tools: [
    {
      name: 'echo',
      description: '回显输入的文本',
      inputSchema: { type: 'object', properties: { text: { type: 'string' } } },
    },
    {
      name: 'add',
      description: '两个数相加',
      inputSchema: {
        type: 'object',
        properties: { a: { type: 'number' }, b: { type: 'number' } },
      },
    },
  ],
}));

server.setRequestHandler(CallToolRequestSchema, async (request) => {
  const { name, arguments: args } = request.params;
  if (name === 'echo') {
    return { content: [{ type: 'text', text: String(args?.text ?? '') }] };
  }
  if (name === 'add') {
    const sum = (args?.a ?? 0) + (args?.b ?? 0);
    return { content: [{ type: 'text', text: String(sum) }] };
  }
  return { content: [{ type: 'text', text: `unknown tool: ${name}` }], isError: true };
});

const transport = new StdioServerTransport();
await server.connect(transport);
